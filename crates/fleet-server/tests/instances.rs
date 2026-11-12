//! Several Server instances on one PostgreSQL database, each through its real listeners
//! (ADR-0025):
//! both hold a lease and serve, a revocation committed on one ends the session it concerns on the
//! other, and an instance whose database stops answering closes its listeners until it holds a new
//! lease.
//!
//! They run against the database the `FLEET_TEST_POSTGRES_*` variables name, in a schema of their
//! own, over TLS against `FLEET_TEST_POSTGRES_CA_FILE` where it is named and over plaintext to the
//! loopback where it is not, and skip, saying so, where no database is named.
//! `scripts/check-store.sh postgres` runs them, and fails on a skip.

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use fleet_server::config::{LeaseTimes, PostgresConfig};
use fleet_server::enrolment::Enrolment;
use fleet_server::floors::PackageFloors;
use fleet_server::postgres::schema::{Migration, MIGRATIONS};
use fleet_server::postgres::{Database, Exclusive};
use fleet_server::revocation::{CertId, Revocations};
use fleet_server::Records;
use opamp::proto::{AgentCapabilities, AgentToServer};
use opamp::uid::InstanceUid;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};

#[path = "support/token.rs"]
mod token;

/// A lease short enough for a test to watch it lapse.
const TIMES: LeaseTimes = LeaseTimes {
    renew: Duration::from_secs(1),
    lapse: Duration::from_secs(3),
};

/// The database the variables name, in a schema of this test's own that is dropped with it.
struct Schema {
    config: PostgresConfig,
    name: String,
    /// Released once the schema is dropped, after this test's instances closed their connections.
    _slot: Slot,
}

/// How many tests work in a schema at once. A pool opens a connection only for a call in flight
/// and keeps it, up to its `pool_size` (16 by default), so a test holds as many as it makes calls
/// at once, beside a listening connection and a lease's session per instance it serves: about
/// eight for most tests, up to the 24 calls at once of
/// `many_creations_at_once_on_many_instances_never_pass_the_ceiling`. The test database, like
/// CI's, admits 100 (three of them reserved), so the tests the harness would otherwise run at
/// once, one per core, can exhaust it.
const SCHEMAS_AT_ONCE: usize = 8;

/// One of the [`SCHEMAS_AT_ONCE`] places a test works in, held until it is dropped.
struct Slot;

/// The places taken, and the wake-up of a test that waits for one.
static SLOTS: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// How long a test waits for a place before it fails: far past the longest a test of this file
/// holds one, so only a wait that cannot end reaches it.
const SLOT_WAIT: Duration = Duration::from_secs(300);

impl Slot {
    /// Waits until fewer than [`SCHEMAS_AT_ONCE`] tests work in a schema, and takes a place. A
    /// thread a test spawned, which the harness does not name, is refused at once: it would wait
    /// for a place beside the one its test holds, for ever once every place is taken.
    fn take() -> Slot {
        assert!(
            std::thread::current().name().is_some(),
            "a test schema is opened on a thread the test spawned: it takes one of the \
             SCHEMAS_AT_ONCE ({SCHEMAS_AT_ONCE}) places beside any its test holds and can \
             wait for it for ever — open it on the test's own thread"
        );
        let (taken, freed) = &SLOTS;
        let (mut taken, waited) = freed
            .wait_timeout_while(
                taken
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                SLOT_WAIT,
                |taken| *taken >= SCHEMAS_AT_ONCE,
            )
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if waited.timed_out() {
            // Released first, so the failure poisons no lock the next test takes.
            drop(taken);
            panic!(
            "no place among the SCHEMAS_AT_ONCE ({SCHEMAS_AT_ONCE}) test schemas came free within \
             {SLOT_WAIT:?}"
            );
        }
        *taken += 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let (taken, freed) = &SLOTS;
        *taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) -= 1;
        freed.notify_one();
    }
}

/// What a session of a test's own connects with: the database's settings and, over TLS, its roots.
struct Session {
    pg: tokio_postgres::Config,
    roots: Option<rustls::RootCertStore>,
}

impl Session {
    async fn open(self) -> tokio_postgres::Client {
        match self.roots {
            Some(roots) => {
                opamp::tls::install_ring_provider();
                let tls = tokio_postgres_rustls::MakeRustlsConnect::new(
                    opamp::tls::client_builder()
                        .with_root_certificates(roots)
                        .with_no_client_auth(),
                );
                let (client, connection) = self.pg.connect(tls).await.expect("db");
                tokio::spawn(connection);
                client
            }
            None => {
                let (client, connection) =
                    self.pg.connect(tokio_postgres::NoTls).await.expect("db");
                tokio::spawn(connection);
                client
            }
        }
    }
}

/// A transaction [`Schema::hold`] holds open, rolled back once this is dropped.
struct Held {
    release: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Held {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Schema {
    fn named() -> Option<Self> {
        let variable = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let (Some(host), Some(dbname), Some(user), Some(password_file)) = (
            variable("FLEET_TEST_POSTGRES_HOST"),
            variable("FLEET_TEST_POSTGRES_DBNAME"),
            variable("FLEET_TEST_POSTGRES_USER"),
            variable("FLEET_TEST_POSTGRES_PASSWORD_FILE"),
        ) else {
            eprintln!(
                "skipped: no PostgreSQL is named — set FLEET_TEST_POSTGRES_HOST, \
                 FLEET_TEST_POSTGRES_PORT, FLEET_TEST_POSTGRES_DBNAME, FLEET_TEST_POSTGRES_USER and \
                 FLEET_TEST_POSTGRES_PASSWORD_FILE to run the tests of several instances"
            );
            return None;
        };
        let ca_file: Option<std::path::PathBuf> =
            variable("FLEET_TEST_POSTGRES_CA_FILE").map(Into::into);
        let config = PostgresConfig {
            host,
            port: variable("FLEET_TEST_POSTGRES_PORT")
                .map_or(5432, |port| port.parse().expect("FLEET_TEST_POSTGRES_PORT")),
            dbname,
            user,
            password_file: password_file.into(),
            tls: ca_file.is_some(),
            pool_size: fleet_server::config::DEFAULT_POOL_SIZE,
            ca_file,
        };
        let mut random = [0u8; 8];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
            .expect("random");
        let schema = Schema {
            config,
            name: format!("fleet_test_{}", hex::encode(random)),
            _slot: Slot::take(),
        };
        schema.sql(&format!("CREATE SCHEMA {}", schema.name));
        Some(schema)
    }

    /// The settings, through `port` rather than the database's own.
    fn config_on(&self, port: u16) -> PostgresConfig {
        PostgresConfig {
            host: self.config.host.clone(),
            port,
            dbname: self.config.dbname.clone(),
            user: self.config.user.clone(),
            password_file: self.config.password_file.clone(),
            ca_file: self.config.ca_file.clone(),
            tls: self.config.tls,
            pool_size: self.config.pool_size,
        }
    }

    /// Runs `statements`, past every adapter.
    fn sql(&self, statements: &str) {
        self.query(statements);
    }

    /// The first column of every row `query` answers in this schema, as text.
    fn query(&self, query: &str) -> Vec<String> {
        let query = format!("SET search_path = {};", self.name) + query;
        let session = self.session();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async move {
                    session
                        .open()
                        .await
                        .simple_query(&query)
                        .await
                        .expect("query")
                        .into_iter()
                        .filter_map(|message| match message {
                            tokio_postgres::SimpleQueryMessage::Row(row) => {
                                row.get(0).map(str::to_string)
                            }
                            _ => None,
                        })
                        .collect()
                })
        })
        .join()
        .expect("the query's thread")
    }

    /// Runs `statements` in a transaction on a session of its own, in this schema, and holds the
    /// transaction open, with the locks it took, until the [`Held`] is dropped; then rolls it back.
    fn hold(&self, statements: &str) -> Held {
        let statements = format!("SET search_path = {};\nBEGIN;\n{statements}", self.name);
        let session = self.session();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (taken, takes) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async move {
                    let client = session.open().await;
                    client.batch_execute(&statements).await.expect("the hold");
                    let _ = taken.send(());
                    let _ = tokio::task::spawn_blocking(move || released.recv()).await;
                    client
                        .batch_execute("ROLLBACK")
                        .await
                        .expect("the rollback");
                });
        });
        takes.recv().expect("the hold is held");
        Held {
            release: Some(release),
            thread: Some(thread),
        }
    }

    /// What a session of this test's own, past every adapter, connects with.
    fn session(&self) -> Session {
        let password = std::fs::read_to_string(&self.config.password_file).expect("password");
        let mut pg = tokio_postgres::Config::new();
        pg.host(&self.config.host)
            .port(self.config.port)
            .dbname(&self.config.dbname)
            .user(&self.config.user)
            .password(password.trim());
        let roots = self.config.ca_file.as_ref().map(|path| {
            opamp::tls::root_store(&std::fs::read(path).expect("FLEET_TEST_POSTGRES_CA_FILE"))
                .expect("roots")
        });
        if roots.is_some() {
            pg.ssl_mode(tokio_postgres::config::SslMode::Require);
        }
        Session { pg, roots }
    }
}

impl Drop for Schema {
    /// Drops the schema while the instances' background work may still query it. One table at a
    /// time, each in a transaction of its own, so the drop never holds one table's lock while it
    /// waits for another's, which an instance's transaction reading two tables in the other order
    /// would answer with a deadlock.
    fn drop(&mut self) {
        let tables = self.query(&format!(
            "SELECT tablename FROM pg_tables WHERE schemaname = '{}'",
            self.name
        ));
        for table in tables {
            self.sql(&format!("DROP TABLE IF EXISTS {table} CASCADE"));
        }
        self.sql(&format!("DROP SCHEMA IF EXISTS {} CASCADE", self.name));
    }
}

/// A throwaway CA, the shape an operator's client CA has.
struct Pki {
    ca_pem: String,
    ca_key_pem: String,
}

impl Pki {
    fn new() -> Self {
        Self::named("opamp-fleet-test-ca")
    }

    /// A CA whose common name is `name`.
    fn named(name: &str) -> Self {
        let key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).expect("ca");
        Pki {
            ca_pem: cert.pem(),
            ca_key_pem: key.serialize_pem(),
        }
    }

    /// A certificate and key for `name`.
    fn issue(&self, name: &str) -> (String, String) {
        let ca_key = KeyPair::from_pem(&self.ca_key_pem).expect("ca key");
        let issuer = Issuer::from_ca_cert_pem(&self.ca_pem, ca_key).expect("issuer");
        let key = KeyPair::generate().expect("key");
        let cert = CertificateParams::new(vec![name.to_string()])
            .expect("params")
            .signed_by(&key, &issuer)
            .expect("signed");
        (cert.pem(), key.serialize_pem())
    }

    fn authorities(&self) -> Vec<fleet_server::revocation::Authority> {
        let der = opamp::tls::certificates(self.ca_pem.as_bytes()).expect("pem");
        let facts = fleet_server::ca::facts(der[0].as_ref()).expect("facts");
        vec![fleet_server::revocation::Authority {
            role: "client".to_string(),
            subject: facts.id.issuer,
            name: facts.issuer_name,
        }]
    }
}

/// The CAs of instances whose client CA key is on a token: that CA's certificate, its key named
/// by its label, a bootstrap CA in a file, and the token's settings
/// (ADR-0043).
struct Signing<'a> {
    token: &'a token::TestToken,
    client_ca_pem: String,
    bootstrap: Pki,
}

impl Signing<'_> {
    /// The key label of the client CA on the token.
    const CLIENT_CA: &'static str = "client-ca";
}

/// One running instance, as the binary composes it.
struct Instance {
    state: Arc<fleet_server::fleet::AppState>,
    agent: SocketAddr,
    operator: SocketAddr,
    revocations: Arc<Revocations>,
    floors: Arc<PackageFloors>,
    enrolment: Arc<Enrolment>,
    following: fleet_server::instance::Following,
    /// The admission throttle of the Agent plane, where it was started with one.
    throttle: Option<Arc<fleet_server::throttle::Throttle>>,
    database: Arc<Database>,
    held: fleet_server::instance::Held,
    /// The time, in nanoseconds, the lease's clock counts beyond what passed: a suspend of the
    /// host, which the clock counts though the process did not run.
    suspended: Arc<AtomicU64>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    /// Every entry the instance's signatures with a token key wrote, where it signs with one.
    audit: Option<Arc<token::Recorded>>,
    /// The database the instance listens through, where it is not [`database`](Self::database).
    listening: Option<Arc<Database>>,
    _dir: tempfile::TempDir,
}

impl Instance {
    /// An instance on `config`, in the test's schema, sharing the database.
    async fn start(pki: &Pki, schema: &Schema, config: PostgresConfig) -> Self {
        Self::start_following(pki, schema, config, true).await
    }

    /// [`start`](Self::start), taking up nothing another instance commits until
    /// [`follow`](Self::follow) is called: what it hears is held back until then.
    async fn start_deaf(pki: &Pki, schema: &Schema, config: PostgresConfig) -> Self {
        Self::start_with(pki, schema, config, true, false, None, None, None, None).await
    }

    /// [`start`](Self::start), its client CA's key on the token `signing` names, opened by this
    /// instance as the Server opens it, and enrolment with `signing`'s bootstrap CA.
    async fn start_signing(
        pki: &Pki,
        schema: &Schema,
        config: PostgresConfig,
        signing: &Signing<'_>,
    ) -> Self {
        Self::start_with(
            pki,
            schema,
            config,
            true,
            true,
            None,
            None,
            None,
            Some(signing),
        )
        .await
    }

    /// [`start`](Self::start), its Agent plane's admission counted by a throttle of `limits`, as
    /// the binary wires one.
    async fn start_throttled(
        pki: &Pki,
        schema: &Schema,
        config: PostgresConfig,
        limits: fleet_server::throttle::Limits,
    ) -> Self {
        Self::start_with(
            pki,
            schema,
            config,
            true,
            true,
            Some(limits),
            None,
            None,
            None,
        )
        .await
    }

    /// Starts taking up what other instances commit, for an instance started deaf.
    fn follow(&mut self) {
        self.following = fleet_server::instance::follow(&self.database, self.state.clone(), None);
    }

    /// [`follow`](Self::follow), listening through a connection of its own on `config`: what
    /// happens to that connection, such as a cut, leaves the lease and every other connection of
    /// the instance as they are. Unlike the Server, the listener is not the database the instance
    /// writes through: it hears the instance's own commits as another's, and not the notices of
    /// commits that database could not confirm. Without the renewals' comparison.
    async fn follow_through(&mut self, schema: &Schema, config: PostgresConfig) {
        let name = schema.name.clone();
        let listening = tokio::task::spawn_blocking(move || {
            Database::open_in(&config, Exclusive::Shared, Some(&name))
        })
        .await
        .expect("open")
        .expect("open the listening connection's database");
        self.following = fleet_server::instance::follow(&listening, self.state.clone(), None);
        self.listening = Some(listening);
    }

    /// [`start`](Self::start), its lease renewals compared with the list's version only where
    /// `renewals` says so: a test of what a notification, or the connection made again, does
    /// alone turns the comparison off.
    async fn start_following(
        pki: &Pki,
        schema: &Schema,
        config: PostgresConfig,
        renewals: bool,
    ) -> Self {
        Self::start_with(pki, schema, config, renewals, true, None, None, None, None).await
    }

    /// [`start`](Self::start), its package blobs on the filesystem in `blobs`, which other
    /// instances may share, rather than in a directory of its own.
    async fn start_on_blobs(
        pki: &Pki,
        schema: &Schema,
        config: PostgresConfig,
        blobs: &std::path::Path,
    ) -> Self {
        Self::start_with(
            pki,
            schema,
            config,
            true,
            true,
            None,
            Some(blobs),
            None,
            None,
        )
        .await
    }

    /// [`start`](Self::start), as a Server of `release` whose schema is `migrations`: one of two
    /// versions side by side.
    async fn start_as(
        pki: &Pki,
        schema: &Schema,
        config: PostgresConfig,
        release: &'static str,
        migrations: &'static [Migration],
    ) -> Self {
        Self::start_with(
            pki,
            schema,
            config,
            true,
            true,
            None,
            None,
            Some((release, migrations)),
            None,
        )
        .await
    }

    /// [`start_following`](Self::start_following), following other instances where `follows`
    /// says so, its admission throttled where `throttle` names limits, its package blobs in
    /// `blobs` where it names a directory, as the release and schema `version` names, or this
    /// build's, and with its client CA's key on a token where `signing` names one.
    #[allow(clippy::too_many_arguments)]
    async fn start_with(
        pki: &Pki,
        schema: &Schema,
        config: PostgresConfig,
        renewals: bool,
        follows: bool,
        throttle: Option<fleet_server::throttle::Limits>,
        blobs: Option<&std::path::Path>,
        version: Option<(&'static str, &'static [Migration])>,
        signing: Option<&Signing<'_>>,
    ) -> Self {
        opamp::tls::install_ring_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let name = schema.name.clone();
        let database = tokio::task::spawn_blocking(move || match version {
            Some((release, migrations)) => {
                Database::open_as(&config, Exclusive::Shared, Some(&name), release, migrations)
            }
            None => Database::open_in(&config, Exclusive::Shared, Some(&name)),
        })
        .await
        .expect("open")
        .expect("open the database");
        let records = Records::Postgres(database.clone());
        let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
        let revocations = Arc::new(
            Revocations::open(
                records.ledger().expect("ledger"),
                clock.clone(),
                pki.authorities(),
            )
            .expect("revocations"),
        );
        let floors = Arc::new(PackageFloors::open(records.floors()).expect("floors"));
        let enrolment =
            Arc::new(Enrolment::open_on(clock.clone(), records.enrolment()).expect("enrolment"));
        // The handshake trusts the test's CA and, where the client CA's key is on a token, that
        // CA too; with it, a bootstrap CA lets a host enrol.
        let client_ca_pem = format!(
            "{}{}",
            pki.ca_pem,
            signing.map_or("", |signing| signing.client_ca_pem.as_str())
        );
        std::fs::write(dir.path().join("ca.pem"), &client_ca_pem).expect("write");
        let enrolment_config = signing.map(|signing| {
            let file = dir.path().join("bootstrap-ca.pem");
            std::fs::write(&file, &signing.bootstrap.ca_pem).expect("write");
            fleet_server::config::EnrolmentConfig {
                bootstrap_ca_file: file,
                bootstrap_ca_key_file: None,
                bootstrap_ca_key_label: None,
                max_pending_per_bootstrap:
                    fleet_server::enrolment::DEFAULT_MAX_PENDING_PER_BOOTSTRAP,
            }
        });
        let audit = signing.map(|_| Arc::new(token::Recorded::default()));
        let client_ca = signing.map(|signing| {
            let file = dir.path().join("client-ca.pem");
            std::fs::write(&file, &signing.client_ca_pem).expect("write");
            let token = signing.token.open();
            token.set_audit(audit.clone().expect("the record"));
            // As the binary wires it: a renewal proof from any CA of client_ca_file holds.
            let tls = toml::from_str::<fleet_server::config::TlsConfig>(&format!(
                "cert_file = {:?}\nkey_file = {:?}\nclient_ca_file = {:?}\n",
                dir.path().join("server-cert.pem").display().to_string(),
                dir.path().join("server-key.pem").display().to_string(),
                dir.path().join("ca.pem").display().to_string(),
            ))
            .expect("tls config");
            fleet_server::ca::ClientCa::for_server(
                &fleet_server::config::ClientCaConfig {
                    cert_file: file,
                    key_file: None,
                    key_label: Some(Signing::CLIENT_CA.to_string()),
                    validity_days: 30,
                },
                &tls,
                Some(&token),
            )
            .expect("the client CA on the token")
        });
        let (packages, _) = fleet_server::packages::PackageStore::open_on(
            &records,
            Some(blobs.unwrap_or(dir.path())),
            &toml::from_str::<fleet_server::config::BlobsConfig>("").expect("blobs"),
            fleet_server::clock::Elapsed::monotonic(),
        )
        .expect("packages");
        let state = Arc::new(
            fleet_server::fleet::AppState::on(&records)
                .expect("state")
                .with_packages(Some(
                    fleet_server::fleet::PackageOffering::on(&records, packages)
                        .expect("deployments"),
                ))
                .with_revocations(Some(revocations.clone()))
                .with_floors(Some(floors.clone()))
                .with_enrolment(Some(enrolment.clone()))
                .with_client_ca(client_ca)
                .with_audit(
                    audit
                        .clone()
                        .map(|audit| audit as Arc<dyn fleet_server::audit::Audit>),
                ),
        );
        let (server_cert, server_key) = pki.issue("localhost");
        for (file, pem) in [
            ("server-cert.pem", &server_cert),
            ("server-key.pem", &server_key),
        ] {
            std::fs::write(dir.path().join(file), pem).expect("write");
        }
        let tls = toml::from_str::<fleet_server::config::TlsConfig>(&format!(
            "cert_file = {:?}\nkey_file = {:?}\nclient_ca_file = {:?}\n",
            dir.path().join("server-cert.pem").display().to_string(),
            dir.path().join("server-key.pem").display().to_string(),
            dir.path().join("ca.pem").display().to_string(),
        ))
        .expect("tls config");
        let planes = fleet_server::tls::server_tls(&tls, None, enrolment_config.as_ref())
            .expect("server material");
        let throttle = throttle
            .map(|limits| Arc::new(fleet_server::throttle::Throttle::new(limits, clock.clone())));
        let mut admission = fleet_server::transport::Admission::new(true)
            .with_revocations(Some(revocations.clone()));
        if signing.is_some() {
            admission = admission.with_enrolment(planes.issuers, Some(enrolment.clone()));
        }
        if let Some(throttle) = &throttle {
            admission = admission.with_throttle(throttle.clone());
        }
        let agents = fleet_server::agent_app(state.clone(), admission);
        let operators = fleet_server::operator_app(state.clone(), None);

        let suspended = Arc::new(AtomicU64::new(0));
        let lease = {
            let database = database.clone();
            let monotonic = fleet_server::clock::Elapsed::monotonic();
            let suspended = suspended.clone();
            let clock = fleet_server::clock::Elapsed::new(move || {
                monotonic.now() + Duration::from_nanos(suspended.load(Ordering::SeqCst))
            });
            fleet_server::instance::Lease::take_as(
                database,
                TIMES,
                "test-host",
                fleet_server::postgres::lease::new_instance_id().expect("id"),
                clock,
            )
            .expect("lease")
        };
        state.serve_under(lease.held());
        let proof = lease.proof();
        state.answer_while(move || proof.holds());
        let following = if follows {
            fleet_server::instance::follow(
                &database,
                state.clone(),
                renewals.then(|| lease.list_versions()),
            )
        } else {
            fleet_server::instance::Following::default()
        };
        let state_kept = state.clone();
        let agent_listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let operator_listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let agent = agent_listener.local_addr().expect("addr");
        let operator = operator_listener.local_addr().expect("addr");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let held = lease.held();
        tokio::spawn(async move {
            fleet_server::instance::serve(
                fleet_server::instance::Planes {
                    agents,
                    operators,
                    agent_tls: planes.agent.rustls_config().expect("agent plane"),
                    operator_tls: planes.operator.rustls_config().expect("operator plane"),
                    agent_max_connections: 64,
                    agent_max_new_connections_per_sec: 200,
                    operator_max_connections: 64,
                    drain: Duration::from_secs(1),
                },
                (agent_listener, operator_listener),
                Some(lease.held()),
                Some(lease.proof()),
                async move {
                    let _ = stopped.await;
                },
            )
            .await
            .expect("serve");
            lease.release().await;
        });
        Instance {
            state: state_kept,
            agent,
            operator,
            revocations,
            floors,
            enrolment,
            following,
            throttle,
            database,
            held,
            suspended,
            stop: Some(stop),
            audit,
            listening: None,
            _dir: dir,
        }
    }

    /// The instance id of the lease it holds, if it holds one.
    fn instance_id(&self) -> Option<String> {
        self.held.borrow().clone()
    }

    /// The host is suspended for `pause` and runs again: the lease's clock counts it, though the
    /// process did not run.
    fn suspend_for(&self, pause: Duration) {
        let nanos = u64::try_from(pause.as_nanos()).expect("a pause in nanoseconds");
        self.suspended.fetch_add(nanos, Ordering::SeqCst);
    }

    /// Whether the Agent plane takes a TCP connection: what a probe sees.
    fn answers_the_probe(&self) -> bool {
        Self::takes_connections(self.agent)
    }

    /// Whether both planes take a TCP connection, and whether neither does.
    fn both_planes(&self) -> (bool, bool) {
        let (agent, operator) = (
            Self::takes_connections(self.agent),
            Self::takes_connections(self.operator),
        );
        (agent && operator, !agent && !operator)
    }

    fn takes_connections(address: SocketAddr) -> bool {
        std::net::TcpStream::connect_timeout(&address, Duration::from_millis(500)).is_ok()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A WebSocket to `instance`'s Agent plane presenting `cert`.
async fn websocket(instance: &Instance, pki: &Pki, cert: &str, key: &str) -> Socket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let config = opamp::tls::client_builder()
        .with_root_certificates(opamp::tls::root_store(pki.ca_pem.as_bytes()).expect("roots"))
        .with_client_auth_cert(
            opamp::tls::certificates(cert.as_bytes()).expect("cert"),
            opamp::tls::private_key(key.as_bytes()).expect("key"),
        )
        .expect("client auth");
    let request = format!("wss://localhost:{}/v1/opamp", instance.agent.port())
        .into_client_request()
        .expect("request");
    let tcp = tokio::net::TcpStream::connect(instance.agent)
        .await
        .expect("tcp");
    tokio_tungstenite::client_async_tls_with_config(
        request,
        tcp,
        None,
        Some(tokio_tungstenite::Connector::Rustls(Arc::new(config))),
    )
    .await
    .map(|(socket, _)| socket)
    .expect("admitted")
}

/// Sends one report and waits for its reply, so the session is known to be up.
async fn exchange(socket: &mut Socket) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let report = AgentToServer {
        instance_uid: InstanceUid::default().as_bytes().to_vec(),
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    };
    let framed = opamp::frame::encode_within(&report, usize::MAX).expect("frame");
    socket.send(Ws::Binary(framed.into())).await.expect("send");
    loop {
        match tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a reply in time")
        {
            Some(Ok(Ws::Binary(_))) => return,
            Some(Ok(_)) => continue,
            other => panic!("the session ended early: {other:?}"),
        }
    }
}

/// The close frame `socket` is ended with within `within`, or `None` if it is still open then.
async fn closed_within(socket: &mut Socket, within: Duration) -> Option<(u16, String)> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as Ws;
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, socket.next()).await {
            Err(_) => return None,
            Ok(Some(Ok(Ws::Close(Some(frame))))) => {
                return Some((frame.code.into(), frame.reason.to_string()))
            }
            Ok(Some(Ok(Ws::Close(None)))) | Ok(None) => return Some((0, String::new())),
            Ok(Some(Ok(_))) => continue,
            Ok(Some(Err(e))) => return Some((0, e.to_string())),
        }
    }
}

fn cert_id(pem: &str) -> CertId {
    let der = opamp::tls::certificates(pem.as_bytes()).expect("pem");
    fleet_server::ca::facts(der[0].as_ref()).expect("facts").id
}

/// Waits up to `within` for `condition`.
async fn eventually(within: Duration, condition: impl Fn() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    condition()
}

/// Two instances on one database both hold a live lease and both answer the probe on the Agent
/// plane; a revocation committed on A ends the session of that certificate on B, by its
/// notification or at the latest by the next renewal, and no other session. The wait is bounded at
/// three renewal intervals, as for a notification missed, leaving a loaded host its delays.
/// Verifies: ADR-0025, ADR-0040
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_on_one_instance_ends_the_session_on_the_other() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;

    let (ids_a, ids_b) = (a.instance_id().expect("a"), b.instance_id().expect("b"));
    assert_ne!(ids_a, ids_b);
    let live = schema.query("SELECT instance_id::text FROM leases WHERE expires_at > now()");
    assert!(live.contains(&ids_a) && live.contains(&ids_b), "{live:?}");
    assert!(a.answers_the_probe() && b.answers_the_probe());

    let (revoked_cert, revoked_key) = pki.issue("edge-01");
    let (kept_cert, kept_key) = pki.issue("edge-02");
    let mut revoked = websocket(&b, &pki, &revoked_cert, &revoked_key).await;
    let mut kept = websocket(&b, &pki, &kept_cert, &kept_key).await;
    exchange(&mut revoked).await;
    exchange(&mut kept).await;

    let serial = cert_id(&revoked_cert).serial;
    let on_a = a.revocations.clone();
    tokio::task::spawn_blocking(move || on_a.revoke_certificate("client", &serial))
        .await
        .expect("revoke")
        .expect("revoke on A");
    assert_eq!(
        closed_within(&mut revoked, TIMES.renew * 3).await,
        Some((1008, "revoked".to_string())),
        "B did not end the revoked session within three renewal intervals"
    );
    assert_eq!(
        closed_within(&mut kept, Duration::from_millis(300)).await,
        None,
        "an unrelated session was closed"
    );
    exchange(&mut kept).await;
}

/// Forwards connections to `target`, holding every byte back while `frozen` is set: a database,
/// or the network to it, that stops answering.
fn freezable_proxy(target: SocketAddr) -> (u16, Arc<AtomicBool>) {
    use std::io::{Read as _, Write as _};
    let proxy = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = proxy.local_addr().expect("addr").port();
    let frozen = Arc::new(AtomicBool::new(false));
    let holding = frozen.clone();
    std::thread::spawn(move || {
        for client in proxy.incoming().flatten() {
            let Ok(server) = std::net::TcpStream::connect(target) else {
                continue;
            };
            for (mut from, mut to) in [
                (
                    client.try_clone().expect("clone"),
                    server.try_clone().expect("clone"),
                ),
                (server, client),
            ] {
                let frozen = holding.clone();
                std::thread::spawn(move || {
                    let mut buf = [0u8; 16 * 1024];
                    loop {
                        let n = match from.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        while frozen.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        if to.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    let _ = to.shutdown(std::net::Shutdown::Both);
                });
            }
        }
    });
    (port, frozen)
}

/// An instance whose database stops answering past its lease closes both listeners, so the probe
/// fails; once the database answers again it takes a new lease under a new instance id and opens
/// them again.
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_instance_without_a_lease_closes_its_listeners_until_it_holds_a_new_one() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let target = std::net::ToSocketAddrs::to_socket_addrs(&(
        schema.config.host.as_str(),
        schema.config.port,
    ))
    .expect("resolve")
    .next()
    .expect("an address");
    let (port, frozen) = freezable_proxy(target);
    let instance = Instance::start(&pki, &schema, schema.config_on(port)).await;
    let first = instance.instance_id().expect("a lease");
    assert!(instance.both_planes().0, "both planes serve");

    frozen.store(true, Ordering::SeqCst);
    assert!(
        eventually(TIMES.lapse + Duration::from_secs(2), || instance
            .instance_id()
            .is_none())
        .await,
        "the lease was not given up"
    );
    assert!(
        eventually(Duration::from_secs(2), || instance.both_planes().1).await,
        "a listener stayed open without a lease"
    );

    frozen.store(false, Ordering::SeqCst);
    assert!(
        eventually(Duration::from_secs(20), || instance.instance_id().is_some()).await,
        "no new lease was taken"
    );
    let second = instance.instance_id().expect("a new lease");
    assert_ne!(first, second, "a new lease is held under a new instance id");
    assert!(
        eventually(Duration::from_secs(5), || instance.both_planes().0).await,
        "the listeners were not opened again"
    );
}

/// An instance whose clock jumps past its lease's lapse while it did not run, as after a suspend
/// of its host, answers nothing more: its next request is not served, a message on a session it
/// carried is not answered and the session is cut, and it holds no lease until it takes a new one
/// under a new instance id, with both listeners open again.
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_instance_whose_clock_jumps_past_its_lapse_serves_nothing_until_it_holds_a_new_lease() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let instance = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let first = instance.instance_id().expect("a lease");
    let (cert, key) = pki.issue("agent-a");
    let mut session = websocket(&instance, &pki, &cert, &key).await;
    exchange(&mut session).await;
    let client = agent_client(&pki, &cert, &key);
    let url = agent_url(&instance, fleet_server::transport::PACKAGE_FLOORS_PATH);
    let answered = tokio::time::timeout(Duration::from_secs(5), client.get(&url).send())
        .await
        .expect("an answer in time")
        .expect("served")
        .status();
    assert!(
        answered.is_success(),
        "served before the suspend: {answered}"
    );

    // Every lease change from now on, as the planes see it.
    let mut held = instance.held.clone();
    held.borrow_and_update();
    let (seen, mut changes) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while held.changed().await.is_ok() {
            if seen.send(held.borrow_and_update().clone()).is_err() {
                return;
            }
        }
    });

    instance.suspend_for(TIMES.lapse + Duration::from_secs(1));
    let report = AgentToServer {
        instance_uid: InstanceUid::default().as_bytes().to_vec(),
        sequence_num: 2,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    };
    let framed = opamp::frame::encode_within(&report, usize::MAX).expect("frame");
    // The send may already find the session cut.
    let _ = session.send(Ws::Binary(framed.into())).await;
    match tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await {
        Ok(Ok(response)) => assert_eq!(
            response.status(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "a request after the suspend was served"
        ),
        Ok(Err(_)) => {} // The listener was closed already.
        Err(_) => panic!("the request after the suspend hung"),
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, session.next()).await {
            Err(_) => panic!("the session outlived the lease"),
            Ok(Some(Ok(Ws::Binary(_)))) => panic!("a message after the suspend was answered"),
            Ok(Some(Ok(Ws::Close(_))) | Some(Err(_)) | None) => break,
            Ok(Some(Ok(_))) => continue,
        }
    }

    let lost = tokio::time::timeout(Duration::from_secs(5), changes.recv())
        .await
        .expect("the lease was not given up after the suspend")
        .expect("the lease is watched");
    assert_eq!(lost, None, "the lease was given up before anything else");
    let again = tokio::time::timeout(Duration::from_secs(20), changes.recv())
        .await
        .expect("no new lease was taken")
        .expect("the lease is watched")
        .expect("a new lease");
    assert_ne!(first, again, "a new lease is held under a new instance id");
    assert!(
        eventually(Duration::from_secs(5), || instance.both_planes().0).await,
        "the listeners were not opened again"
    );
    let served = tokio::time::timeout(Duration::from_secs(5), client.get(&url).send())
        .await
        .expect("an answer in time")
        .expect("served")
        .status();
    assert!(served.is_success(), "served under the new lease: {served}");
}

/// The store as one instance opens it, in the test's schema: what each instance writes through.
fn records(schema: &Schema) -> Records {
    Records::Postgres(
        Database::open_in(&schema.config, Exclusive::Shared, Some(&schema.name))
            .expect("open the database"),
    )
}

/// Two instances that each revoke a certificate at the same moment keep both revocations: each
/// adds its own row, and neither writes the list it holds over the other's.
/// Verifies: ADR-0025, Q-1
#[test]
fn two_revocations_committed_at_once_on_two_instances_are_both_kept() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
    let instances: Vec<Arc<Revocations>> = (0..2)
        .map(|_| {
            Arc::new(
                Revocations::open(
                    records(&schema).ledger().expect("ledger"),
                    clock.clone(),
                    pki.authorities(),
                )
                .expect("revocations"),
            )
        })
        .collect();
    for round in 0..8u32 {
        let at_once = Arc::new(std::sync::Barrier::new(2));
        let revoking: Vec<_> = instances
            .iter()
            .enumerate()
            .map(|(n, instance)| {
                let (instance, at_once) = (instance.clone(), at_once.clone());
                std::thread::spawn(move || {
                    at_once.wait();
                    instance
                        .revoke_certificate("client", &format!("{}{round:x}", n + 1))
                        .expect("revoke")
                })
            })
            .collect();
        for thread in revoking {
            thread.join().expect("the revoking thread");
        }
    }
    let mut kept = schema.query("SELECT revoked->'certificate'->>'serial' FROM revocations");
    kept.sort();
    let mut expected: Vec<String> = (0..8u32)
        .flat_map(|round| (1..=2).map(move |n| format!("{n}{round:x}")))
        .collect();
    expected.sort();
    assert_eq!(
        kept, expected,
        "every revocation of both instances is on the list"
    );
}

/// An instance that holds an older floor in memory cannot lower the one another instance stored:
/// the floor is decided from the stored row, and the answer is the `409` of a lower floor, naming
/// the stored version, which the instance then holds.
/// Verifies: ADR-0025, ADR-0035, Q-6
#[test]
fn an_instance_with_an_older_floor_in_memory_cannot_lower_the_stored_one() {
    use fleet_server::floors::FloorError;
    let Some(schema) = Schema::named() else {
        return;
    };
    let a = PackageFloors::open(records(&schema).floors()).expect("floors on A");
    let b = PackageFloors::open(records(&schema).floors()).expect("floors on B");
    b.put("otelcol", "1.0.0", vec![1; 64])
        .expect("B sets 1.0.0");
    a.put("otelcol", "2.0.0", vec![2; 64])
        .expect("A raises it to 2.0.0");
    assert_eq!(
        b.all().get("otelcol").map(|floor| floor.version.clone()),
        Some("1.0.0".to_string()),
        "B holds the older floor in memory"
    );
    assert_eq!(
        b.put("otelcol", "1.5.0", vec![3; 64]),
        Err(FloorError::Lower("2.0.0".to_string()))
    );
    assert_eq!(
        schema.query("SELECT version FROM package_floors WHERE agent_type = 'otelcol'"),
        ["2.0.0"]
    );
    assert_eq!(
        b.all().get("otelcol").map(|floor| floor.version.clone()),
        Some("2.0.0".to_string()),
        "B holds what the refusal found stored"
    );
    assert!(a.remove("otelcol").expect("A removes it"));
    assert!(
        b.put("otelcol", "1.0.0", vec![4; 64]).is_ok(),
        "removed on A, it is set anew on B, whatever B held"
    );
}

/// Stores the Deployment "stable" that [`released`] records were released through: a rollout
/// releases through a stored Deployment.
fn store_stable(schema: &Schema) {
    // The first open applies the schema.
    drop(records(schema));
    schema.query(
        "INSERT INTO deployments (name, selector, packages, configurations) \
         VALUES ('stable', '{}', '{}', '{}') ON CONFLICT DO NOTHING",
    );
}

/// An Agent's record as a rollout leaves it, its assignment pinning `hash` of `configuration`.
fn released(configuration: &str, hash: &str) -> fleet_server::agent_store::PersistedAgent {
    fleet_server::agent_store::PersistedAgent {
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        description: None,
        health: None,
        effective_config: None,
        remote_config_status: None,
        connection_settings_status: None,
        package_statuses: None,
        available_components: None,
        transport: fleet_server::fleet::Transport::WebSocket,
        last_seen_ms: 0,
        restart_pending: false,
        assignment: Some(fleet_server::fleet::Assignment {
            deployment: "stable".to_string(),
            package: None,
            configurations: std::collections::BTreeMap::from([(
                configuration.to_string(),
                hash.to_string(),
            )]),
        }),
        labels: std::collections::BTreeMap::new(),
        host: None,
    }
}

/// Two instances that each roll a revision of one Configuration out at the same moment keep both
/// revisions: each adds the row it pins and deletes none, and the collection each runs after its
/// rollout reckons what the Configuration keeps from the stored assignments, in its own
/// transaction, not from the one assignment the instance knows of.
/// Verifies: ADR-0025, ADR-0033
#[test]
fn two_revisions_rolled_out_at_once_on_two_instances_are_both_kept() {
    use fleet_server::configs::{revision_hash, ConfigStore, Revision};
    let Some(schema) = Schema::named() else {
        return;
    };
    store_stable(&schema);
    let revision = |body: &str| Revision {
        body: body.to_string(),
        role: String::new(),
        service_name: "otelcol".to_string(),
    };
    let instances: Vec<_> = (0..2)
        .map(|_| {
            let records = records(&schema);
            let agents = records.agents().expect("agents");
            agents.load().expect("load");
            let configs = ConfigStore::open(records.configs().expect("configs")).expect("open");
            Arc::new((configs, agents))
        })
        .collect();
    instances[0]
        .0
        .put_saved("base", revision("v0\n"))
        .expect("save");
    let mut expected = Vec::new();
    for round in 0..8u32 {
        let at_once = Arc::new(std::sync::Barrier::new(2));
        let rolling: Vec<_> = instances
            .iter()
            .enumerate()
            .map(|(n, instance)| {
                let (instance, at_once) = (instance.clone(), at_once.clone());
                let body = format!("v{round}.{n}\n");
                expected.push(revision_hash(&revision(&body)));
                std::thread::spawn(move || {
                    let (configs, agents) = &*instance;
                    at_once.wait();
                    configs.put_saved("base", revision(&body)).expect("save");
                    let names = std::collections::BTreeSet::from(["base".to_string()]);
                    // The Agent reported before it is released to: a release writes the
                    // assignment of a record that is stored.
                    let uid = InstanceUid::default();
                    let reported = fleet_server::agent_store::PersistedAgent {
                        assignment: None,
                        ..released("base", "")
                    };
                    agents.put(&uid, &reported).expect("report");
                    configs
                        .pin(&names, |hashes, pinned| {
                            agents.release(pinned, &[(uid, released("base", &hashes["base"]))])
                        })
                        .expect("roll out");
                    configs
                        .retain_only("base", &agents.referenced_revisions("base"))
                        .expect("collect");
                })
            })
            .collect();
        for thread in rolling {
            thread.join().expect("the rolling thread");
        }
    }
    let mut kept = schema.query("SELECT hash FROM configuration_revisions");
    kept.sort();
    expected.sort();
    assert_eq!(
        kept, expected,
        "every revision an assignment pins is kept, and only those"
    );
    let mut pinned = schema.query("SELECT revision FROM agent_assignment_configurations");
    pinned.sort();
    assert_eq!(pinned, expected, "and every assignment is stored");
}

/// Two instances that each add an entry of another platform to one Package at the same moment
/// keep both: each writes the row of its platform, and neither writes the Package it holds over
/// the other's.
/// Verifies: ADR-0025, ADR-0034
#[test]
fn two_entries_added_at_once_on_two_instances_are_both_kept() {
    use fleet_server::packages::{PackageId, PackageStore, Platform};
    let Some(schema) = Schema::named() else {
        return;
    };
    let blobs = tempfile::tempdir().expect("tempdir");
    let instances: Vec<Arc<PackageStore>> = (0..2)
        .map(|n| {
            let root = blobs.path().join(n.to_string());
            std::fs::create_dir(&root).expect("the instance's directory");
            Arc::new(
                PackageStore::with_backend(
                    records(&schema).packages().expect("packages"),
                    Box::new(fleet_server::fs::FsBlobStore::open(&root).expect("blobs")),
                    fleet_server::clock::Elapsed::monotonic(),
                )
                .expect("open"),
            )
        })
        .collect();
    let mut expected = Vec::new();
    for round in 0..8u32 {
        let id = PackageId::new("otelcol", &format!("1.0.{round}")).expect("id");
        for instance in &instances {
            instance.create(&id).expect("create");
        }
        let at_once = Arc::new(std::sync::Barrier::new(2));
        let adding: Vec<_> = instances
            .iter()
            .zip(["amd64", "arm64"])
            .map(|(instance, arch)| {
                expected.push(format!("1.0.{round} {arch}"));
                let (instance, at_once, id) = (instance.clone(), at_once.clone(), id.clone());
                std::thread::spawn(move || {
                    let platform = Platform::new("linux", arch).expect("platform");
                    at_once.wait();
                    instance
                        .put_entry(&id, &platform, format!("{arch} {round}").into_bytes())
                        .expect("add the entry");
                })
            })
            .collect();
        for thread in adding {
            thread.join().expect("the adding thread");
        }
    }
    let mut kept = schema.query("SELECT version || ' ' || arch FROM package_entries");
    kept.sort();
    expected.sort();
    assert_eq!(kept, expected, "every entry of both instances is held");
}

/// Two instances that each add a member to one Deployment at the same moment keep both: each
/// writes the row of its member under the Deployment's row lock, and neither writes the
/// Deployment it holds over the other's. A type another instance filled is refused from what is
/// stored, whatever the refusing instance holds in memory.
/// Verifies: ADR-0025, ADR-0033, ADR-0034
#[test]
fn two_members_added_at_once_on_two_instances_are_both_kept() {
    use fleet_server::deployments::{DeploymentError, DeploymentStore};
    use fleet_server::packages::PackageId;
    let Some(schema) = Schema::named() else {
        return;
    };
    let instances: Vec<Arc<DeploymentStore>> = (0..2)
        .map(|_| {
            Arc::new(
                DeploymentStore::open(records(&schema).deployments().expect("deployments"))
                    .expect("open"),
            )
        })
        .collect();
    // An add refuses a member that is not stored: every one this test adds is.
    for side in ["a", "b"] {
        schema.query(&format!(
            "INSERT INTO configurations (name, body, role, service_name) \
             SELECT '{side}-' || round, '', '', 'otelcol' FROM generate_series(0, 7) AS round; \
             INSERT INTO packages (agent_type, version) \
             SELECT '{side}-type-' || round, '1.0.0' FROM generate_series(0, 7) AS round"
        ));
    }
    schema.query(
        "INSERT INTO packages (agent_type, version) VALUES ('otelcol', '1.0.0'), \
         ('otelcol', '2.0.0')",
    );
    let channel = std::collections::BTreeMap::from([("channel".to_string(), "stable".to_string())]);
    instances[0].put("stable", channel.clone()).expect("save");
    instances[1]
        .put("stable", channel)
        .expect("save on the other");
    let mut expected = Vec::new();
    for round in 0..8u32 {
        let at_once = Arc::new(std::sync::Barrier::new(2));
        let adding: Vec<_> = instances
            .iter()
            .zip(["a", "b"])
            .map(|(instance, side)| {
                let (configuration, agent_type) =
                    (format!("{side}-{round}"), format!("{side}-type-{round}"));
                expected.push((configuration.clone(), agent_type.clone()));
                let (instance, at_once) = (instance.clone(), at_once.clone());
                std::thread::spawn(move || {
                    let id = PackageId::new(&agent_type, "1.0.0").expect("id");
                    at_once.wait();
                    instance
                        .put_configuration("stable", &configuration)
                        .expect("add the Configuration");
                    instance
                        .put_package("stable", &id, false)
                        .expect("add the Package");
                })
            })
            .collect();
        for thread in adding {
            thread.join().expect("the adding thread");
        }
    }
    let sorted = |mut rows: Vec<String>| {
        rows.sort();
        rows
    };
    assert_eq!(
        sorted(schema.query("SELECT configuration FROM deployment_configurations")),
        sorted(expected.iter().map(|(c, _)| c.clone()).collect()),
        "every Configuration of both instances is held"
    );
    assert_eq!(
        sorted(schema.query("SELECT agent_type FROM deployment_packages")),
        sorted(expected.iter().map(|(_, p)| p.clone()).collect()),
        "every Package of both instances is held"
    );

    let held = PackageId::new("otelcol", "1.0.0").expect("id");
    instances[0]
        .put_package("stable", &held, false)
        .expect("A adds otelcol");
    assert_eq!(
        instances[1].put_package(
            "stable",
            &PackageId::new("otelcol", "2.0.0").expect("id"),
            false
        ),
        Err(DeploymentError::TypeTaken {
            agent_type: "otelcol".to_string(),
            held,
        }),
        "B, which never heard of it, is refused from what is stored"
    );
}

// ---- Every change reaches every instance (clauses 9, 10 and 12) ----

impl Schema {
    /// Announces `payload` on the schema's channel, past every adapter: what another instance, or
    /// a newer version of this one, sends.
    fn announce(&self, payload: &str) {
        self.sql(&format!(
            "SELECT pg_notify('opamp_fleet:' || current_schema(), '{}')",
            payload.replace('\'', "''")
        ));
    }
}

impl Instance {
    /// Waits until the instance has read the ledger again at least `reads` times for what it
    /// heard: its first read follows the listening connection being made.
    async fn has_read(&self, reads: u64) -> bool {
        eventually(Duration::from_secs(10), || {
            self.following.ledger_reads() >= reads
        })
        .await
    }
}

impl Pki {
    /// A certificate and key naming `host` the way the Server's own signer does (ADR-0027 clause
    /// 11), and no other name.
    fn issue_to_host(&self, host: &str) -> (String, String) {
        let ca_key = KeyPair::from_pem(&self.ca_key_pem).expect("ca key");
        let issuer = Issuer::from_ca_cert_pem(&self.ca_pem, ca_key).expect("issuer");
        let key = KeyPair::generate().expect("key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.subject_alt_names.push(rcgen::SanType::URI(
            format!("{}{host}", fleet_server::ca::HOST_URI_PREFIX)
                .try_into()
                .expect("uri"),
        ));
        let cert = params.signed_by(&key, &issuer).expect("signed");
        (cert.pem(), key.serialize_pem())
    }
}

/// A client of `instance`'s Agent plane over plain HTTP, presenting `cert`.
fn agent_client(pki: &Pki, cert: &str, key: &str) -> reqwest::Client {
    let mut pem = key.as_bytes().to_vec();
    pem.extend_from_slice(cert.as_bytes());
    reqwest::Client::builder()
        .use_rustls_tls()
        .tls_certs_only([reqwest::Certificate::from_pem(pki.ca_pem.as_bytes()).expect("ca")])
        .resolve(
            "localhost",
            "127.0.0.1:0".parse::<SocketAddr>().expect("address"),
        )
        .identity(reqwest::Identity::from_pem(&pem).expect("identity"))
        .build()
        .expect("client")
}

/// `path` on `instance`'s Agent plane.
fn agent_url(instance: &Instance, path: &str) -> String {
    format!("https://localhost:{}{path}", instance.agent.port())
}

/// Forwards connections to `target` until it is cut: a cut ends every connection it carries and
/// refuses new ones until it is restored. What the network to a database does when it drops.
struct Cuttable {
    port: u16,
    cut: Arc<AtomicBool>,
    carried: Arc<std::sync::Mutex<Vec<std::net::TcpStream>>>,
}

impl Cuttable {
    fn to(target: SocketAddr) -> Self {
        use std::io::{Read as _, Write as _};
        let proxy = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = proxy.local_addr().expect("addr").port();
        let cut = Arc::new(AtomicBool::new(false));
        let carried = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (refusing, carrying) = (cut.clone(), carried.clone());
        std::thread::spawn(move || {
            for client in proxy.incoming().flatten() {
                if refusing.load(Ordering::SeqCst) {
                    let _ = client.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let Ok(server) = std::net::TcpStream::connect(target) else {
                    continue;
                };
                {
                    let mut carried = carrying.lock().expect("carried");
                    carried.push(client.try_clone().expect("clone"));
                    carried.push(server.try_clone().expect("clone"));
                }
                for (mut from, mut to) in [
                    (
                        client.try_clone().expect("clone"),
                        server.try_clone().expect("clone"),
                    ),
                    (server, client),
                ] {
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 16 * 1024];
                        loop {
                            let n = match from.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => n,
                            };
                            if to.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        let _ = to.shutdown(std::net::Shutdown::Both);
                    });
                }
            }
        });
        Cuttable { port, cut, carried }
    }

    /// Ends every connection carried, and refuses new ones until [`restore`](Self::restore).
    fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
        for stream in self.carried.lock().expect("carried").drain(..) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    fn restore(&self) {
        self.cut.store(false, Ordering::SeqCst);
    }
}

/// The address the test's database answers on.
fn database_address(schema: &Schema) -> SocketAddr {
    std::net::ToSocketAddrs::to_socket_addrs(&(schema.config.host.as_str(), schema.config.port))
        .expect("resolve")
        .next()
        .expect("an address")
}

/// An instance whose listening connection is cut and restored acts as if everything had changed:
/// a revocation committed on A while B heard nothing ends B's session of that certificate once B
/// listens again.
/// Verifies: ADR-0025, ADR-0040
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_committed_while_the_listening_connection_was_cut_ends_its_session_once_back()
{
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let network = Cuttable::to(database_address(&schema));
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    // Only B's listening connection goes through the cut, so the cut cannot cost B its lease. It
    // follows without the renewals' comparison, which would catch the revocation on its own.
    let mut b = Instance::start_deaf(&pki, &schema, schema.config_on(schema.config.port)).await;
    b.follow_through(&schema, schema.config_on(network.port))
        .await;
    assert!(b.has_read(1).await, "B never listened");

    let (cert, key) = pki.issue("edge-01");
    let mut session = websocket(&b, &pki, &cert, &key).await;
    exchange(&mut session).await;

    network.cut();
    let serial = cert_id(&cert).serial;
    let on_a = a.revocations.clone();
    tokio::task::spawn_blocking(move || on_a.revoke_certificate("client", &serial))
        .await
        .expect("revoke")
        .expect("revoke on A");
    network.restore();

    assert_eq!(
        closed_within(&mut session, Duration::from_secs(15)).await,
        Some((1008, "revoked".to_string())),
        "B did not end the revoked session once it listened again"
    );
}

/// A notification of a kind the instance does not know, as a newer version sends, has it read
/// everything again: a revocation it was not told of ends its session.
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_notification_of_an_unknown_kind_has_everything_read_again() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(b.has_read(1).await, "B never listened");
    let (cert, key) = pki.issue("edge-01");
    let mut session = websocket(&b, &pki, &cert, &key).await;
    exchange(&mut session).await;

    let id = cert_id(&cert);
    let revoked = serde_json::json!({ "certificate": {
        "authority": "client",
        "issuers": [id.issuer],
        "serial": id.serial,
    }});
    schema.sql(&format!(
        "INSERT INTO revocations (id, position, revoked, revoked_ms) VALUES ('silent', 0, '{revoked}', 1)"
    ));
    schema.announce(r#"{"kind":"from-a-newer-version","id":"","from":"elsewhere"}"#);

    assert_eq!(
        closed_within(&mut session, Duration::from_secs(10)).await,
        Some((1008, "revoked".to_string())),
        "B did not read again for a kind it does not know"
    );
}

/// The `ETag` of the Gateways' revocation list one instance answered is answered `304` by
/// another, and a revocation on either moves it on both.
/// Verifies: ADR-0025, ADR-0040
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_etag_one_instance_answered_is_answered_304_by_another() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (cert, key) = pki.issue_to_host("gw-1");
    let gateway = agent_client(&pki, &cert, &key);
    let der = opamp::tls::certificates(cert.as_bytes()).expect("pem");
    let facts = fleet_server::ca::facts(der[0].as_ref()).expect("facts");
    let on_a = a.revocations.clone();
    let marked = tokio::task::spawn_blocking(move || {
        on_a.record(facts, b"gw", None)?;
        on_a.set_gateway("gw-1", true)
    })
    .await
    .expect("mark")
    .expect("mark on A");
    assert!(marked, "A knows the Gateway's host");
    assert!(
        eventually(Duration::from_secs(10), || b.revocations.is_gateway("gw-1")).await,
        "B never took up the Gateway's mark"
    );

    let fetch = |instance: &Instance, etag: Option<String>| {
        let request = gateway.get(agent_url(instance, "/v1/gateway/revocations"));
        let request = match etag {
            Some(etag) => request.header(reqwest::header::IF_NONE_MATCH, etag),
            None => request,
        };
        async move {
            let response = request.send().await.expect("fetch the list");
            let etag = response
                .headers()
                .get(reqwest::header::ETAG)
                .map(|etag| etag.to_str().expect("etag").to_string());
            (response.status(), etag)
        }
    };
    let mut moved = None;
    for (revoking, other) in [(&a, &b), (&b, &a)] {
        let (revoked, _) = pki.issue("edge-01");
        let id = cert_id(&revoked);
        let on = revoking.revocations.clone();
        let serial = id.serial.clone();
        tokio::task::spawn_blocking(move || on.revoke_certificate("client", &serial))
            .await
            .expect("revoke")
            .expect("revoke");
        assert!(
            eventually(Duration::from_secs(10), || other
                .revocations
                .is_certificate_revoked(&id))
            .await,
            "the other instance never took up the revocation"
        );
        let (status, etag) = fetch(revoking, None).await;
        assert_eq!(status, reqwest::StatusCode::OK);
        let etag = etag.expect("an ETag");
        let version = schema.query("SELECT version FROM revocation_list_version");
        assert!(
            etag.starts_with(&format!("\"list-{}-", version[0])),
            "the ETag names the list's version in the database: {etag}"
        );
        assert_ne!(Some(&etag), moved.as_ref(), "a revocation moves the ETag");
        assert_eq!(
            fetch(other, Some(etag.clone())).await.0,
            reqwest::StatusCode::NOT_MODIFIED,
            "the other instance answers the ETag the first gave"
        );
        moved = Some(etag);
    }
}

/// A floor set on A is what B serves the next Agent that asks for the floors.
/// Verifies: ADR-0025, ADR-0035
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_floor_set_on_one_instance_is_served_by_the_other() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(b.has_read(1).await, "B never listened");
    let (cert, key) = pki.issue("edge-01");
    let member = agent_client(&pki, &cert, &key);
    let served_by_b = || async {
        let floors: serde_json::Value = member
            .get(agent_url(&b, "/v1/package-floors"))
            .send()
            .await
            .expect("ask B")
            .json()
            .await
            .expect("json");
        floors["otelcol"]["version"].as_str().map(str::to_string)
    };
    assert_eq!(served_by_b().await, None);

    let on_a = a.floors.clone();
    tokio::task::spawn_blocking(move || on_a.put("otelcol", "2.0.0", vec![7; 64]))
        .await
        .expect("put")
        .expect("A sets the floor");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut served = served_by_b().await;
    while served.as_deref() != Some("2.0.0") && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
        served = served_by_b().await;
    }
    assert_eq!(served.as_deref(), Some("2.0.0"), "B serves the floor A set");
}

/// A signer that issues a host certificate of the test's CA for every request, whatever it asks.
struct Issuing {
    ca_pem: String,
    ca_key_pem: String,
}

impl fleet_server::fleet::CertificateSigner for Issuing {
    fn sign(
        &self,
        _csr_pem: &str,
        host: &str,
        _subject: &fleet_server::revocation::SubjectValues,
        _use_case: fleet_server::pkcs11::UseCase,
    ) -> Result<fleet_server::revocation::Signed, String> {
        let pki = Pki {
            ca_pem: self.ca_pem.clone(),
            ca_key_pem: self.ca_key_pem.clone(),
        };
        let (pem, _) = pki.issue_to_host(host);
        let der = opamp::tls::certificates(pem.as_bytes()).expect("pem");
        let facts = fleet_server::ca::facts(der[0].as_ref()).expect("facts");
        Ok(fleet_server::revocation::Signed { pem, facts })
    }

    fn renewal_proof(
        &self,
        _csr_pem: &str,
    ) -> Result<Option<fleet_server::revocation::Facts>, String> {
        Ok(None)
    }

    fn bootstrap_proof_id(&self, _csr_pem: &str) -> Result<Option<CertId>, String> {
        Ok(None)
    }

    fn check_claims(&self, _csr_pem: &str, _sender: &[u8]) -> Result<(), String> {
        Ok(())
    }
}

/// The request a host whose key has the fingerprint `key` sends, and who it is.
fn enrolment_request(
    key: &str,
) -> (
    fleet_server::enrolment::Request,
    fleet_server::enrolment::Requester,
) {
    (
        fleet_server::enrolment::Request {
            csr_pem: format!("csr-{key}"),
            subject: String::new(),
            key_fingerprint: key.to_string(),
            instance_uid: key.as_bytes().to_vec(),
        },
        fleet_server::enrolment::Requester {
            bootstrap_subject: "CN=bootstrap".to_string(),
            carried: Ok(fleet_server::revocation::SubjectValues::default()),
            bootstrap_fingerprint: "bootstrap".to_string(),
            bootstrap: CertId::new(b"bootstrap CA", "01"),
            peer: None,
            gateway: None,
        },
    )
}

/// Approves the request `key` on `instance`, recording its certificate in the register.
fn approve(instance: &Instance, pki: &Pki, key: &str) {
    let signer = Issuing {
        ca_pem: pki.ca_pem.clone(),
        ca_key_pem: pki.ca_key_pem.clone(),
    };
    instance
        .enrolment
        .approve(
            key,
            &signer,
            &format!("host-{key}"),
            &Registering(&instance.revocations),
            &|_| false,
        )
        .expect("approve");
}

/// Prepares an approval's register entry and Host on the instance's register, which the store
/// writes with the decision, and takes them up once it is written, as the Server does.
struct Registering<'a>(&'a Revocations);

impl fleet_server::enrolment::Registrar for Registering<'_> {
    fn register(
        &self,
        approved: &fleet_server::revocation::Approved,
    ) -> Result<(), fleet_server::enrolment::DecisionError> {
        self.0
            .register_approved(approved)
            .map_err(fleet_server::enrolment::DecisionError::Store)
    }

    fn committed(&self, approved: &fleet_server::revocation::Approved, written: Option<u64>) {
        self.0.took_up_approved(approved, written);
    }
}

impl fleet_server::enrolment::Recorder for Registering<'_> {
    fn signed(
        &self,
        signed: &fleet_server::revocation::Signed,
        _: &str,
        instance_uid: &[u8],
        bootstrap: &CertId,
    ) -> Result<Option<fleet_server::revocation::Approved>, fleet_server::enrolment::DecisionError>
    {
        self.0
            .prepare_approved(signed.facts.clone(), instance_uid, bootstrap)
            .map(Some)
            .map_err(fleet_server::enrolment::DecisionError::Store)
    }

    fn undecided(
        &self,
        _: &fleet_server::revocation::Signed,
        _: &str,
        _: fleet_server::enrolment::Undecided,
    ) {
    }
}

/// A bootstrap certificate, carrying the organisation and unit an approval carries on, and its
/// key.
fn issue_bootstrap(bootstrap: &Pki) -> (String, String) {
    let ca_key = KeyPair::from_pem(&bootstrap.ca_key_pem).expect("ca key");
    let issuer = Issuer::from_ca_cert_pem(&bootstrap.ca_pem, ca_key).expect("issuer");
    let key = KeyPair::generate().expect("key");
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "Kunde GmbH");
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationalUnitName, "Werk Nord");
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let cert = params.signed_by(&key, &issuer).expect("signed");
    (cert.pem(), key.serialize_pem())
}

/// A report of `uid` that asks for a certificate for `csr`.
fn csr_report(uid: &InstanceUid, csr: String) -> AgentToServer {
    use opamp::proto::{
        CertificateRequest, ConnectionSettingsRequest, OpAmpConnectionSettingsRequest,
    };
    AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        connection_settings_request: Some(ConnectionSettingsRequest {
            opamp: Some(OpAmpConnectionSettingsRequest {
                certificate_request: Some(CertificateRequest {
                    csr: csr.into_bytes(),
                }),
            }),
        }),
        ..Default::default()
    }
}

/// A CSR for a fresh key, with no proof, and that key.
fn fresh_csr() -> (String, KeyPair) {
    let key = KeyPair::generate().expect("key");
    let csr = CertificateParams::new(Vec::<String>::new())
        .expect("params")
        .serialize_request(&key)
        .expect("csr")
        .pem()
        .expect("pem");
    (csr, key)
}

/// A CSR for a fresh key that proves it renews `old_pem`, whose key is `old_key`, and that key.
fn renewing(old_pem: &str, old_key: &KeyPair) -> (String, KeyPair) {
    use base64::Engine as _;
    use rcgen::{PublicKeyData as _, SigningKey as _};
    use sha2::{Digest, Sha256};
    let new_key = KeyPair::generate().expect("new key");
    let old_der = opamp::tls::certificates(old_pem.as_bytes()).expect("pem")[0]
        .as_ref()
        .to_vec();
    let signature = old_key
        .sign(&fleet_core::renewal::statement(&Sha256::digest(
            new_key.subject_public_key_info(),
        )))
        .expect("sign");
    let proof = format!(
        "{}{}",
        fleet_core::renewal::URI_PREFIX,
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(fleet_core::renewal::encode(&old_der, &signature))
    );
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params
        .subject_alt_names
        .push(rcgen::SanType::URI(proof.try_into().expect("uri")));
    let csr = params
        .serialize_request(&new_key)
        .expect("csr")
        .pem()
        .expect("pem");
    (csr, new_key)
}

/// Sends `report` on `socket` and waits for the certificate the Server hands out on it.
async fn certificate_for(socket: &mut Socket, report: &AgentToServer, within: Duration) -> String {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message as Ws;
    let framed = opamp::frame::encode_within(report, usize::MAX).expect("frame");
    socket.send(Ws::Binary(framed.into())).await.expect("send");
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let message = next_message(socket, left)
            .await
            .expect("a certificate in time");
        if let Some(certificate) = message
            .connection_settings
            .and_then(|settings| settings.opamp)
            .and_then(|opamp| opamp.certificate)
        {
            return String::from_utf8(certificate.cert).expect("PEM");
        }
    }
}

/// Whether the certificate `pem` was signed by the key of the CA `ca_pem`.
fn signed_by(pem: &str, ca_pem: &str) -> bool {
    use x509_parser::prelude::{FromDer, X509Certificate};
    let der = |pem: &str| {
        opamp::tls::certificates(pem.as_bytes()).expect("pem")[0]
            .as_ref()
            .to_vec()
    };
    let (leaf, ca) = (der(pem), der(ca_pem));
    let (_, leaf) = X509Certificate::from_der(&leaf).expect("leaf");
    let (_, ca) = X509Certificate::from_der(&ca).expect("ca");
    leaf.verify_signature(Some(ca.public_key())).is_ok()
}

/// Two instances whose client CA is named by its label on one token, each having opened it in
/// this process, so one `cryptoki` context serves both, and that CA's certificate.
async fn on_one_token(
    schema: &Schema,
    pki: &Pki,
    test: &token::TestToken,
    dir: &std::path::Path,
) -> (Instance, Instance, String) {
    let opened = test.open();
    let (client_ca_pem, _) = test.ca(&opened, Signing::CLIENT_CA, "OpAMP Fleet Client CA", dir);
    let signing = Signing {
        token: test,
        client_ca_pem: client_ca_pem.clone(),
        bootstrap: Pki::named("OpAMP Fleet Bootstrap CA"),
    };
    let a =
        Instance::start_signing(pki, schema, schema.config_on(schema.config.port), &signing).await;
    let b =
        Instance::start_signing(pki, schema, schema.config_on(schema.config.port), &signing).await;
    (a, b, client_ca_pem)
}

/// Two instances whose client CA's key is named by its label on one SoftHSM2 token both start and
/// both sign renewals: a host renews on A and another on B, each to a certificate the token's key
/// signed, and each signature is in that instance's record. The hosts' certificates come from the
/// CA beside it in `client_ca_file`, so each moves to the token's key at its renewal.
/// Verifies: ADR-0043
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_instances_on_one_token_both_sign_renewals() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("dir");
    let (a, b, client_ca_pem) = on_one_token(&schema, &pki, &test, dir.path()).await;
    for (instance, host) in [(&a, "host-a"), (&b, "host-b")] {
        let (cert, key) = pki.issue_to_host(host);
        let old_key = KeyPair::from_pem(&key).expect("key");
        let mut socket = websocket(instance, &pki, &cert, &key).await;
        let (csr, _) = renewing(&cert, &old_key);
        let renewed = certificate_for(
            &mut socket,
            &csr_report(&InstanceUid::default(), csr),
            Duration::from_secs(10),
        )
        .await;
        assert!(signed_by(&renewed, &client_ca_pem), "{host}: {renewed}");
        let signed = instance
            .audit
            .as_ref()
            .expect("record")
            .events("pkcs11.signed");
        assert_eq!(signed.len(), 1, "{host}: {signed:?}");
        assert_eq!(signed[0]["use_case"], "renewal");
        assert_eq!(signed[0]["key_label"], Signing::CLIENT_CA);
    }
}

/// A host enrols through B's Agent plane, over its WebSocket, while A opened the window; it is
/// approved on A, whose client CA's key is on the token both share, and B hands the certificate to
/// the connection that asked. The host is admitted on both.
/// Verifies: ADR-0025, ADR-0027, ADR-0043
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_enrolment_approved_on_one_instance_is_handed_out_by_the_other() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("dir");
    let opened = test.open();
    let (client_ca_pem, _) = test.ca(
        &opened,
        Signing::CLIENT_CA,
        "OpAMP Fleet Client CA",
        dir.path(),
    );
    let signing = Signing {
        token: &test,
        client_ca_pem: client_ca_pem.clone(),
        bootstrap: Pki::named("OpAMP Fleet Bootstrap CA"),
    };
    let a = Instance::start_signing(
        &pki,
        &schema,
        schema.config_on(schema.config.port),
        &signing,
    )
    .await;
    let b = Instance::start_signing(
        &pki,
        &schema,
        schema.config_on(schema.config.port),
        &signing,
    )
    .await;
    a.enrolment.open(600).expect("A opens the window");
    assert!(
        eventually(Duration::from_secs(10), || b.enrolment.is_open()).await,
        "B never took up the window A opened"
    );
    let (bootstrap_cert, bootstrap_key) = issue_bootstrap(&signing.bootstrap);
    let mut enrolling = websocket(&b, &pki, &bootstrap_cert, &bootstrap_key).await;
    let (csr, host_key) = fresh_csr();
    let asked = csr_report(&InstanceUid::default(), csr);
    let approving = {
        let a_state = a.state.clone();
        let a_enrolment = a.enrolment.clone();
        tokio::spawn(async move {
            assert!(
                eventually(Duration::from_secs(10), || !a_enrolment
                    .pending()
                    .is_empty())
                .await,
                "A never listed the request B took in"
            );
            let id = a_enrolment.pending()[0].id.clone();
            tokio::task::spawn_blocking(move || a_state.approve_enrolment(&id))
                .await
                .expect("approve")
                .expect("approved on A");
        })
    };
    let issued = certificate_for(&mut enrolling, &asked, Duration::from_secs(20)).await;
    approving.await.expect("approved");
    assert!(signed_by(&issued, &client_ca_pem), "{issued}");
    let signed = a.audit.as_ref().expect("record").events("pkcs11.signed");
    assert_eq!(signed.len(), 1, "{signed:?}");
    assert_eq!(signed[0]["use_case"], "enrolment_approval");
    // A member now, on either instance.
    for instance in [&a, &b] {
        let mut member = websocket(instance, &pki, &issued, &host_key.serialize_pem()).await;
        exchange(&mut member).await;
    }
}

/// During an enrolment wave on A, A reads nothing again for its own notifications, and B takes up
/// every certificate and Host the wave registered. A then takes up the one change B commits, and
/// that once: what it heard of its own acts before was passed over.
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_instance_reads_nothing_again_for_its_own_notifications() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    // A without the renewals' comparison, whose read for B's revocation would count beside the
    // notification's.
    let a =
        Instance::start_following(&pki, &schema, schema.config_on(schema.config.port), false).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(
        a.has_read(1).await && b.has_read(1).await,
        "an instance never listened"
    );
    a.enrolment.open(600).expect("A opens the window");
    let before = a.following.ledger_reads();

    let keys: Vec<String> = (0..10).map(|n| format!("wave-{n}")).collect();
    for key in &keys {
        let (request, requester) = enrolment_request(key);
        let on_a = a.enrolment.clone();
        assert_eq!(
            tokio::task::spawn_blocking(move || on_a.submit(request, requester))
                .await
                .expect("submit"),
            fleet_server::enrolment::Submitted::Waiting
        );
        approve(&a, &pki, key);
    }
    let registered: Vec<CertId> = a
        .revocations
        .issued()
        .into_iter()
        .map(|issued| issued.facts.id)
        .collect();
    assert_eq!(registered.len(), keys.len());
    assert!(
        eventually(Duration::from_secs(10), || registered
            .iter()
            .all(|id| b.revocations.issued_entry(id).is_some()))
        .await,
        "B did not take up every certificate of the wave"
    );
    assert!(
        eventually(Duration::from_secs(10), || keys.iter().all(|key| b
            .revocations
            .hosts()
            .iter()
            .any(|(host, _, _)| *host == format!("host-{key}"))))
        .await,
        "B did not take up every Host of the wave"
    );

    let (unrelated, _) = pki.issue("edge-99");
    let id = cert_id(&unrelated);
    let on_b = b.revocations.clone();
    let serial = id.serial.clone();
    tokio::task::spawn_blocking(move || on_b.revoke_certificate("client", &serial))
        .await
        .expect("revoke")
        .expect("revoke on B");
    assert!(
        eventually(Duration::from_secs(10), || a
            .revocations
            .is_certificate_revoked(&id))
        .await,
        "A never took up B's revocation"
    );
    assert!(
        a.has_read(before + 1).await,
        "A did not read for B's change"
    );
    assert_eq!(
        a.following.ledger_reads(),
        before + 1,
        "A read the ledger again for its own notifications"
    );
}

/// The next message `session` is sent within `within`, or `None` if none is.
async fn next_message(
    session: &mut Socket,
    within: Duration,
) -> Option<opamp::proto::ServerToAgent> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as Ws;
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, session.next()).await {
            Err(_) => return None,
            Ok(Some(Ok(Ws::Binary(bytes)))) => {
                return Some(opamp::frame::decode(&bytes, usize::MAX).expect("decode"));
            }
            Ok(Some(Ok(_))) => continue,
            other => panic!("the session ended: {other:?}"),
        }
    }
}

/// A rollout committed on A reaches an Agent B carries: B takes up the Configuration A saved and
/// the assignment A released, and offers the Agent the new configuration within one second.
/// Verifies: ADR-0025, ADR-0033
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rollout_on_one_instance_is_offered_by_the_instance_that_carries_the_agent() {
    use futures_util::SinkExt;
    use opamp::proto::{AgentDescription, AnyValue, KeyValue};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (cert, key) = pki.issue("edge-01");
    let mut session = websocket(&b, &pki, &cert, &key).await;
    let uid = InstanceUid([7; 16]);
    let report = AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num: 1,
        capabilities: (AgentCapabilities::ReportsStatus as u64)
            | (AgentCapabilities::AcceptsRemoteConfig as u64)
            | (AgentCapabilities::ReportsRemoteConfig as u64),
        agent_description: Some(AgentDescription {
            identifying_attributes: vec![KeyValue {
                key: "service.name".to_string(),
                value: Some(AnyValue {
                    value: Some(opamp::proto::any_value::Value::StringValue(
                        "otelcol".to_string(),
                    )),
                }),
            }],
            non_identifying_attributes: Vec::new(),
        }),
        ..Default::default()
    };
    let framed = opamp::frame::encode_within(&report, usize::MAX).expect("frame");
    session.send(Ws::Binary(framed.into())).await.expect("send");
    next_message(&mut session, Duration::from_secs(5))
        .await
        .expect("B answers the report");
    assert!(
        eventually(Duration::from_secs(10), || schema
            .query("SELECT instance_uid FROM agents")
            .len()
            == 1)
        .await,
        "B never wrote the Agent's record"
    );

    // A holds none of it in memory: it matches the Agent as the database stores it.
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(a.state.snapshot().is_empty(), "A holds no Agent");
    let on_a = a.state.clone();
    let assigned = tokio::task::spawn_blocking(move || {
        on_a.save_configuration(
            "collector",
            fleet_server::configs::Revision {
                body: "receivers: {}".to_string(),
                role: String::new(),
                service_name: "otelcol".to_string(),
            },
        )?;
        on_a.deployment_store()
            .expect("deployments")
            .put(
                "stable",
                std::collections::BTreeMap::from([(
                    "service.name".to_string(),
                    "otelcol".to_string(),
                )]),
            )
            .map_err(|e| format!("{e:?}"))?;
        on_a.put_deployment_configuration("stable", "collector")
            .map_err(|e| format!("{e:?}"))?;
        on_a.rollout_deployment("stable")
            .map_err(|e| format!("{e:?}"))
    })
    .await
    .expect("roll out")
    .expect("roll out on A");
    assert_eq!(assigned, 1, "A released to the Agent B carries");

    let offer = next_message(&mut session, Duration::from_secs(1))
        .await
        .expect("B offered nothing within one second");
    let body = offer
        .remote_config
        .and_then(|config| config.config)
        .and_then(|map| map.config_map.into_values().next())
        .map(|file| String::from_utf8(file.body).expect("utf-8"));
    assert_eq!(body.as_deref(), Some("receivers: {}"));
}

/// A Package created on A, and one deleted there, is what B lists next: B drops the Packages it
/// holds for each notification and reads them again.
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_package_published_on_one_instance_is_listed_by_the_other() {
    use fleet_server::packages::PackageId;
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(b.has_read(1).await, "B never listened");
    let id = PackageId::new("otelcol", "1.0.0").expect("id");
    let listed_by_b = |id: &PackageId| {
        b.state
            .packages()
            .expect("packages")
            .list()
            .iter()
            .any(|package| package.id == *id)
    };
    let on_a = a.state.clone();
    let created = id.clone();
    tokio::task::spawn_blocking(move || on_a.create_package_set(&created))
        .await
        .expect("create")
        .expect("A creates the Package");
    assert!(
        eventually(Duration::from_secs(10), || listed_by_b(&id)).await,
        "B never listed the Package A created"
    );
    let on_a = a.state.clone();
    let deleted = id.clone();
    assert!(
        tokio::task::spawn_blocking(move || on_a.delete_package_set(&deleted))
            .await
            .expect("delete")
            .expect("A deletes the Package")
    );
    assert!(
        eventually(Duration::from_secs(10), || !listed_by_b(&id)).await,
        "B still lists the Package A deleted"
    );
}

/// A Configuration and a Package deleted through B leave the Deployment that held them in A's
/// memory too: the transaction that deletes them announces each Deployment it took them out of,
/// and A reads the Deployments again for it.
/// Verifies: ADR-0025, ADR-0033
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_deleted_through_one_instance_leaves_the_deployments_the_other_holds() {
    use fleet_server::packages::PackageId;
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(a.has_read(1).await, "A never listened");
    assert!(b.has_read(1).await, "B never listened");
    let id = PackageId::new("otelcol", "1.0.0").expect("id");
    let on_a = a.state.clone();
    let held = id.clone();
    tokio::task::spawn_blocking(move || {
        on_a.save_configuration(
            "base",
            fleet_server::configs::Revision {
                body: "receivers: {}\n".to_string(),
                role: String::new(),
                service_name: "otelcol".to_string(),
            },
        )?;
        on_a.create_package_set(&held)?;
        on_a.deployment_store()
            .expect("deployments")
            .put(
                "canary",
                std::collections::BTreeMap::from([("ring".to_string(), "canary".to_string())]),
            )
            .map_err(|e| format!("{e:?}"))?;
        on_a.put_deployment_configuration("canary", "base")
            .map_err(|e| format!("{e:?}"))?;
        on_a.put_deployment_package("canary", &held, false)
            .map_err(|e| format!("{e:?}"))
    })
    .await
    .expect("hold them")
    .expect("A puts both into canary");
    let members = |instance: &Instance| {
        instance
            .state
            .deployment_store()
            .expect("deployments")
            .get("canary")
            .map(|canary| (canary.configurations.len(), canary.packages.len()))
    };
    assert!(
        eventually(Duration::from_secs(10), || members(&b) == Some((1, 1))).await,
        "B never held canary with both"
    );
    assert!(
        eventually(Duration::from_secs(10), || b
            .state
            .packages()
            .expect("packages")
            .list()
            .iter()
            .any(|package| package.id == id))
        .await,
        "B never listed the Package"
    );
    let on_b = b.state.clone();
    let deleted = id.clone();
    tokio::task::spawn_blocking(move || {
        assert!(
            on_b.delete_configuration("base").expect("B deletes it"),
            "B deletes the Configuration"
        );
        assert_eq!(
            on_b.delete_package_set(&deleted),
            Ok(true),
            "B deletes the Package"
        );
    })
    .await
    .expect("delete");
    assert!(
        eventually(Duration::from_secs(10), || members(&a) == Some((0, 0))).await,
        "A's canary still holds what B deleted: {:?}",
        members(&a)
    );
}

/// A revocation whose notification never arrives, while the listening connection stays up, is
/// caught by the next lease renewal: it finds the list's version moved, and the instance reads the
/// ledger again and ends the session it concerns.
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_whose_notification_is_missed_is_caught_at_the_next_renewal() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(b.has_read(1).await, "B never listened");
    let (cert, key) = pki.issue("edge-01");
    let mut session = websocket(&b, &pki, &cert, &key).await;
    exchange(&mut session).await;

    let id = cert_id(&cert);
    let revoked = serde_json::json!({ "certificate": {
        "authority": "client",
        "issuers": [id.issuer],
        "serial": id.serial,
    }});
    // Committed as an instance commits it, its notification lost on the way.
    schema.sql(&format!(
        "INSERT INTO revocations (id, position, revoked, revoked_ms) VALUES ('missed', 0, '{revoked}', 1); \
         UPDATE revocation_list_version SET version = version + 1"
    ));

    assert_eq!(
        closed_within(&mut session, TIMES.renew * 3).await,
        Some((1008, "revoked".to_string())),
        "B did not catch the missed revocation at a renewal"
    );
}

/// A binding and its Host committed by another instance that ended before it sent the notice
/// that follows the commit are taken up by the next lease renewal, which reads the ledger again
/// (ADR-0025 clauses 9 and 10).
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_binding_whose_notice_after_its_commit_is_lost_is_taken_up_at_the_next_renewal() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(b.has_read(1).await, "B never listened");
    // Committed as an instance commits a first report's binding, its notice never sent.
    schema.sql(
        "INSERT INTO hosts (host, gateway, instance_uids, subject, valid_until_ms) \
         VALUES ('edge-missed', false, '{}', '{}', 9000000000000); \
         INSERT INTO host_bindings (instance_uid, host) VALUES ('0a', 'edge-missed')",
    );
    assert!(
        eventually(TIMES.renew * 3, || b
            .revocations
            .bound_host(&[0x0a])
            .as_deref()
            == Some("edge-missed"))
        .await,
        "B did not take up the binding at a renewal"
    );
    assert!(
        b.revocations
            .hosts()
            .iter()
            .any(|(host, _, _)| host == "edge-missed"),
        "nor its Host"
    );
}

/// A renewal registered by another writer, whose notification never arrives while the listening
/// connection stays up, is taken up by the next lease renewal: so a revocation of the certificate
/// it renewed, committed by this instance afterwards, refuses the renewal too (ADR-0040 clause 28,
/// ADR-0023 clause 10).
/// Verifies: ADR-0025, ADR-0023
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_renewal_whose_notification_is_missed_is_refused_once_its_predecessor_is_revoked() {
    use fleet_server::revocation::{Issued, LedgerStore, RegisterBounds, Registered};
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(b.has_read(1).await, "B never listened");
    let (old, _) = pki.issue("edge-01");
    let (renewed, _) = pki.issue("edge-01");
    let old = cert_id(&old);
    let renewed = {
        let der = opamp::tls::certificates(renewed.as_bytes()).expect("pem");
        fleet_server::ca::facts(der[0].as_ref()).expect("facts")
    };
    let renewed_id = renewed.id.clone();

    // Registered as an instance registers it, through a writer whose notification B takes for its
    // own and so never takes up: a notification missed.
    let writer = fleet_server::postgres::ledger::PgLedgerStore::new(b.database.clone());
    let predecessor = old.clone();
    let registered = tokio::task::spawn_blocking(move || {
        writer.register_issued(
            &Issued {
                facts: renewed,
                instance_uid: "01".into(),
                predecessor: Some(predecessor),
                issued_ms: 1,
            },
            &RegisterBounds {
                size: 100,
                per_root: 100,
            },
        )
    })
    .await
    .expect("register")
    .expect("register the renewal");
    assert_eq!(registered, Registered::Registered);
    let on_b = b.revocations.clone();
    let serial = old.serial.clone();
    tokio::task::spawn_blocking(move || on_b.revoke_certificate("client", &serial))
        .await
        .expect("revoke")
        .expect("revoke on B");
    assert!(b.revocations.is_certificate_revoked(&old));
    assert!(
        eventually(TIMES.renew * 3, || b
            .revocations
            .is_certificate_revoked(&renewed_id))
        .await,
        "B admits the renewal of the certificate it revoked"
    );
}

/// An instance killed without a shutdown leaves its lease and its audit chain behind; a running
/// instance takes the lease over at one of its renewals once it lapsed, and the dead chain ends
/// with `audit.chain_closed`, `reason` `lease_lapsed`, `closed_by` the running instance's chain
/// (ADR-0025
/// clause 18).
/// Verifies: ADR-0029, ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_instance_closes_the_chain_of_one_killed_at_its_renewal() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let running = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let closer = running.instance_id().expect("a lease");
    // Killed: a lease and its chain, and nothing renews or closes them.
    let (config, name) = (schema.config_on(schema.config.port), schema.name.clone());
    let dead = tokio::task::spawn_blocking(move || {
        let database = Database::open_in(&config, Exclusive::Shared, Some(&name)).expect("open");
        database
            .take_lease("killed-host", TIMES.lapse)
            .expect("lease")
            .instance_id
    })
    .await
    .expect("take");
    schema.sql(&format!(
        "UPDATE leases SET expires_at = now() WHERE instance_id = '{dead}'"
    ));
    let last = || {
        schema
            .query(&format!(
                "SELECT line FROM audit_entries WHERE chain = '{dead}' ORDER BY seq DESC LIMIT 1"
            ))
            .pop()
            .unwrap_or_default()
    };
    assert!(
        eventually(TIMES.renew * 5, || last().contains("audit.chain_closed")).await,
        "no renewal closed the dead chain: {}",
        last()
    );
    let closing = last();
    assert!(closing.contains("\"reason\":\"lease_lapsed\""), "{closing}");
    assert!(
        closing.contains(&format!("\"closed_by\":\"{closer}\"")),
        "{closing}"
    );
    assert!(schema
        .query(&format!(
            "SELECT 1 FROM leases WHERE instance_id = '{dead}'"
        ))
        .is_empty());
}

/// The attributes an Agent describes itself by: the identifying ones, then the others.
type Attributes<'a> = (&'a [(&'a str, &'a str)], &'a [(&'a str, &'a str)]);

/// One report of `uid` on `session`, its reply awaited: its capabilities, the attributes it
/// describes itself by, the second set non-identifying, and whether it is healthy.
async fn report_as(
    session: &mut Socket,
    uid: InstanceUid,
    sequence_num: u64,
    capabilities: u64,
    attributes: Attributes<'_>,
    healthy: bool,
) -> opamp::proto::ServerToAgent {
    use futures_util::SinkExt;
    use opamp::proto::{AgentDescription, AnyValue, ComponentHealth, KeyValue};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let pairs = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(key, value)| KeyValue {
                key: (*key).to_string(),
                value: Some(AnyValue {
                    value: Some(opamp::proto::any_value::Value::StringValue(
                        (*value).to_string(),
                    )),
                }),
            })
            .collect()
    };
    let report = AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::ReportsHealth as u64
            | capabilities,
        agent_description: Some(AgentDescription {
            identifying_attributes: pairs(attributes.0),
            non_identifying_attributes: pairs(attributes.1),
        }),
        health: Some(ComponentHealth {
            healthy,
            ..Default::default()
        }),
        ..Default::default()
    };
    let framed = opamp::frame::encode_within(&report, usize::MAX).expect("frame");
    session.send(Ws::Binary(framed.into())).await.expect("send");
    next_message(session, Duration::from_secs(5))
        .await
        .expect("an answer to the report")
}

/// The view `instance` holds of `uid`.
fn view_of(instance: &Instance, uid: InstanceUid) -> Option<fleet_server::fleet::AgentView> {
    instance
        .state
        .snapshot()
        .into_iter()
        .find(|view| view.instance_uid == uid.to_string())
}

/// Labels set through B on a Client Agent A carries stay as B set them when A writes the Agent's
/// next report before it heard of them, since A's write is of what a report decides alone; A then
/// takes them up, and a later change through B reaches A's view within one second.
/// Verifies: ADR-0030, ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn labels_set_through_one_instance_stay_and_reach_the_instance_that_carries_the_agent() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    // B starts before the Agent first reports, so it knows it from the database alone.
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let mut a = Instance::start_deaf(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (cert, key) = pki.issue("edge-01");
    let client = InstanceUid([0x11; 16]);
    let client_attributes: Attributes<'_> = (
        &[("service.name", "fleet-agent")],
        &[("supervisor.kind.otelcol", "true")],
    );
    let mut client_session = websocket(&a, &pki, &cert, &key).await;
    report_as(&mut client_session, client, 1, 0, client_attributes, true).await;
    let labels = |uid: InstanceUid| {
        schema.query(&format!(
            "SELECT labels::text FROM agents WHERE instance_uid = '{uid}'"
        ))
    };

    let on_b = b.state.clone();
    let set = move |tier: &'static str| {
        let on_b = on_b.clone();
        async move {
            tokio::task::spawn_blocking(move || {
                on_b.set_labels(
                    &client,
                    std::collections::BTreeMap::from([("tier".to_string(), tier.to_string())]),
                )
            })
            .await
            .expect("set labels")
        }
    };
    assert!(
        set("edge").await.is_ok(),
        "B sets the labels of an Agent it never held"
    );
    // A has not heard of them, and writes the Client Agent's next report, which changes its health.
    report_as(&mut client_session, client, 2, 0, client_attributes, false).await;
    assert_eq!(
        labels(client),
        [r#"{"tier":"edge"}"#],
        "the labels stay as B set them"
    );
    assert!(
        view_of(&a, client).is_some_and(|view| view.labels.is_empty()),
        "A had not heard of them"
    );

    a.follow();
    let holds = |tier: &str| {
        view_of(&a, client)
            .is_some_and(|view| view.labels.get("tier").map(String::as_str) == Some(tier))
    };
    assert!(
        eventually(Duration::from_secs(10), || holds("edge")).await,
        "A took the labels up once it followed"
    );
    assert!(set("core").await.is_ok());
    assert!(
        eventually(Duration::from_secs(1), || holds("core")).await,
        "A took the change up within one second"
    );
}

/// A restart requested through B for an Agent A carries stays queued when A writes the Agent's
/// next report before it heard of it; A then takes it up and sends the restart, which clears it,
/// and a request made after that is queued again and sent within one second.
/// Verifies: ADR-0030, ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_requested_through_one_instance_stays_until_the_carrying_instance_sends_it() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let mut a = Instance::start_deaf(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (cert, key) = pki.issue("edge-01");
    let uid = InstanceUid([0x21; 16]);
    let restartable = AgentCapabilities::AcceptsRestartCommand as u64;
    let attributes: Attributes<'_> = (&[("service.name", "otelcol")], &[]);
    let mut session = websocket(&a, &pki, &cert, &key).await;
    report_as(&mut session, uid, 1, restartable, attributes, true).await;
    let queued = || {
        schema.query(&format!(
            "SELECT restart_pending::text FROM agents WHERE instance_uid = '{uid}'"
        ))
    };
    let request = || {
        let on_b = b.state.clone();
        async move {
            tokio::task::spawn_blocking(move || on_b.request_restart(&uid).is_ok())
                .await
                .expect("request")
        }
    };
    let is_restart = |message: &opamp::proto::ServerToAgent| {
        message
            .command
            .as_ref()
            .is_some_and(|command| command.r#type == opamp::proto::CommandType::Restart as i32)
    };

    assert!(
        request().await,
        "B queues the restart of an Agent it never held"
    );
    let answer = report_as(&mut session, uid, 2, restartable, attributes, false).await;
    assert!(!is_restart(&answer), "A had not heard of it");
    assert_eq!(queued(), ["true"], "the request stays queued");

    a.follow();
    let sent = next_message(&mut session, Duration::from_secs(10))
        .await
        .expect("A sends the restart once it follows");
    assert!(is_restart(&sent), "{sent:?}");
    assert_eq!(queued(), ["false"], "sending it cleared it");

    assert!(request().await, "a later request");
    let sent = next_message(&mut session, Duration::from_secs(1))
        .await
        .expect("A sends the later restart within one second");
    assert!(is_restart(&sent), "{sent:?}");
    assert_eq!(queued(), ["false"]);
}

/// The ceiling on Agent records holds for the fleet: with room for one, an Agent first heard of
/// through B is refused once A stored another, and the one A stored keeps reporting through B.
/// Verifies: ADR-0025, ADR-0040
#[test]
fn the_ceiling_on_agent_records_holds_across_instances() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let [a, b] = [(); 2].map(|()| {
        AppState::on(&records(&schema))
            .expect("state")
            .with_max_agents(1)
    });
    let report = |uid: InstanceUid| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    };
    let (first, second) = (InstanceUid([0x31; 16]), InstanceUid([0x32; 16]));
    let refused = |processed: fleet_server::fleet::Processed| {
        processed
            .reply
            .error_response
            .is_some_and(|error| error.error_message.contains("ceiling"))
    };
    assert!(!refused(a.process(report(first), Transport::Http, None)));
    assert!(
        refused(b.process(report(second), Transport::Http, None)),
        "B counts what A stored"
    );
    assert!(
        !refused(b.process(report(first), Transport::Http, None)),
        "a stored Agent passes no ceiling"
    );
    assert_eq!(
        schema.query("SELECT instance_uid FROM agents"),
        [first.to_string()]
    );
}

/// Many Agents created at once through twelve instances leave no more records than `max_agents`:
/// each creation counts the records the others created, however many are made at once.
/// Verifies: ADR-0025, ADR-0030
#[test]
fn many_creations_at_once_on_many_instances_never_pass_the_ceiling() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    const CEILING: usize = 40;
    let states: Vec<Arc<AppState>> = (0..12)
        .map(|_| {
            Arc::new(
                AppState::on(&records(&schema))
                    .expect("state")
                    .with_max_agents(CEILING),
            )
        })
        .collect();
    let creating: Vec<_> = (0..24u8)
        .map(|thread| {
            let state = states[usize::from(thread) % 12].clone();
            std::thread::spawn(move || {
                for n in 0..10u8 {
                    let mut uid = [0x70; 16];
                    (uid[1], uid[2]) = (thread, n);
                    state.process(
                        AgentToServer {
                            instance_uid: uid.to_vec(),
                            sequence_num: 1,
                            capabilities: AgentCapabilities::ReportsStatus as u64,
                            ..Default::default()
                        },
                        Transport::Http,
                        None,
                    );
                }
            })
        })
        .collect();
    for thread in creating {
        thread.join().expect("the creations");
    }
    assert_eq!(
        schema.query("SELECT count(*)::text FROM agents"),
        [CEILING.to_string()],
        "the records stored"
    );
    assert_eq!(
        schema.query("SELECT agents::text FROM agent_count"),
        [CEILING.to_string()],
        "the count kept"
    );
}

/// An Agent carried by A that went quiet is forgotten through B, which decides from the presence
/// A wrote: refused while it reports, forgotten once it is silent past the budget, and A then ends
/// its session within one second.
/// Verifies: ADR-0030, ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_forgotten_through_one_instance_has_its_session_ended_by_the_one_that_carries_it()
{
    use fleet_server::fleet::ForgetError;
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (cert, key) = pki.issue("edge-01");
    let uid = InstanceUid([0x41; 16]);
    let attributes: Attributes<'_> = (&[("service.name", "otelcol")], &[]);
    let mut session = websocket(&a, &pki, &cert, &key).await;
    report_as(&mut session, uid, 1, 0, attributes, true).await;
    let (on_a, holder) = (a.state.clone(), a.instance_id().expect("a lease"));
    tokio::task::spawn_blocking(move || on_a.write_presence(&holder))
        .await
        .expect("batch")
        .expect("A writes the presence");
    let forget = || {
        let on_b = b.state.clone();
        async move {
            tokio::task::spawn_blocking(move || on_b.forget_agent(&uid))
                .await
                .expect("forget")
        }
    };

    assert!(
        matches!(forget().await, Err(ForgetError::StillReporting)),
        "B sees A holds it, and that it reports"
    );
    // The Agent went quiet two minutes ago, past any budget; A's presence is as it last wrote it.
    schema.sql(&format!(
        "UPDATE agents SET last_seen_ms = last_seen_ms - 120000 WHERE instance_uid = '{uid}'"
    ));
    assert!(forget().await.is_ok(), "B forgets an Agent it never held");
    assert_eq!(
        closed_within(&mut session, Duration::from_secs(1)).await,
        Some((1000, "forgotten".to_string())),
        "A ends its session within one second"
    );
    assert!(view_of(&a, uid).is_none(), "and holds it no more");
}

/// An Agent forgotten through B stays forgotten when A, which holds it and has not heard of it
/// yet, writes its next report or flushes at shutdown: A writes nothing of it again, neither the
/// record nor the labels it held, and drops the Agent it reported for and ends its session.
/// Verifies: ADR-0030
#[test]
fn an_agent_forgotten_through_one_instance_is_not_written_again_by_another() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let [a, b] = [(); 2].map(|()| AppState::on(&records(&schema)).expect("state"));
    let (reporting, flushed) = (InstanceUid([0x51; 16]), InstanceUid([0x52; 16]));
    let report = |uid: InstanceUid, sequence_num: u64, healthy: bool| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::ReportsHealth as u64,
        health: Some(opamp::proto::ComponentHealth {
            healthy,
            ..Default::default()
        }),
        ..Default::default()
    };
    for (conn, uid) in [(7, reporting), (8, flushed)] {
        a.process(report(uid, 1, true), Transport::WebSocket, Some(conn));
    }
    assert!(b
        .set_labels(
            &reporting,
            std::collections::BTreeMap::from([("tier".to_string(), "edge".to_string())]),
        )
        .is_ok());
    // No presence is stored for either, and B decides from what is stored.
    for uid in [reporting, flushed] {
        assert!(b.forget_agent(&uid).is_ok(), "B forgets {uid}");
    }

    // A has not heard of it, and the next report changed its health.
    let answered = a.process(report(reporting, 2, false), Transport::WebSocket, Some(7));
    assert_eq!(answered.uid, None, "A carries it no more");
    a.flush_agents();
    assert!(
        schema.query("SELECT instance_uid FROM agents").is_empty(),
        "neither is written again"
    );
    assert_eq!(a.take_forgotten(7), [reporting], "its session ends");
    assert!(a
        .snapshot()
        .iter()
        .all(|view| view.instance_uid != reporting.to_string()));
}

/// An Agent that asks for a new identity keeps its stored record under the new key, labels set
/// through another instance included, and the instance that carries it keeps carrying it: the
/// record moves in the store as it is stored, and is not dropped as one forgotten.
/// Verifies: ADR-0030
#[test]
fn an_agent_rekeyed_at_its_request_keeps_its_stored_record() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let [a, b] = [(); 2].map(|()| AppState::on(&records(&schema)).expect("state"));
    let uid = InstanceUid([0x53; 16]);
    let report = |flags: u64, sequence_num: u64| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        flags,
        ..Default::default()
    };
    a.process(report(0, 1), Transport::WebSocket, Some(7));
    assert!(b
        .set_labels(
            &uid,
            std::collections::BTreeMap::from([("tier".to_string(), "edge".to_string())]),
        )
        .is_ok());
    let rekeyed = a
        .process(
            report(
                opamp::proto::AgentToServerFlags::RequestInstanceUid as u64,
                2,
            ),
            Transport::WebSocket,
            Some(7),
        )
        .uid
        .expect("carried under its new identity");
    assert_ne!(rekeyed, uid);
    assert_eq!(
        schema.query("SELECT instance_uid FROM agents"),
        [rekeyed.to_string()]
    );
    assert_eq!(
        schema.query("SELECT labels::text FROM agents"),
        [r#"{"tier":"edge"}"#],
        "the labels B set stay"
    );
    assert!(a
        .snapshot()
        .iter()
        .any(|view| view.instance_uid == rekeyed.to_string()));
}

/// An Agent rekeyed at its request through A is dropped under its old identity by B, which
/// held it: the move announces the old identity forgotten, so B holds no record of it under a
/// name no store keeps.
/// Verifies: ADR-0030
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_rekeyed_through_one_instance_is_dropped_under_its_old_identity_by_another() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Arc::new(AppState::on(&records(&schema)).expect("state"));
    let uid = InstanceUid([0x54; 16]);
    let report = move |flags: u64, sequence_num: u64| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        flags,
        ..Default::default()
    };
    // B holds the record the Agent's report to it loaded, and listens before A moves it.
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(b.has_read(1).await, "B follows");
    let on_b = b.state.clone();
    tokio::task::spawn_blocking(move || on_b.process(report(0, 1), Transport::WebSocket, Some(9)))
        .await
        .expect("report to B");
    let on_a = a.clone();
    tokio::task::spawn_blocking(move || on_a.process(report(0, 2), Transport::WebSocket, Some(7)))
        .await
        .expect("report");
    assert!(view_of(&b, uid).is_some(), "B holds it");

    let on_a = a.clone();
    let rekeyed = tokio::task::spawn_blocking(move || {
        on_a.process(
            report(
                opamp::proto::AgentToServerFlags::RequestInstanceUid as u64,
                3,
            ),
            Transport::WebSocket,
            Some(7),
        )
        .uid
    })
    .await
    .expect("rekey")
    .expect("carried under its new identity");
    assert_ne!(rekeyed, uid);
    assert!(
        eventually(Duration::from_secs(10), || view_of(&b, uid).is_none()).await,
        "B drops the old identity"
    );
}

/// A rollout on A to Agents forgotten through B meanwhile, before A heard of it, releases to the
/// others alone: the press counts only the Agents it wrote an assignment for, a rollout to one
/// forgotten Agent answers that it is unknown, and A holds neither any more.
/// Verifies: ADR-0030, ADR-0033
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rollout_counts_only_the_agents_it_released_to() {
    use fleet_server::fleet::{AppState, RolloutError, Transport};
    use opamp::proto::{AgentDescription, AnyValue, KeyValue};
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start_deaf(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = AppState::on(&records(&schema)).expect("state");
    let (kept, forgotten, named) = (
        InstanceUid([0x71; 16]),
        InstanceUid([0x72; 16]),
        InstanceUid([0x73; 16]),
    );
    let report = |uid: InstanceUid| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::AcceptsRemoteConfig as u64,
        agent_description: Some(AgentDescription {
            identifying_attributes: vec![KeyValue {
                key: "service.name".to_string(),
                value: Some(AnyValue {
                    value: Some(opamp::proto::any_value::Value::StringValue(
                        "otelcol".to_string(),
                    )),
                }),
            }],
            non_identifying_attributes: Vec::new(),
        }),
        ..Default::default()
    };
    let on_a = a.state.clone();
    let (to_one, released) = tokio::task::spawn_blocking(move || {
        for uid in [kept, forgotten, named] {
            on_a.process(report(uid), Transport::Http, None);
        }
        on_a.save_configuration(
            "collector",
            fleet_server::configs::Revision {
                body: "receivers: {}".to_string(),
                role: String::new(),
                service_name: "otelcol".to_string(),
            },
        )
        .expect("save");
        on_a.deployment_store()
            .expect("deployments")
            .put(
                "stable",
                std::collections::BTreeMap::from([(
                    "service.name".to_string(),
                    "otelcol".to_string(),
                )]),
            )
            .expect("deployment");
        on_a.put_deployment_configuration("stable", "collector")
            .expect("hold it");
        // Silent past any budget, and forgotten through B.
        schema.sql("UPDATE agents SET last_seen_ms = last_seen_ms - 120000");
        for uid in [forgotten, named] {
            assert!(b.forget_agent(&uid).is_ok(), "B forgets {uid}");
        }
        let to_one = on_a.rollout_to_agent(&named, None);
        (
            to_one,
            on_a.rollout_deployment("stable")
                .map_err(|e| format!("{e:?}")),
        )
    })
    .await
    .expect("roll out");
    assert_eq!(released, Ok(1), "only the Agent still stored is counted");
    assert!(matches!(to_one, Err(RolloutError::UnknownAgent)));
    for uid in [forgotten, named] {
        assert!(view_of(&a, uid).is_none(), "A holds {uid} no more");
    }
    assert!(view_of(&a, kept).is_some_and(|view| view.assigned_deployment == "stable"));
}

/// The shutdown flush of A writes only the Agents A heard from: an Agent B carries, which A
/// only loaded, keeps what B wrote of it, and an Agent A heard from keeps a `last_seen_ms`
/// another instance wrote later.
/// Verifies: ADR-0030
#[test]
fn the_shutdown_flush_writes_only_the_agents_this_instance_heard_from() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let (carried_by_b, heard_by_a) = (InstanceUid([0x81; 16]), InstanceUid([0x82; 16]));
    let report = |uid: InstanceUid, sequence_num: u64, healthy: bool| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::ReportsHealth as u64,
        health: Some(opamp::proto::ComponentHealth {
            healthy,
            ..Default::default()
        }),
        ..Default::default()
    };
    let b = AppState::on(&records(&schema)).expect("state");
    b.process(report(carried_by_b, 1, true), Transport::Http, None);
    // A loads the record at startup, and B writes the Agent's next report.
    let a = AppState::on(&records(&schema)).expect("state");
    b.process(report(carried_by_b, 2, false), Transport::Http, None);
    a.process(report(heard_by_a, 1, true), Transport::Http, None);
    // Another instance wrote a later presence of the Agent A heard from.
    schema.sql(&format!(
        "UPDATE agents SET last_seen_ms = last_seen_ms + 600000 WHERE instance_uid = '{heard_by_a}'"
    ));
    let column = |column: &str, uid: InstanceUid| {
        schema.query(&format!(
            "SELECT {column}::text FROM agents WHERE instance_uid = '{uid}'"
        ))
    };
    let (sequence, last_seen) = (
        column("sequence_num", carried_by_b),
        column("last_seen_ms", heard_by_a),
    );

    a.flush_agents();
    assert_eq!(
        column("sequence_num", carried_by_b),
        sequence,
        "what B wrote stays"
    );
    assert_eq!(
        column("last_seen_ms", heard_by_a),
        last_seen,
        "last_seen_ms does not move back"
    );
}

/// The shutdown flush of A leaves an Agent A heard from, and that has moved to B since, as B
/// wrote it: B holds its connection, so A writes nothing of it.
/// Verifies: ADR-0030
#[test]
fn the_shutdown_flush_leaves_an_agent_that_moved_to_another_instance_as_that_one_wrote_it() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x83; 16]);
    let report = |sequence_num: u64, healthy: bool| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::ReportsHealth as u64,
        health: Some(opamp::proto::ComponentHealth {
            healthy,
            ..Default::default()
        }),
        ..Default::default()
    };
    let [a, b] = [
        "00000000-0000-7000-8000-00000000000a",
        "00000000-0000-7000-8000-00000000000b",
    ]
    .map(|lease| {
        let state = AppState::on(&records(&schema)).expect("state");
        state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
        state
    });
    a.process(report(1, true), Transport::WebSocket, Some(7));
    // The Agent reconnects to B, which writes that it holds it, and its next report.
    b.process(report(2, false), Transport::WebSocket, Some(9));
    let stored = || {
        schema.query(&format!(
            "SELECT holder::text || ' ' || sequence_num FROM agents WHERE instance_uid = '{uid}'"
        ))
    };
    assert_eq!(stored(), ["00000000-0000-7000-8000-00000000000b 2"]);

    a.flush_agents();
    assert_eq!(
        stored(),
        ["00000000-0000-7000-8000-00000000000b 2"],
        "what B wrote stays"
    );
}

/// The shutdown flush of A leaves an Agent A heard from, that moved to B since and disconnected
/// there, as B wrote it: no instance holds it any more, and B took it after A did, though B's
/// clock runs behind A's.
/// Verifies: ADR-0030
#[test]
fn the_shutdown_flush_leaves_an_agent_that_moved_away_and_disconnected_as_last_written() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x84; 16]);
    let report = |sequence_num: u64, healthy: bool, goodbye: bool| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::ReportsHealth as u64,
        health: Some(opamp::proto::ComponentHealth {
            healthy,
            ..Default::default()
        }),
        agent_disconnect: goodbye.then(opamp::proto::AgentDisconnect::default),
        ..Default::default()
    };
    let (lease_a, lease_b) = (
        "00000000-0000-7000-8000-00000000000a",
        "00000000-0000-7000-8000-00000000000b",
    );
    let [a, b] = [lease_a, lease_b].map(|lease| {
        let state = AppState::on(&records(&schema)).expect("state");
        state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
        state
    });
    a.process(report(1, true, false), Transport::WebSocket, Some(7));
    // The Agent reconnects to B, reports there, and says goodbye; B writes the disconnect.
    b.process(report(2, false, false), Transport::WebSocket, Some(9));
    b.process(report(3, false, true), Transport::WebSocket, Some(9));
    b.write_presence(lease_b).expect("B's batch");
    // B's clock runs a minute behind A's.
    schema.sql(&format!(
        "UPDATE agents SET last_seen_ms = last_seen_ms - 60000 WHERE instance_uid = '{uid}'"
    ));
    let stored = || {
        schema.query(&format!(
            "SELECT coalesce(holder::text, 'none') || ' ' || sequence_num FROM agents \
             WHERE instance_uid = '{uid}'"
        ))
    };
    assert_eq!(stored(), ["none 2"]);

    a.flush_agents();
    assert_eq!(stored(), ["none 2"], "what B wrote stays");
}

/// A's presence batch writes the disconnect of an Agent A took back from B, whose clock runs
/// ahead of A's: the row still holds the count of takes A was answered, whatever `last_seen_ms` B
/// stamped it with.
/// Verifies: ADR-0030, ADR-0025
#[test]
fn a_disconnect_is_written_after_an_instance_whose_clock_runs_ahead_held_the_agent() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x85; 16]);
    let report = |sequence_num: u64, goodbye: bool| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        agent_disconnect: goodbye.then(opamp::proto::AgentDisconnect::default),
        ..Default::default()
    };
    let (lease_a, lease_b) = (
        "00000000-0000-7000-8000-00000000000a",
        "00000000-0000-7000-8000-00000000000b",
    );
    let [a, b] = [lease_a, lease_b].map(|lease| {
        let state = AppState::on(&records(&schema)).expect("state");
        state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
        state
    });
    a.process(report(1, false), Transport::WebSocket, Some(7));
    b.process(report(2, false), Transport::WebSocket, Some(9));
    // B's clock runs a minute and a half ahead of A's.
    schema.sql(&format!(
        "UPDATE agents SET last_seen_ms = last_seen_ms + 90000 WHERE instance_uid = '{uid}'"
    ));
    // A's first connection closes; the Agent reconnects to A, which takes it back, and says
    // goodbye.
    a.mark_disconnected(&[uid], 7);
    a.process(report(3, false), Transport::WebSocket, Some(8));
    a.process(report(4, true), Transport::WebSocket, Some(8));
    a.write_presence(lease_a).expect("A's batch");
    assert_eq!(
        schema.query(&format!(
            "SELECT coalesce(holder::text, 'none') || ' ' || connected FROM agents \
             WHERE instance_uid = '{uid}'"
        )),
        ["none false"]
    );
}

/// A presence batch that names A anew, over an Agent B took and disconnected, counts as a take:
/// A's disconnect is written under the count it answered, the shutdown flush of B, which took the
/// Agent before, leaves what A wrote, and that of A, which disconnected the Agent last, writes it.
/// Verifies: ADR-0030, ADR-0025
#[test]
fn a_batch_that_names_its_instance_anew_counts_as_a_take() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x86; 16]);
    let report = |sequence_num: u64, healthy: bool, goodbye: bool| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::ReportsHealth as u64,
        health: Some(opamp::proto::ComponentHealth {
            healthy,
            ..Default::default()
        }),
        agent_disconnect: goodbye.then(opamp::proto::AgentDisconnect::default),
        ..Default::default()
    };
    let (lease_a, lease_b) = (
        "00000000-0000-7000-8000-00000000000a",
        "00000000-0000-7000-8000-00000000000b",
    );
    let a = AppState::on(&records(&schema)).expect("state");
    let b = AppState::on(&records(&schema)).expect("state");
    b.serve_under(tokio::sync::watch::channel(Some(lease_b.to_string())).1);
    // A, still without a lease, takes nothing at the connecting report.
    a.process(report(1, true, false), Transport::WebSocket, Some(7));
    b.process(report(2, false, false), Transport::WebSocket, Some(9));
    b.process(report(3, false, true), Transport::WebSocket, Some(9));
    b.write_presence(lease_b).expect("B's batch");
    a.serve_under(tokio::sync::watch::channel(Some(lease_a.to_string())).1);
    a.write_presence(lease_a).expect("A's batch");
    let stored = || {
        schema.query(&format!(
            "SELECT coalesce(holder::text, 'none') || ' ' || connected || ' ' || sequence_num \
             FROM agents WHERE instance_uid = '{uid}'"
        ))
    };
    assert_eq!(stored(), [format!("{lease_a} true 2")]);
    a.process(report(4, false, true), Transport::WebSocket, Some(7));
    a.write_presence(lease_a).expect("A's disconnect");
    assert_eq!(stored(), ["none false 4"], "the disconnect is written");

    b.flush_agents();
    assert_eq!(stored(), ["none false 4"], "what A wrote stays");
    schema.sql(&format!(
        "UPDATE agents SET sequence_num = 0 WHERE instance_uid = '{uid}'"
    ));
    a.flush_agents();
    assert_eq!(stored(), ["none false 4"], "A's flush writes it");
}

/// An Agent forgotten after A took it several times, which connects to A again before A's next
/// batch and says goodbye, is written disconnected: the record created again counts its takes from
/// the start, and A keeps the count that take answered, not the greater one from before.
/// Verifies: ADR-0030
#[test]
fn the_goodbye_of_an_agent_forgotten_and_connected_again_is_written() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x87; 16]);
    let report = |sequence_num: u64, goodbye: bool| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        agent_disconnect: goodbye.then(opamp::proto::AgentDisconnect::default),
        ..Default::default()
    };
    let lease = "00000000-0000-7000-8000-00000000000a";
    let a = AppState::on(&records(&schema)).expect("state");
    a.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
    let stored = || {
        schema.query(&format!(
            "SELECT coalesce(holder::text, 'none') || ' ' || connected || ' ' || takes \
             FROM agents WHERE instance_uid = '{uid}'"
        ))
    };
    a.process(report(1, false), Transport::WebSocket, Some(7));
    a.mark_disconnected(&[uid], 7);
    a.process(report(2, false), Transport::WebSocket, Some(8));
    a.process(report(3, true), Transport::WebSocket, Some(8));
    a.write_presence(lease).expect("A's batch");
    assert_eq!(stored(), ["none false 2"]);
    assert!(a.forget_agent(&uid).is_ok(), "forgotten");
    assert!(stored().is_empty());

    a.process(report(1, false), Transport::WebSocket, Some(9));
    a.process(report(2, true), Transport::WebSocket, Some(9));
    a.write_presence(lease).expect("A's batch");
    assert_eq!(stored(), ["none false 1"], "the goodbye is written");
}

/// An Agent forgotten through B while A still holds it, created again through B and then
/// connected to A, has its goodbye written by A: A keeps the count its take answered, though it
/// counted more takes of the record before it was forgotten.
/// Verifies: ADR-0030
#[test]
fn the_goodbye_of_an_agent_forgotten_through_another_instance_is_written_where_it_connects_again() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x89; 16]);
    let report = |sequence_num: u64, goodbye: bool| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        agent_disconnect: goodbye.then(opamp::proto::AgentDisconnect::default),
        ..Default::default()
    };
    let (lease_a, lease_b) = (
        "00000000-0000-7000-8000-00000000000a",
        "00000000-0000-7000-8000-00000000000b",
    );
    let [a, b] = [lease_a, lease_b].map(|lease| {
        let state = AppState::on(&records(&schema)).expect("state");
        state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
        state
    });
    let stored = || {
        schema.query(&format!(
            "SELECT coalesce(holder::text, 'none') || ' ' || connected || ' ' || takes \
             FROM agents WHERE instance_uid = '{uid}'"
        ))
    };
    // A takes the Agent three times, and writes its goodbye.
    for (conn, sequence_num) in [(7, 1), (8, 2), (9, 3)] {
        a.process(
            report(sequence_num, false),
            Transport::WebSocket,
            Some(conn),
        );
        a.mark_disconnected(&[uid], conn);
    }
    a.write_presence(lease_a).expect("A's batch");
    assert_eq!(stored(), ["none false 3"]);
    // B forgets it; A has not heard. The Agent comes back through B, then moves to A.
    assert!(b.forget_agent(&uid).is_ok(), "forgotten through B");
    b.process(report(1, false), Transport::WebSocket, Some(5));
    a.process(report(2, false), Transport::WebSocket, Some(10));
    assert_eq!(stored(), [format!("{lease_a} true 2")]);
    a.process(report(3, true), Transport::WebSocket, Some(10));
    a.write_presence(lease_a).expect("A's batch");
    assert_eq!(stored(), ["none false 2"], "the goodbye is written");
}

/// The take that commits last holds the Agent, and the instance it displaced takes it back at
/// the Agent's next report: A's take for the Agent's old connection commits after B's for its
/// new one, B stops carrying it on hearing of the take, A writes the disconnect when the old
/// connection closes, and the Agent's next report on B makes B its holder again.
/// Verifies: ADR-0025
#[test]
fn an_instance_displaced_by_a_take_that_commits_later_takes_the_agent_back_at_its_next_report() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x88; 16]);
    let report = |sequence_num: u64| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    };
    let (lease_a, lease_b) = (
        "00000000-0000-7000-8000-00000000000a",
        "00000000-0000-7000-8000-00000000000b",
    );
    let [a, b] = [lease_a, lease_b].map(|lease| {
        let state = AppState::on(&records(&schema)).expect("state");
        state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
        state
    });
    let stored = || {
        schema.query(&format!(
            "SELECT coalesce(holder::text, 'none') || ' ' || connected FROM agents \
             WHERE instance_uid = '{uid}'"
        ))
    };
    let carried = |state: &AppState| {
        state
            .snapshot()
            .into_iter()
            .find(|view| view.instance_uid == uid.to_string())
            .is_some_and(|view| view.connected)
    };
    // B takes the Agent on its new connection; A's take for the old one commits after it.
    b.process(report(2), Transport::WebSocket, Some(9));
    a.process(report(1), Transport::WebSocket, Some(7));
    assert_eq!(stored(), [format!("{lease_a} true")]);
    b.release_taken(None).expect("B hears of the take");
    assert!(!carried(&b), "B stops carrying it");
    a.mark_disconnected(&[uid], 7);
    a.write_presence(lease_a).expect("A's batch");
    assert_eq!(stored(), ["none false"]);

    b.process(report(3), Transport::WebSocket, Some(9));
    assert_eq!(stored(), [format!("{lease_b} true")], "B holds it again");
    assert!(carried(&b), "and carries it");
}

/// An Agent that has just connected to A is not forgotten through B before A's next presence
/// batch: the report that connects it writes that A holds it, at once.
/// Verifies: ADR-0030
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_just_connected_is_not_forgotten_through_another_instance() {
    use fleet_server::fleet::{AppState, ForgetError};
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Arc::new(AppState::on(&records(&schema)).expect("state"));
    let (cert, key) = pki.issue("edge-01");
    let uid = InstanceUid([0x91; 16]);
    let attributes: Attributes<'_> = (&[("service.name", "otelcol")], &[]);
    let mut session = websocket(&a, &pki, &cert, &key).await;
    report_as(&mut session, uid, 1, 0, attributes, true).await;
    assert_eq!(
        schema.query(&format!(
            "SELECT holder::text FROM agents WHERE instance_uid = '{uid}' AND connected"
        )),
        [a.instance_id().expect("a lease")],
        "A wrote that it holds the Agent"
    );
    let forgotten = tokio::task::spawn_blocking(move || b.forget_agent(&uid))
        .await
        .expect("forget");
    assert!(
        matches!(forgotten, Err(ForgetError::StillReporting)),
        "B sees A holds it"
    );
}

/// An Agent carried by A whose connection moves to B is held by B: B takes it and announces it, A
/// reads the holder again and stops carrying it, though its old connection has not closed yet.
/// When that connection closes and A writes its next batch, the row still names B as the holder,
/// connected, and B still carries it.
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_whose_connection_moves_is_held_by_the_new_instance_and_no_longer_by_the_old() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (a_id, b_id) = (
        a.instance_id().expect("a lease"),
        b.instance_id().expect("a lease"),
    );
    let (cert, key) = pki.issue("edge-01");
    let uid = InstanceUid([0x95; 16]);
    let attributes: Attributes<'_> = (&[("service.name", "otelcol")], &[]);
    let presence = || {
        schema.query(&format!(
            "SELECT holder::text || ' ' || connected FROM agents WHERE instance_uid = '{uid}'"
        ))
    };
    let mut on_a = websocket(&a, &pki, &cert, &key).await;
    report_as(&mut on_a, uid, 1, 0, attributes, true).await;
    assert_eq!(presence(), [format!("{a_id} true")]);

    let mut on_b = websocket(&b, &pki, &cert, &key).await;
    report_as(&mut on_b, uid, 2, 0, attributes, true).await;
    assert_eq!(presence(), [format!("{b_id} true")], "B took it");
    assert!(
        eventually(Duration::from_secs(10), || {
            view_of(&a, uid).is_some_and(|view| !view.connected)
        })
        .await,
        "A stops carrying it"
    );

    drop(on_a);
    let state = a.state.clone();
    let holder = a_id.clone();
    tokio::task::spawn_blocking(move || state.write_presence(&holder))
        .await
        .expect("batch")
        .expect("A's batch");
    assert_eq!(presence(), [format!("{b_id} true")], "A's batch leaves it");
    assert!(view_of(&b, uid).is_some_and(|view| view.connected));
    report_as(&mut on_b, uid, 3, 0, attributes, true).await;
    assert_eq!(presence(), [format!("{b_id} true")]);
}

/// Of two instances that take one Agent at once, each hearing of the other's take only after its
/// own, only the one the row no longer names stops carrying it: each reads the holder again
/// rather than drop the Agent on hearing of a take.
/// Verifies: ADR-0025
#[test]
fn of_two_instances_that_take_one_agent_at_once_only_the_one_the_row_names_carries_it() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x96; 16]);
    let report = |sequence_num: u64| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    };
    let (lease_a, lease_b) = (
        "00000000-0000-7000-8000-00000000000a",
        "00000000-0000-7000-8000-00000000000b",
    );
    let [a, b] = [lease_a, lease_b].map(|lease| {
        let state = AppState::on(&records(&schema)).expect("state");
        state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
        state
    });
    a.process(report(1), Transport::WebSocket, Some(7));
    b.process(report(2), Transport::WebSocket, Some(9));
    let named = std::collections::HashSet::from([uid]);
    a.release_taken(Some(&named)).expect("A hears of B's take");
    b.release_taken(Some(&named)).expect("B hears of A's take");
    let carried = |state: &AppState| {
        state
            .snapshot()
            .into_iter()
            .any(|view| view.instance_uid == uid.to_string() && view.connected)
    };
    assert!(!carried(&a), "the row names B");
    assert!(carried(&b), "B keeps it");
}

/// An Agent whose holder changes eleven times within a minute, as two connections of one
/// `instance_uid` on two instances take it in turn, is logged as a likely duplicate once, when
/// the eleventh change lands, and not before.
/// Verifies: ADR-0025
#[test]
fn an_agent_whose_holder_changes_eleven_times_in_a_minute_is_logged_once_as_a_likely_duplicate() {
    use fleet_server::fleet::{AppState, Transport};
    let Some(schema) = Schema::named() else {
        return;
    };
    let uid = InstanceUid([0x97; 16]);
    let report = |sequence_num: u64| AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    };
    let [a, b] = [
        "00000000-0000-7000-8000-00000000000a",
        "00000000-0000-7000-8000-00000000000b",
    ]
    .map(|lease| {
        let state = AppState::on(&records(&schema)).expect("state");
        state.serve_under(tokio::sync::watch::channel(Some(lease.to_string())).1);
        state
    });
    let lines = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let writer = lines.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || Collect(writer.clone()))
        .finish();
    let named = std::collections::HashSet::from([uid]);
    let likely = || {
        String::from_utf8_lossy(&lines.lock().expect("lines"))
            .lines()
            .filter(|line| line.contains("likely a duplicate") && line.contains(&uid.to_string()))
            .count()
    };
    tracing::subscriber::with_default(subscriber, || {
        a.process(report(1), Transport::WebSocket, Some(7));
        let mut sequence_num = 1;
        for change in 1..=11 {
            // The instance the row no longer names hears of the take, and the duplicate's next
            // report on the other connection takes it back.
            let (from, to, conn) = if change % 2 == 1 {
                (&a, &b, 9)
            } else {
                (&b, &a, 7)
            };
            sequence_num += 1;
            to.release_taken(Some(&named)).expect("hears of the take");
            to.process(report(sequence_num), Transport::WebSocket, Some(conn));
            from.release_taken(Some(&named)).expect("hears of the take");
            assert_eq!(likely(), usize::from(change == 11), "after change {change}");
        }
    });
}

/// A first report of `uid`, framed, its answer not awaited: what a peer sends when it connects.
fn first_report(uid: InstanceUid, sequence_num: u64) -> tokio_tungstenite::tungstenite::Message {
    use opamp::proto::{AgentDescription, AnyValue, KeyValue};
    let report = AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        agent_description: Some(AgentDescription {
            identifying_attributes: vec![KeyValue {
                key: "service.name".to_string(),
                value: Some(AnyValue {
                    value: Some(opamp::proto::any_value::Value::StringValue(
                        "otelcol".to_string(),
                    )),
                }),
            }],
            non_identifying_attributes: Vec::new(),
        }),
        ..Default::default()
    };
    tokio_tungstenite::tungstenite::Message::Binary(
        opamp::frame::encode_within(&report, usize::MAX)
            .expect("frame")
            .into(),
    )
}

/// The identities the next `count` answers on `session` assign, each within ten seconds.
async fn assigned(session: &mut Socket, count: usize) -> Vec<Option<Vec<u8>>> {
    let mut assigned = Vec::new();
    for _ in 0..count {
        let reply = next_message(session, Duration::from_secs(10))
            .await
            .expect("an answer to each report");
        assert!(
            reply.error_response.is_none(),
            "each report is taken up: {:?}",
            reply.error_response
        );
        assigned.push(
            reply
                .agent_identification
                .map(|identification| identification.new_instance_uid),
        );
    }
    assigned
}

/// A peer of ten Agents whose first reports wait for the database longer than its heartbeat
/// interval gives its connection up and comes back on a new one with the same certificate, while
/// the Server still handles the old one's reports: its Agents keep their identities, one record
/// each, and the newest connection carries them. A report or a goodbye the older connection
/// delivers after that does not take its Agent back or end it, so the older connection's close
/// leaves every Agent connected.
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_whose_first_reports_wait_keeps_its_agents_identities_on_its_new_connection() {
    use futures_util::{SinkExt, StreamExt};
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (cert, key) = pki.issue_to_host("edge-36");
    let uids: Vec<InstanceUid> = (0..10u8)
        .map(|i| {
            let mut uid = [0x36; 16];
            uid[15] = i;
            InstanceUid(uid)
        })
        .collect();

    // Every report's first read of the store waits while the agents' table is locked.
    let held = schema.hold("LOCK TABLE agents IN ACCESS EXCLUSIVE MODE");
    let mut old = websocket(&a, &pki, &cert, &key).await;
    for uid in &uids {
        old.send(first_report(*uid, 1)).await.expect("send");
    }
    let waits = || {
        schema.query(
            "SELECT count(*) FROM pg_locks WHERE relation = 'agents'::regclass AND NOT granted",
        ) != ["0"]
    };
    assert!(
        eventually(Duration::from_secs(10), waits).await,
        "the old connection's first report waits for the database"
    );
    // Its peer heard nothing for an interval, gave the connection up and comes back on a new one.
    let mut new = websocket(&a, &pki, &cert, &key).await;
    for uid in &uids {
        new.send(first_report(*uid, 2)).await.expect("send");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    drop(held);

    assert_eq!(
        assigned(&mut new, uids.len()).await,
        vec![None; uids.len()],
        "no Agent on the new connection is re-keyed"
    );
    assert_eq!(
        assigned(&mut old, uids.len()).await,
        vec![None; uids.len()],
        "the old connection's first reports assign nothing either"
    );
    // A report the old connection delivers late does not take its Agent back from the new one,
    // nor writes a take.
    let takes = || {
        schema.query(&format!(
            "SELECT takes FROM agents WHERE instance_uid = '{}'",
            uids[0]
        ))
    };
    let before = takes();
    old.send(first_report(uids[0], 3)).await.expect("send");
    assert_eq!(assigned(&mut old, 1).await, vec![None]);
    assert_eq!(takes(), before, "the late report writes no take");
    // Nor does a goodbye it delivers late end its Agent's connection on the new one.
    let goodbye = {
        let mut report = AgentToServer {
            instance_uid: uids[1].as_bytes().to_vec(),
            sequence_num: 3,
            capabilities: AgentCapabilities::ReportsStatus as u64,
            ..Default::default()
        };
        report.agent_disconnect = Some(opamp::proto::AgentDisconnect {});
        tokio_tungstenite::tungstenite::Message::Binary(
            opamp::frame::encode_within(&report, usize::MAX)
                .expect("frame")
                .into(),
        )
    };
    old.send(goodbye).await.expect("send");
    assert_eq!(assigned(&mut old, 1).await, vec![None]);
    old.close(None).await.expect("close");
    while let Ok(Some(Ok(_))) = tokio::time::timeout(Duration::from_secs(5), old.next()).await {}
    tokio::time::sleep(Duration::from_secs(1)).await;

    for uid in &uids {
        assert!(
            view_of(&a, *uid).is_some_and(|view| view.connected),
            "{uid} is carried by the new connection"
        );
    }
    assert_eq!(a.state.snapshot().len(), uids.len(), "one record an Agent");
    assert_eq!(
        schema.query("SELECT count(*) FROM agents"),
        [uids.len().to_string()]
    );
}

/// A duplicate `instance_uid` on two live connections of different peers is re-keyed as before:
/// on two Gateways' connections, whose certificates bind no Agent, and on two connections that
/// present no host, by the duplicate detection; on two hosts' connections by the binding of the
/// identity to the host that reported it first. The incumbent keeps its identity each time.
/// Verifies: ADR-0025, ADR-0040
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_duplicate_on_live_connections_of_different_peers_is_rekeyed() {
    use futures_util::SinkExt;
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let gateways = [pki.issue_to_host("gw-a"), pki.issue_to_host("gw-b")];
    let unnamed = pki.issue("no-host");
    let hosts = [pki.issue_to_host("host-a"), pki.issue_to_host("host-b")];
    let mut gateway_sessions = Vec::new();
    for (cert, key) in &gateways {
        let mut session = websocket(&a, &pki, cert, key).await;
        exchange(&mut session).await;
        gateway_sessions.push(session);
    }
    for host in ["gw-a", "gw-b"] {
        assert!(a.revocations.set_gateway(host, true).expect("mark"));
    }
    let pairs = [
        (
            "two Gateways",
            gateway_sessions.remove(0),
            gateway_sessions.remove(0),
        ),
        (
            "two connections without a host",
            websocket(&a, &pki, &unnamed.0, &unnamed.1).await,
            websocket(&a, &pki, &unnamed.0, &unnamed.1).await,
        ),
        (
            "two hosts",
            websocket(&a, &pki, &hosts[0].0, &hosts[0].1).await,
            websocket(&a, &pki, &hosts[1].0, &hosts[1].1).await,
        ),
    ];
    for (i, (case, mut first, mut second)) in pairs.into_iter().enumerate() {
        let uid = InstanceUid([0x60 + u8::try_from(i).expect("few"); 16]);
        first.send(first_report(uid, 1)).await.expect("send");
        assert_eq!(assigned(&mut first, 1).await, vec![None], "{case}");
        second.send(first_report(uid, 1)).await.expect("send");
        let new = assigned(&mut second, 1).await.remove(0);
        assert!(
            new.as_ref().is_some_and(|new| new != uid.as_bytes()),
            "{case}: the second is re-keyed"
        );
        assert!(
            view_of(&a, uid).is_some_and(|view| view.connected),
            "{case}: the incumbent keeps its identity"
        );
        drop((first, second));
    }
}

/// A log line sink for a test's own subscriber.
#[derive(Clone)]
struct Collect(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Collect {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lines").extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A layer-4 balancer in front of the Agent plane: each new connection goes, its bytes unread, to
/// the target the test names, or to the next target that takes it when that one does not.
struct Balancer {
    address: SocketAddr,
    target: Arc<std::sync::Mutex<usize>>,
}

impl Balancer {
    async fn to(targets: Vec<SocketAddr>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("addr");
        let target = Arc::new(std::sync::Mutex::new(0usize));
        let chosen = target.clone();
        tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let first = *chosen.lock().expect("target");
                let targets = targets.clone();
                tokio::spawn(async move {
                    for i in 0..targets.len() {
                        let to = targets[(first + i) % targets.len()];
                        if let Ok(mut outbound) = tokio::net::TcpStream::connect(to).await {
                            let _ =
                                tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                            return;
                        }
                    }
                });
            }
        });
        Balancer { address, target }
    }

    /// Sends the next connections to target `index`.
    fn send_to(&self, index: usize) {
        *self.target.lock().expect("target") = index;
    }
}

/// Signs a Gateway's certificate for a Host, registers it through `instance` and marks the Host
/// as a Gateway; the files go into `dir`, with the CA's.
fn mark_gateway(instance: &Instance, pki: &Pki, dir: &std::path::Path) {
    let path = |file: &str| dir.join(file).display().to_string();
    std::fs::write(dir.join("ca.pem"), &pki.ca_pem).expect("write");
    std::fs::write(dir.join("ca-key.pem"), &pki.ca_key_pem).expect("write");
    let signer = fleet_server::ca::ClientCa::from_config(
        &toml::from_str::<fleet_server::config::ClientCaConfig>(&format!(
            "cert_file = {:?}\nkey_file = {:?}\n",
            path("ca.pem"),
            path("ca-key.pem"),
        ))
        .expect("client_ca config"),
    )
    .expect("client ca");
    let key = KeyPair::generate().expect("key");
    let csr = CertificateParams::new(vec!["gateway".to_string()])
        .expect("params")
        .serialize_request(&key)
        .expect("csr")
        .pem()
        .expect("pem");
    let signed = signer
        .sign(
            &csr,
            "gateway-host",
            &fleet_server::revocation::SubjectValues::default(),
            fleet_server::pkcs11::UseCase::MemberCsr,
        )
        .expect("sign");
    let host = signed.facts.host.clone().expect("a host");
    instance
        .revocations
        .record(signed.facts, &[7; 16], None)
        .expect("register");
    assert!(instance.revocations.set_gateway(&host, true).expect("mark"));
    std::fs::write(dir.join("gateway.pem"), &signed.pem).expect("write");
    std::fs::write(dir.join("gateway-key.pem"), key.serialize_pem()).expect("write");
}

/// A Gateway with the certificate [`mark_gateway`] wrote into `dir`, its pool reaching the Agent
/// plane through `balancer`, its downstream listener on a port of its own.
async fn gateway(
    pki: &Pki,
    balancer: &Balancer,
    dir: &std::path::Path,
) -> (SocketAddr, tokio::sync::watch::Sender<bool>) {
    let path = |file: &str| dir.join(file).display().to_string();
    let (listener_cert, listener_key) = pki.issue("127.0.0.1");
    std::fs::write(dir.join("listener.pem"), listener_cert).expect("write");
    std::fs::write(dir.join("listener-key.pem"), listener_key).expect("write");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let downstream = listener.local_addr().expect("addr");
    let config: fleet_agent::config::ClientConfig = toml::from_str(&format!(
        r#"
        endpoint = "wss://localhost:{port}/v1/opamp"
        state_dir = {state:?}
        [tls]
        ca_file = {ca:?}
        cert_file = {cert:?}
        key_file = {key:?}
        [gateway]
        listen = "{downstream}"
        upstream_connections = 4
        [gateway.tls]
        cert_file = {listener:?}
        key_file = {listener_key:?}
        client_ca_file = {ca:?}
        "#,
        port = balancer.address.port(),
        state = path("state"),
        ca = path("ca.pem"),
        cert = path("gateway.pem"),
        key = path("gateway-key.pem"),
        listener = path("listener.pem"),
        listener_key = path("listener-key.pem"),
    ))
    .expect("gateway config");
    let (stop, shutdown) = fleet_agent::shutdown::shutdown_channel();
    tokio::spawn(async move {
        fleet_agent::gateway::run_on(Arc::new(config), listener, shutdown)
            .await
            .expect("gateway");
    });
    assert!(
        eventually(Duration::from_secs(5), || Instance::takes_connections(
            downstream
        ))
        .await
    );
    (downstream, stop)
}

/// A WebSocket to the Gateway at `downstream`, presenting `cert`.
async fn behind(downstream: SocketAddr, pki: &Pki, cert: &str, key: &str) -> Socket {
    let config = opamp::tls::client_builder()
        .with_root_certificates(opamp::tls::root_store(pki.ca_pem.as_bytes()).expect("roots"))
        .with_client_auth_cert(
            opamp::tls::certificates(cert.as_bytes()).expect("cert"),
            opamp::tls::private_key(key.as_bytes()).expect("key"),
        )
        .expect("client auth");
    tokio_tungstenite::connect_async_tls_with_config(
        format!("wss://{downstream}/v1/opamp"),
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(Arc::new(config))),
    )
    .await
    .map(|(socket, _)| socket)
    .expect("admitted by the gateway")
}

/// One report of `uid` until one is answered, each waited for a second, as an Agent's heartbeat
/// sends the next: a report the Gateway wrote to a connection the instance was closing is lost.
/// Answers the reply, and the sequence number it bore.
async fn reported(
    session: &mut Socket,
    uid: InstanceUid,
    sequence_num: &mut u64,
) -> opamp::proto::ServerToAgent {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message as Ws;
    for _ in 0..20 {
        *sequence_num += 1;
        let report = AgentToServer {
            instance_uid: uid.as_bytes().to_vec(),
            sequence_num: *sequence_num,
            capabilities: AgentCapabilities::ReportsStatus as u64,
            ..Default::default()
        };
        let framed = opamp::frame::encode_within(&report, usize::MAX).expect("frame");
        session.send(Ws::Binary(framed.into())).await.expect("send");
        if let Some(reply) = next_message(session, Duration::from_secs(1)).await {
            return reply;
        }
    }
    panic!("no report of {uid} was answered");
}

/// A Gateway whose pool connections reach A and B through a balancer keeps each downstream
/// Agent on one instance while its pool connection lives: its holder never changes. When A
/// stops, each downstream Agent A held is taken over by B once, at its next report, and stays
/// connected to the Gateway; B refuses none of the pool's connections, and the Gateway goes on
/// admitting downstream peers.
/// Verifies: ADR-0025, ADR-0016
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gateway_keeps_its_agents_on_one_instance_and_moves_those_of_a_stopped_one_once() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let mut a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let dir = tempfile::tempdir().expect("tempdir");
    // B is started once the Gateway's Host is marked, so it reads the mark at its start.
    mark_gateway(&a, &pki, dir.path());
    // A single failed admission would put the Gateway's address into back-off on B.
    let b = Instance::start_throttled(
        &pki,
        &schema,
        schema.config_on(schema.config.port),
        fleet_server::throttle::Limits {
            max_failures: 1,
            ..fleet_server::throttle::Limits::default()
        },
    )
    .await;
    let balancer = Balancer::to(vec![a.agent, b.agent]).await;
    let (downstream, _stop) = gateway(&pki, &balancer, dir.path()).await;
    let (a_id, b_id) = (
        a.instance_id().expect("a lease"),
        b.instance_id().expect("a lease"),
    );
    let uids: Vec<InstanceUid> = (1..=4u8).map(|n| InstanceUid([0xa0 + n; 16])).collect();
    let mut sessions = Vec::new();
    let mut sequence = [0u64; 4];
    // Each of the first four Agents opens a pool connection of its own: two to A, two to B.
    for (i, uid) in uids.iter().enumerate() {
        balancer.send_to(i / 2);
        let (cert, key) = pki.issue(&format!("edge-{i}"));
        let mut session = behind(downstream, &pki, &cert, &key).await;
        reported(&mut session, *uid, &mut sequence[i]).await;
        sessions.push(session);
    }
    let presence = |uid: InstanceUid| {
        schema.query(&format!(
            "SELECT holder::text || ' ' || cardinality(holder_changes) FROM agents \
             WHERE instance_uid = '{uid}'"
        ))
    };
    let holders: Vec<Vec<String>> = uids.iter().map(|uid| presence(*uid)).collect();
    assert_eq!(
        holders,
        [
            [format!("{a_id} 0")],
            [format!("{a_id} 0")],
            [format!("{b_id} 0")],
            [format!("{b_id} 0")]
        ],
        "two on each instance"
    );
    // In another order each round, so a pool that placed each report anew would move some.
    for round in 1..=3 {
        for k in 0..uids.len() {
            let i = (k + round) % uids.len();
            reported(&mut sessions[i], uids[i], &mut sequence[i]).await;
        }
    }
    let unchanged: Vec<Vec<String>> = uids.iter().map(|uid| presence(*uid)).collect();
    assert_eq!(unchanged, holders, "no holder changed while the pool lives");

    // A stops: its listener closes, its sessions end, and its lease is released.
    balancer.send_to(1);
    let _ = a.stop.take().expect("running").send(());
    assert!(
        eventually(Duration::from_secs(15), || schema.query(&format!(
            "SELECT count(*) FROM leases WHERE instance_id = '{a_id}'"
        )) == ["0"])
        .await,
        "A released its lease"
    );
    for (i, uid) in uids.iter().enumerate() {
        let reply = reported(&mut sessions[i], *uid, &mut sequence[i]).await;
        assert!(reply.error_response.is_none(), "{:?}", reply.error_response);
    }
    for (i, uid) in uids.iter().enumerate() {
        reported(&mut sessions[i], *uid, &mut sequence[i]).await;
    }
    let moved: Vec<Vec<String>> = uids.iter().map(|uid| presence(*uid)).collect();
    assert_eq!(
        moved,
        [
            [format!("{b_id} 1")],
            [format!("{b_id} 1")],
            [format!("{b_id} 0")],
            [format!("{b_id} 0")]
        ],
        "A's Agents taken over by B once each, B's left"
    );
    assert!(uids
        .iter()
        .all(|uid| view_of(&b, *uid).is_some_and(|view| view.connected)));
    let throttle = b.throttle.as_ref().expect("B's throttle");
    let gateway_address: std::net::IpAddr = "127.0.0.1".parse().expect("address");
    assert_eq!(
        throttle.retry_after(gateway_address),
        None,
        "B's throttle holds the Gateway's address in no back-off"
    );
    assert!(
        throttle.begin(gateway_address).is_some(),
        "B's throttle counted no failure of the Gateway's"
    );

    // The Gateway goes on admitting a new downstream peer.
    let (cert, key) = pki.issue("edge-late");
    let mut late = behind(downstream, &pki, &cert, &key).await;
    let mut late_sequence = 0;
    reported(&mut late, InstanceUid([0xaf; 16]), &mut late_sequence).await;
}

/// A Configuration whose revision was pinned through one instance is not deleted through another
/// that holds no such pin in memory: the deletion counts the stored assignments in its own
/// transaction, under the Configuration's row lock.
/// Verifies: ADR-0025, ADR-0033
#[test]
fn a_configuration_pinned_through_one_instance_is_not_deleted_through_another() {
    use fleet_server::agent_store::Pin;
    use fleet_server::configs::{ConfigStore, Removal, Revision};
    let Some(schema) = Schema::named() else {
        return;
    };
    store_stable(&schema);
    let open = || {
        let records = records(&schema);
        let agents = records.agents().expect("agents");
        agents.load().expect("load");
        let configs = ConfigStore::open(records.configs().expect("configs")).expect("open");
        (configs, agents)
    };
    let (a_configs, a_agents) = open();
    a_configs
        .put_saved(
            "base",
            Revision {
                body: "receivers: {}\n".to_string(),
                role: String::new(),
                service_name: "otelcol".to_string(),
            },
        )
        .expect("save");
    // B reads the Configuration before A pins it.
    let (b_configs, b_agents) = open();
    let uid = InstanceUid([0x19; 16]);
    a_agents
        .put(
            &uid,
            &fleet_server::agent_store::PersistedAgent {
                assignment: None,
                ..released("base", "")
            },
        )
        .expect("report");
    let names = std::collections::BTreeSet::from(["base".to_string()]);
    a_configs
        .pin(&names, |hashes, pinned| {
            a_agents.release(pinned, &[(uid, released("base", &hashes["base"]))])
        })
        .expect("roll out through A");
    assert_eq!(
        b_agents.pinning(Pin::Configuration("base")),
        0,
        "B holds no pin in memory"
    );
    assert_eq!(
        b_configs.delete("base").expect("B asks"),
        Removal::Pinned(1),
        "B refuses it from the stored assignment"
    );
    assert_eq!(
        schema.query("SELECT name FROM configurations"),
        ["base"],
        "the Configuration is stored"
    );
    assert!(b_configs.get("base").is_some(), "and B still holds it");
    assert_eq!(
        schema.query("SELECT count(*)::text FROM configuration_revisions"),
        ["1"],
        "with the revision the assignment pins"
    );
}

/// Whether `answer` is the refusal `refusal`, as a database adapter answers it: the database named
/// before it.
fn refused_with<T: std::fmt::Debug>(
    answer: Result<T, fleet_server::packages::PackageError>,
    refusal: &fleet_server::packages::PackageError,
) -> bool {
    let refused = answer.expect_err("refused");
    refused.refusal == refusal.refusal && refused.message.ends_with(&refusal.message)
}

/// A Package pinned through one instance is not changed or deleted through another that holds no
/// such pin: each write to it counts the stored assignments in its own transaction, under the
/// Package's row lock, and is refused, leaving the Package as stored.
/// Verifies: ADR-0025, ADR-0033
#[test]
fn a_package_pinned_through_one_instance_is_not_changed_through_another() {
    use fleet_server::packages::{PackageId, PackageStore, Platform};
    let Some(schema) = Schema::named() else {
        return;
    };
    store_stable(&schema);
    // One directory stands in for the bucket both instances share.
    let blobs = tempfile::tempdir().expect("tempdir");
    let open = || {
        PackageStore::with_backend(
            records(&schema).packages().expect("packages"),
            Box::new(fleet_server::fs::FsBlobStore::open(blobs.path()).expect("blobs")),
            fleet_server::clock::Elapsed::monotonic(),
        )
        .expect("open")
    };
    let a = open();
    let id = PackageId::new("otelcol", "1.2.3").expect("id");
    let linux = Platform::new("linux", "amd64").expect("platform");
    let windows = Platform::new("windows", "amd64").expect("platform");
    a.create(&id).expect("create");
    a.put_entry(&id, &linux, b"linux artifact".to_vec())
        .expect("an entry");
    a.put_signature(&id, &linux, vec![7; 64]).expect("signed");
    // B reads the Package before A pins it, and holds no assignment at all.
    let b = open();
    let a_agents = records(&schema).agents().expect("agents");
    a_agents.load().expect("load");
    let uid = InstanceUid([0x19; 16]);
    a_agents
        .put(
            &uid,
            &fleet_server::agent_store::PersistedAgent {
                assignment: None,
                ..released("base", "")
            },
        )
        .expect("report");
    let mut record = released("base", "");
    record.assignment = Some(fleet_server::fleet::Assignment {
        deployment: "stable".to_string(),
        package: Some(id.clone()),
        configurations: std::collections::BTreeMap::new(),
    });
    a_agents
        .release(&[], &[(uid, record)])
        .expect("roll out through A");
    let refused = fleet_server::packages::assigned(&id);
    assert!(
        refused_with(
            b.put_entry(&id, &windows, b"windows artifact".to_vec()),
            &refused
        ),
        "B adds no entry"
    );
    assert!(
        refused_with(b.put_entry(&id, &linux, b"other bytes".to_vec()), &refused),
        "B replaces none"
    );
    assert!(
        refused_with(b.put_signature(&id, &linux, vec![8; 64]), &refused),
        "B signs none"
    );
    assert!(
        refused_with(b.remove_signature(&id, &linux), &refused),
        "B takes no signature away"
    );
    assert!(
        refused_with(b.delete_entry(&id, &linux), &refused),
        "B deletes no entry"
    );
    assert!(
        refused_with(b.delete_set(&id), &fleet_server::packages::pinned(&id, 1)),
        "B deletes no Package"
    );
    assert_eq!(
        schema.query(
            "SELECT os || ' ' || encode(signature, 'hex') FROM package_entries \
             JOIN packages USING (agent_type, version)"
        ),
        [format!("linux {}", "07".repeat(64))],
        "the Package is as A stored it"
    );
}

/// A Deployment an Agent was released through on one instance is not deleted through another
/// that holds no such assignment: the deletion counts the stored assignments in its own
/// transaction, under the Deployment's row lock.
/// Verifies: ADR-0025, ADR-0033
#[test]
fn a_deployment_released_through_on_one_instance_is_not_deleted_through_another() {
    use fleet_server::agent_store::Pin;
    use fleet_server::deployments::{DeploymentError, DeploymentStore};
    let Some(schema) = Schema::named() else {
        return;
    };
    let a_agents = records(&schema).agents().expect("agents");
    a_agents.load().expect("load");
    let a = DeploymentStore::open(records(&schema).deployments().expect("deployments"))
        .expect("open A");
    let channel = std::collections::BTreeMap::from([("channel".to_string(), "stable".to_string())]);
    a.put("stable", channel).expect("save");
    // B reads the Deployment before A releases through it.
    let b_agents = records(&schema).agents().expect("agents");
    b_agents.load().expect("load");
    let b = DeploymentStore::open(records(&schema).deployments().expect("deployments"))
        .expect("open B");
    let uid = InstanceUid([0x19; 16]);
    a_agents
        .put(
            &uid,
            &fleet_server::agent_store::PersistedAgent {
                assignment: None,
                ..released("base", "")
            },
        )
        .expect("report");
    let mut record = released("base", "");
    if let Some(assignment) = record.assignment.as_mut() {
        assignment.configurations.clear();
    }
    a_agents
        .release(&[], &[(uid, record)])
        .expect("roll out through A");
    assert_eq!(
        b_agents.pinning(Pin::Deployment("stable")),
        0,
        "B holds no pin"
    );
    match b.delete("stable") {
        Err(DeploymentError::Conflict(refusal)) => assert!(
            refusal.ends_with(&fleet_server::deployments::pinned("stable", 1)),
            "{refusal}"
        ),
        other => panic!("B deleted it: {other:?}"),
    }
    assert_eq!(schema.query("SELECT name FROM deployments"), ["stable"]);
    assert!(b.get("stable").is_some(), "and B still holds it");
}

/// An `otelcol` Agent on linux/amd64 that accepts packages.
const PACKAGED: Attributes<'static> = (
    &[("service.name", "otelcol")],
    &[("os.type", "linux"), ("host.arch", "amd64")],
);

/// The capabilities of an Agent that is offered packages and reports what it holds.
const TAKES_PACKAGES: u64 =
    AgentCapabilities::AcceptsPackages as u64 | AgentCapabilities::ReportsPackageStatuses as u64;

/// Through `instance`: a signed Package `otelcol@1.0.0` for linux/amd64 in a Deployment `stable`
/// that selects `otelcol`, rolled out. Answers how many Agents it released to.
async fn release_a_package(instance: &Instance) -> usize {
    let state = instance.state.clone();
    tokio::task::spawn_blocking(move || {
        let id = fleet_server::packages::PackageId::new("otelcol", "1.0.0")?;
        let linux = fleet_server::packages::Platform::new("linux", "amd64")?;
        state.create_package_set(&id)?;
        state.put_package_entry(&id, &linux, &mut &b"v1"[..])?;
        state.put_package_entry_signature(&id, &linux, vec![1; 64])?;
        state
            .deployment_store()
            .expect("deployments")
            .put(
                "stable",
                std::collections::BTreeMap::from([(
                    "service.name".to_string(),
                    "otelcol".to_string(),
                )]),
            )
            .map_err(|e| format!("{e:?}"))?;
        state
            .put_deployment_package("stable", &id, true)
            .map_err(|e| format!("{e:?}"))?;
        state
            .rollout_deployment("stable")
            .map_err(|e| format!("{e:?}"))
    })
    .await
    .expect("release")
    .expect("release a package")
}

/// A Package that a release through B pinned, and that a later release through A moved the Agent
/// off, is deleted through B, which does not carry the Agent: B's own count of the pin is stale,
/// and the deletion decides from the stored assignments, not from it.
/// Verifies: ADR-0025, ADR-0033
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_package_released_away_through_one_instance_is_deleted_through_another() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let blobs = tempfile::tempdir().expect("blobs");
    let a = Instance::start_on_blobs(
        &pki,
        &schema,
        schema.config_on(schema.config.port),
        blobs.path(),
    )
    .await;
    let b = Instance::start_on_blobs(
        &pki,
        &schema,
        schema.config_on(schema.config.port),
        blobs.path(),
    )
    .await;
    let (cert, key) = pki.issue_to_host("h1");
    let uid = InstanceUid([0x24; 16]);
    let mut session = websocket(&a, &pki, &cert, &key).await;
    report_as(&mut session, uid, 1, TAKES_PACKAGES, PACKAGED, true).await;
    assert_eq!(release_a_package(&b).await, 1, "released through B");

    let on_a = a.state.clone();
    tokio::task::spawn_blocking(move || {
        let next = fleet_server::packages::PackageId::new("otelcol", "2.0.0")?;
        let linux = fleet_server::packages::Platform::new("linux", "amd64")?;
        on_a.create_package_set(&next)?;
        on_a.put_package_entry(&next, &linux, &mut &b"v2"[..])?;
        on_a.put_package_entry_signature(&next, &linux, vec![2; 64])?;
        on_a.put_deployment_package("stable", &next, true)
            .map_err(|e| format!("{e:?}"))?;
        on_a.rollout_deployment("stable")
            .map_err(|e| format!("{e:?}"))
    })
    .await
    .expect("release")
    .expect("released 2.0.0 through A");

    let on_b = b.state.clone();
    let deleted = tokio::task::spawn_blocking(move || {
        on_b.delete_package_set(&fleet_server::packages::PackageId::new("otelcol", "1.0.0")?)
            .map_err(String::from)
    })
    .await
    .expect("delete");
    assert_eq!(
        deleted,
        Ok(true),
        "B deletes the Package no stored assignment pins"
    );
}

/// A restarted instance loads no Agent record and holds none until one reports to it, and reads
/// an Agent another instance carries from the database: the fleet list, the Hosts list and the
/// deployment reach through B name the Agent A carries with its labels, its assignment and A's
/// presence, B serves its host the artifact released to it, and once A's lease is gone the Agent
/// reads as disconnected.
/// Verifies: ADR-0030, ADR-0025, ADR-0023, ADR-0034
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_carried_by_one_instance_is_read_through_another_that_holds_none_of_it() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    // One blob store, as instances on S3 share one bucket.
    let blobs = tempfile::tempdir().expect("blobs");
    let a = Instance::start_on_blobs(
        &pki,
        &schema,
        schema.config_on(schema.config.port),
        blobs.path(),
    )
    .await;
    let (cert, key) = pki.issue_to_host("h1");
    let uid = InstanceUid([0x21; 16]);
    let mut session = websocket(&a, &pki, &cert, &key).await;
    report_as(&mut session, uid, 1, TAKES_PACKAGES, PACKAGED, true).await;
    let on_a = a.state.clone();
    tokio::task::spawn_blocking(move || {
        on_a.set_labels(
            &uid,
            std::collections::BTreeMap::from([("tier".to_string(), "edge".to_string())]),
        )
    })
    .await
    .expect("labels")
    .unwrap_or_else(|_| panic!("label on A"));
    assert_eq!(release_a_package(&a).await, 1, "A released to the Agent");

    // B starts after all of it, as an instance restarted would.
    let b = Instance::start_on_blobs(
        &pki,
        &schema,
        schema.config_on(schema.config.port),
        blobs.path(),
    )
    .await;
    assert!(b.state.snapshot().is_empty(), "B loaded no Agent record");

    let on_b = b.state.clone();
    let (views, hosts, reach) = tokio::task::spawn_blocking(move || {
        (
            on_b.fleet_view().expect("fleet"),
            on_b.host_agents().expect("hosts"),
            on_b.deployment_reach().expect("reach"),
        )
    })
    .await
    .expect("read through B");
    let view = views
        .iter()
        .find(|view| view.instance_uid == uid.to_string())
        .expect("B lists the Agent A carries");
    assert_eq!(view.labels.get("tier").map(String::as_str), Some("edge"));
    assert_eq!(view.assigned_deployment, "stable");
    assert_eq!(view.assigned_package, "otelcol@1.0.0");
    assert!(view.connected, "A's presence: it holds the connection");
    let host = hosts.get("h1").expect("B lists the Host");
    assert_eq!(host.agents.len(), 1);
    assert_eq!(host.agents[0].instance_uid, uid.to_string());
    assert!(host.agents[0].connected);
    assert_eq!(
        (reach["stable"].claiming, reach["stable"].targeted),
        (1, 0),
        "the Deployment claims the Agent, released already"
    );

    let on_b = b.state.clone();
    let offered = move || {
        let id = fleet_server::packages::PackageId::new("otelcol", "1.0.0").expect("id");
        let linux = fleet_server::packages::Platform::new("linux", "amd64").expect("platform");
        on_b.offers_artifact("h1", &id, &linux)
    };
    assert!(
        tokio::task::spawn_blocking(offered).await.expect("offered"),
        "B serves the host the artifact released to the Agent A carries"
    );
    let on_b = b.state.clone();
    let per_agent = tokio::task::spawn_blocking(move || {
        on_b.rollout_to_agent(&uid, Some("stable"))
            .map_err(|e| format!("{e:?}"))
    })
    .await
    .expect("roll out");
    assert_eq!(per_agent, Ok(()), "B rolls out to the Agent A carries");
    assert!(b.state.snapshot().is_empty(), "B still holds none of it");

    // An Agent that reports to B is what B holds.
    let other = InstanceUid([0x22; 16]);
    let (other_cert, other_key) = pki.issue_to_host("h2");
    let mut other_session = websocket(&b, &pki, &other_cert, &other_key).await;
    report_as(&mut other_session, other, 1, 0, PACKAGED, true).await;
    let held: Vec<String> = b
        .state
        .snapshot()
        .into_iter()
        .map(|view| view.instance_uid)
        .collect();
    assert_eq!(held, [other.to_string()]);

    drop(session);
    drop(a);
    let disconnected = || {
        b.state
            .fleet_view()
            .expect("fleet")
            .into_iter()
            .any(|view| view.instance_uid == uid.to_string() && !view.connected)
    };
    assert!(
        eventually(Duration::from_secs(10), disconnected).await,
        "an Agent whose holder holds no live lease reads disconnected"
    );
}

/// An Agent B carries inherits the labels of its Client Agent, which A carries: B reads the Client
/// Agent from the database when it places the Agent, once it has taken up the host register
/// another instance wrote, and follows a change of its labels made through A.
/// Verifies: ADR-0025, ADR-0030
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_inherits_the_labels_of_its_client_agent_that_another_instance_carries() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    // B hears nothing until it follows, so what it knows of the Client Agent at first is nothing.
    let mut b = Instance::start_deaf(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (cert, key) = pki.issue_to_host("h1");
    let (client, agent) = (InstanceUid([0x31; 16]), InstanceUid([0x32; 16]));
    let mut client_session = websocket(&a, &pki, &cert, &key).await;
    report_as(
        &mut client_session,
        client,
        1,
        0,
        (
            &[("service.name", "fleet-agent")],
            &[("supervisor.kind.otelcol", "true")],
        ),
        true,
    )
    .await;
    let label = |tier: &'static str| {
        let on_a = a.state.clone();
        async move {
            tokio::task::spawn_blocking(move || {
                on_a.set_labels(
                    &client,
                    std::collections::BTreeMap::from([("tier".to_string(), tier.to_string())]),
                )
                .is_ok()
            })
            .await
            .expect("labels")
        }
    };
    assert!(label("edge").await, "A labels the Client Agent");
    let mut session = websocket(&b, &pki, &cert, &key).await;
    report_as(&mut session, agent, 1, 0, PACKAGED, true).await;
    let held_by_b = b.state.clone();
    let inherits = |tier: &str| {
        held_by_b.snapshot().into_iter().any(|view| {
            view.instance_uid == agent.to_string()
                && view.inherited_labels.get("tier").map(String::as_str) == Some(tier)
                && view.client_agent == client.to_string()
        })
    };
    assert!(
        view_of(&b, agent).is_some_and(|view| view.inherited_labels.is_empty()),
        "B did not know the Client Agent was bound to the Host"
    );

    b.follow();
    assert!(
        eventually(Duration::from_secs(10), || inherits("edge")).await,
        "B placed the Agent under the Client Agent A carries once it read everything again"
    );
    assert!(label("core").await);
    assert!(
        eventually(Duration::from_secs(10), || inherits("core")).await,
        "B followed the Client Agent's labels"
    );
}

/// The list of Agents as `instance` answers it: those `filter` admits, by name, a page of `limit`
/// after `after`.
fn page_on(
    instance: &Instance,
    filter: fleet_server::listing::AgentFilter,
    after: Option<fleet_server::listing::Position>,
    limit: usize,
) -> fleet_server::fleet::AgentListing {
    instance
        .state
        .list_agents(&fleet_server::fleet::AgentQuery {
            filter,
            after,
            limit,
            ..fleet_server::fleet::AgentQuery::default()
        })
        .expect("a page")
}

/// The Instance UIDs a filter admits on `instance`.
fn holding(instance: &Instance, holder: fleet_server::listing::HolderFilter) -> Vec<String> {
    page_on(
        instance,
        fleet_server::listing::AgentFilter {
            holder: Some(holder),
            ..Default::default()
        },
        None,
        100,
    )
    .agents
    .into_iter()
    .map(|view| view.instance_uid)
    .collect()
}

/// An Agent's view names the instance that carries it and the host its lease records, read
/// through any instance; the list filters by it, `none` naming the Agents no instance with a live
/// lease holds; a page's cursor from one instance continues the list on another; and once the
/// holder's lease is gone the Agent is held by none.
/// Verifies: ADR-0031, ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agents_view_names_its_holder_and_the_list_filters_by_it() {
    use fleet_server::listing::HolderFilter;
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let (on_a, on_b) = (InstanceUid([0x41; 16]), InstanceUid([0x42; 16]));
    let (cert, key) = pki.issue_to_host("h1");
    let mut session_a = websocket(&a, &pki, &cert, &key).await;
    report_as(&mut session_a, on_a, 1, 0, PACKAGED, true).await;
    let (other_cert, other_key) = pki.issue_to_host("h2");
    let mut session_b = websocket(&b, &pki, &other_cert, &other_key).await;
    report_as(&mut session_b, on_b, 1, 0, PACKAGED, true).await;
    let (a_id, b_id) = (
        a.instance_id().expect("A's lease"),
        b.instance_id().expect("B's lease"),
    );

    let read = b.state.clone();
    let view = tokio::task::spawn_blocking(move || read.agent_view(&on_a))
        .await
        .expect("read")
        .expect("readable")
        .expect("known");
    assert_eq!(
        (view.holder.as_str(), view.holder_host.as_str()),
        (a_id.as_str(), "test-host"),
        "read through B, the Agent A carries names A"
    );
    let all = page_on(&b, Default::default(), None, 100);
    let held: std::collections::BTreeMap<String, String> = all
        .agents
        .into_iter()
        .map(|view| (view.instance_uid, view.holder))
        .collect();
    assert_eq!(held[&on_a.to_string()], a_id);
    assert_eq!(held[&on_b.to_string()], b_id);
    assert_eq!(
        holding(&b, HolderFilter::Instance(a_id.clone())),
        [on_a.to_string()]
    );
    assert_eq!(
        holding(&a, HolderFilter::Instance(b_id.clone())),
        [on_b.to_string()]
    );
    assert!(holding(&b, HolderFilter::None).is_empty(), "each is held");

    let first = page_on(&a, Default::default(), None, 1);
    let rest = page_on(&b, Default::default(), first.next.clone(), 1);
    assert_eq!((first.total, rest.total), (2, 2));
    assert!(rest.next.is_none());
    let mut both: Vec<String> = first
        .agents
        .into_iter()
        .chain(rest.agents)
        .map(|view| view.instance_uid)
        .collect();
    both.sort();
    let mut expected = vec![on_a.to_string(), on_b.to_string()];
    expected.sort();
    assert_eq!(both, expected, "A's cursor continues the list on B");

    // The same through the REST API of each instance's Operator plane.
    let operator = reqwest::Client::builder()
        .use_rustls_tls()
        .tls_certs_only([reqwest::Certificate::from_pem(pki.ca_pem.as_bytes()).expect("ca")])
        .resolve(
            "localhost",
            "127.0.0.1:0".parse::<SocketAddr>().expect("address"),
        )
        .build()
        .expect("client");
    let rest = |instance: &Instance, path: String| {
        let (operator, port) = (operator.clone(), instance.operator.port());
        async move {
            let response = operator
                .get(format!("https://localhost:{port}{path}"))
                .send()
                .await
                .expect("send");
            let status = response.status().as_u16();
            let total = response
                .headers()
                .get("x-total-count")
                .map(|v| v.to_str().expect("text").to_string());
            let next = response.headers().get("link").map(|link| {
                link.to_str()
                    .expect("text")
                    .trim_start_matches('<')
                    .split('>')
                    .next()
                    .expect("a target")
                    .to_string()
            });
            let body: serde_json::Value = response.json().await.expect("json");
            (status, total, next, body)
        }
    };
    let uids = |body: &serde_json::Value| -> Vec<String> {
        body.as_array()
            .expect("an array")
            .iter()
            .map(|agent| agent["instance_uid"].as_str().expect("uid").to_string())
            .collect()
    };
    let (status, _, _, body) = rest(&b, format!("/api/v1/agents?holder={a_id}")).await;
    assert_eq!(
        (status, uids(&body)),
        (200, vec![on_a.to_string()]),
        "{body}"
    );
    assert_eq!(body[0]["holder"], a_id.as_str());
    assert_eq!(body[0]["holder_host"], "test-host");
    let (status, total, _, body) = rest(&b, "/api/v1/agents?holder=none".to_string()).await;
    assert_eq!(
        (status, total.as_deref(), uids(&body).len()),
        (200, Some("0"), 0)
    );
    let (status, total, next, first) = rest(&a, "/api/v1/agents?limit=1".to_string()).await;
    assert_eq!((status, total.as_deref()), (200, Some("2")));
    let next = next.expect("a next page");
    let (status, _, last, rest_of) = rest(&b, next).await;
    assert_eq!((status, last), (200, None), "{rest_of}");
    let mut through_rest: Vec<String> = uids(&first).into_iter().chain(uids(&rest_of)).collect();
    through_rest.sort();
    assert_eq!(through_rest, expected, "A's Link continues on B");
    let (status, _, _, body) = rest(&b, "/api/v1/agents?q=a%00".to_string()).await;
    assert_eq!(status, 400, "a NUL is refused on PostgreSQL too: {body}");
    assert!(
        !body.to_string().contains("database"),
        "the refusal names no database: {body}"
    );

    drop(session_a);
    drop(a);
    assert!(
        eventually(Duration::from_secs(10), || {
            holding(&b, HolderFilter::None) == [on_a.to_string()]
        })
        .await,
        "once A's lease is gone none holds what it carried"
    );
    let (status, _, _, body) = rest(&b, "/api/v1/agents?holder=none".to_string()).await;
    assert_eq!(
        (status, uids(&body)),
        (200, vec![on_a.to_string()]),
        "{body}"
    );
    let read = b.state.clone();
    let view = tokio::task::spawn_blocking(move || read.agent_view(&on_a))
        .await
        .expect("read")
        .expect("readable")
        .expect("known");
    assert_eq!((view.holder.as_str(), view.holder_host.as_str()), ("", ""));
}

/// An Agent behind a Gateway that B carries inherits, in B's memory, the labels of its Client
/// Agent behind the same Gateway, which A carries: B reads the Agents that arrived over the
/// Gateway's Host through the port's `query_page` when it places the Agent, and places it again
/// when A changes that Client Agent's labels.
/// Verifies: ADR-0025, ADR-0030
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_behind_a_gateway_inherits_the_labels_of_its_client_agent_another_instance_carries(
) {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let dir = tempfile::tempdir().expect("dir");
    mark_gateway(&a, &pki, dir.path());
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let read = |file: &str| std::fs::read_to_string(dir.path().join(file)).expect("read");
    let (cert, key) = (read("gateway.pem"), read("gateway-key.pem"));
    let (client, agent) = (InstanceUid([0x51; 16]), InstanceUid([0x52; 16]));
    let mut client_session = websocket(&a, &pki, &cert, &key).await;
    report_as(
        &mut client_session,
        client,
        1,
        0,
        (
            &[("service.name", "fleet-agent")],
            &[("supervisor.kind.otelcol", "true"), ("host.name", "m1")],
        ),
        true,
    )
    .await;
    let on_a = a.state.clone();
    tokio::task::spawn_blocking(move || {
        on_a.set_labels(
            &client,
            std::collections::BTreeMap::from([("tier".to_string(), "edge".to_string())]),
        )
        .is_ok()
    })
    .await
    .expect("labels")
    .then_some(())
    .expect("A labels the Client Agent");

    let mut session = websocket(&b, &pki, &cert, &key).await;
    report_as(
        &mut session,
        agent,
        1,
        0,
        (&[("service.name", "otelcol")], &[("host.name", "m1")]),
        true,
    )
    .await;
    let inherits = |tier: &str| {
        view_of(&b, agent).is_some_and(|view| {
            view.inherited_labels.get("tier").map(String::as_str) == Some(tier)
                && view.client_agent == client.to_string()
        })
    };
    assert!(
        eventually(Duration::from_secs(10), || inherits("edge")).await,
        "B placed the Agent under the Client Agent A carries behind the Gateway: {:?}",
        view_of(&b, agent).map(|view| (view.inherited_labels, view.client_agent))
    );

    // A labels the Client Agent anew: B, which placed the Agent under it, follows.
    let on_a = a.state.clone();
    tokio::task::spawn_blocking(move || {
        on_a.set_labels(
            &client,
            std::collections::BTreeMap::from([("tier".to_string(), "core".to_string())]),
        )
        .is_ok()
    })
    .await
    .expect("labels")
    .then_some(())
    .expect("A labels the Client Agent anew");
    assert!(
        eventually(Duration::from_secs(10), || inherits("core")).await,
        "B followed the labels of the Client Agent behind the Gateway: {:?}",
        view_of(&b, agent).map(|view| view.inherited_labels)
    );
}

/// The schema of a Server two releases on: this release's migrations and one more, which names
/// 0.9.0 as the oldest Server that understands what it leaves.
fn two_releases_on() -> &'static [Migration] {
    let mut migrations = MIGRATIONS.to_vec();
    migrations.push(Migration {
        version: i32::try_from(migrations.len() + 1).expect("few"),
        oldest_server: "0.9.0",
        sql: "CREATE TABLE fleet_test_ahead (n integer)",
    });
    Box::leak(migrations.into_boxed_slice())
}

/// The upgrade from 0.8.0 itself: on the schema 0.8.0 leaves, migrations 1 to 11, with a live lease
/// as an instance of 0.8.0 takes one, writing no release, two instances of this release that start
/// at once apply migration 12 once beside it and both serve, and that lease reads 0.8.0
/// (ADR-0025
/// clause 16).
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn this_release_starts_beside_a_live_lease_of_0_8_0_and_migrates_once() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let config = || schema.config_on(schema.config.port);
    let (before, name) = (config(), schema.name.clone());
    tokio::task::spawn_blocking(move || {
        Database::open_as(
            &before,
            Exclusive::Shared,
            Some(&name),
            "0.8.0",
            &MIGRATIONS[..11],
        )
        .map(|_| ())
    })
    .await
    .expect("open")
    .expect("the schema of 0.8.0");
    // A lease as 0.8.0 takes one: no `release` column yet, and none written.
    let old = "00000000-0000-4000-8000-000000000080";
    schema.sql(&format!(
        "INSERT INTO leases (instance_id, host, must_be_alone, taken_at, renewed_at, expires_at) \
         VALUES ('{old}', 'host-0.8.0', false, now(), now(), now() + interval '1 hour')"
    ));
    let (newer, other) = tokio::join!(
        Instance::start_as(&pki, &schema, config(), "0.8.1", MIGRATIONS),
        Instance::start_as(&pki, &schema, config(), "0.8.1", MIGRATIONS),
    );
    assert_eq!(
        schema.query("SELECT count(*)::text FROM fleet_schema WHERE version = 12"),
        ["1"],
        "migration 12 applied once"
    );
    for instance in [&newer, &other] {
        assert!(instance.both_planes().0, "both serve beside 0.8.0");
    }
    assert_eq!(
        schema.query(&format!(
            "SELECT release FROM leases WHERE instance_id = '{old}' AND expires_at > now()"
        )),
        ["0.8.0"],
        "the lease of 0.8.0 reads 0.8.0"
    );
}

/// The schema of the next patch release: this release's migrations and one more, which names
/// 0.8.0, so the instances of 0.8.0 serve on what it leaves.
fn the_next_patch_release() -> &'static [Migration] {
    let mut migrations = MIGRATIONS.to_vec();
    migrations.push(Migration {
        version: i32::try_from(migrations.len() + 1).expect("few"),
        oldest_server: "0.8.0",
        sql: "CREATE TABLE fleet_test_next (n integer)",
    });
    Box::leak(migrations.into_boxed_slice())
}

/// With an instance of N on its schema, two instances of N+1 that start at once apply N+1's
/// migration once and both serve; the instances of N and N+1 both serve the Agents that report
/// to either, each reading the other's; and each lease names the release of its instance
/// (ADR-0025
/// clause 16).
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_versions_serve_side_by_side_and_the_newer_migrates_once() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let config = || schema.config_on(schema.config.port);
    let next = the_next_patch_release();
    let older = Instance::start_as(&pki, &schema, config(), "0.8.0", MIGRATIONS).await;
    assert_eq!(
        schema.query("SELECT max(version)::text FROM fleet_schema"),
        [MIGRATIONS.len().to_string()]
    );
    let (newer, other) = tokio::join!(
        Instance::start_as(&pki, &schema, config(), "0.8.1", next),
        Instance::start_as(&pki, &schema, config(), "0.8.1", next),
    );
    assert_eq!(
        schema.query(&format!(
            "SELECT count(*)::text FROM fleet_schema WHERE version = {}",
            next.len()
        )),
        ["1"],
        "migrated once"
    );
    for instance in [&older, &newer, &other] {
        assert!(instance.both_planes().0, "every instance serves");
    }
    let mut releases = schema.query("SELECT release FROM leases WHERE expires_at > now()");
    releases.sort();
    assert_eq!(
        releases,
        ["0.8.0", "0.8.1", "0.8.1"],
        "each lease names its release"
    );

    let (on_older, on_newer) = (InstanceUid([0x51; 16]), InstanceUid([0x52; 16]));
    let (cert, key) = pki.issue_to_host("h1");
    let mut session = websocket(&older, &pki, &cert, &key).await;
    report_as(&mut session, on_older, 1, 0, PACKAGED, true).await;
    let (cert, key) = pki.issue_to_host("h2");
    let mut session = websocket(&newer, &pki, &cert, &key).await;
    report_as(&mut session, on_newer, 1, 0, PACKAGED, true).await;
    for (reader, uid, carrier) in [(&newer, on_older, &older), (&older, on_newer, &newer)] {
        let state = reader.state.clone();
        let view = tokio::task::spawn_blocking(move || state.agent_view(&uid))
            .await
            .expect("read")
            .expect("readable")
            .expect("known on the other version");
        assert_eq!(
            Some(view.holder),
            carrier.instance_id(),
            "read through the other version, the Agent names its carrier"
        );
    }
}

/// An instance whose lease lapsed while a Server two releases on migrated checks the schema as
/// it takes its new lease, and stops: it takes none, keeps the refusal naming both versions, and
/// ends what its lease sends, so its planes stay closed and the Server exits
/// (ADR-0025
/// clause 16).
/// Verifies: ADR-0025
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_instance_whose_lease_lapsed_while_a_newer_server_migrated_stops_at_its_new_lease() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let target = std::net::ToSocketAddrs::to_socket_addrs(&(
        schema.config.host.as_str(),
        schema.config.port,
    ))
    .expect("resolve")
    .next()
    .expect("an address");
    let (port, frozen) = freezable_proxy(target);
    let (config, name) = (schema.config_on(port), schema.name.clone());
    let older = tokio::task::spawn_blocking(move || {
        Database::open_as(&config, Exclusive::Shared, Some(&name), "0.8.0", MIGRATIONS)
    })
    .await
    .expect("open")
    .expect("0.8.0 opens");
    let lease = {
        let older = older.clone();
        tokio::task::spawn_blocking(move || {
            fleet_server::instance::Lease::take(older, TIMES, "host-older")
        })
        .await
        .expect("take")
        .expect("a lease")
    };
    let mut held = lease.held();

    frozen.store(true, Ordering::SeqCst);
    assert!(
        eventually(TIMES.lapse + Duration::from_secs(2), || held
            .borrow()
            .is_none())
        .await,
        "the lease was not given up"
    );
    // Lapsed in the database too, so the newer Server sees no live lease and migrates.
    schema.sql("UPDATE leases SET expires_at = now() - interval '1 second'");
    let (config, name) = (schema.config_on(schema.config.port), schema.name.clone());
    tokio::task::spawn_blocking(move || {
        Database::open_as(
            &config,
            Exclusive::Shared,
            Some(&name),
            "0.10.0",
            two_releases_on(),
        )
        .map(|_| ())
    })
    .await
    .expect("open")
    .expect("the newer Server migrates");

    frozen.store(false, Ordering::SeqCst);
    let ended = tokio::time::timeout(Duration::from_secs(30), async {
        while held.changed().await.is_ok() {}
    })
    .await;
    assert!(ended.is_ok(), "the instance did not stop");
    assert!(held.borrow().is_none(), "it holds no lease");
    let refusal = older.outdated().expect("the refusal is kept");
    for named in ["needs Server 0.9.0", "this Server is 0.8.0"] {
        assert!(refusal.contains(named), "{named}: {refusal}");
    }
    assert!(
        schema
            .query("SELECT instance_id::text FROM leases WHERE expires_at > now()")
            .is_empty(),
        "no new lease"
    );
}

/// One instance's record on PostgreSQL that signs its checkpoints with `key`, every `interval`,
/// through `source`; answers the record and its lease.
async fn signing_record(
    schema: &Schema,
    key: &fleet_server::pkcs11::TokenKey,
    interval: Duration,
    source: impl FnOnce(
        Arc<Database>,
        fleet_server::instance::Held,
    ) -> Box<dyn fleet_server::checkpoint::CheckpointSource>,
) -> (
    Arc<fleet_server::audit_log::AuditLog>,
    fleet_server::instance::Lease,
    Arc<Database>,
) {
    let (config, name) = (schema.config_on(schema.config.port), schema.name.clone());
    let database = tokio::task::spawn_blocking(move || {
        Database::open_in(&config, Exclusive::Shared, Some(&name))
    })
    .await
    .expect("open")
    .expect("open the database");
    let lease = {
        let database = database.clone();
        tokio::task::spawn_blocking(move || {
            fleet_server::instance::Lease::take(database, TIMES, "test-host")
        })
        .await
        .expect("take")
        .expect("a lease")
    };
    let signing = fleet_server::audit_log::Signing {
        interval,
        ..fleet_server::audit_log::Signing::new(
            Arc::new(key.clone()),
            source(database.clone(), lease.held()),
        )
    };
    let audit = fleet_server::audit_log::AuditLog::start_signing(
        Box::new(fleet_server::postgres::audit::PgAuditStore::new(
            database.clone(),
            lease.held(),
        )),
        fleet_server::audit_log::Limits::default(),
        Arc::new(fleet_server::clock::SystemClock),
        Some((lease.held(), Some(lease.proof()))),
        None,
        Some(signing),
    )
    .expect("the record");
    (Arc::new(audit), lease, database)
}

/// The checkpoints the schema holds, by their number: the chain each is in and its line.
fn checkpoints_in(schema: &Schema) -> Vec<(String, fleet_server::checkpoint::Parsed)> {
    schema
        .query(
            "SELECT chain::text || ' ' || line FROM audit_entries \
             WHERE checkpoint_seq IS NOT NULL ORDER BY checkpoint_seq",
        )
        .into_iter()
        .map(|row| {
            let (chain, line) = row.split_once(' ').expect("chain and line");
            (
                chain.to_string(),
                fleet_server::checkpoint::parse(line).expect("a checkpoint"),
            )
        })
        .collect()
}

/// On PostgreSQL with two instances, one signs at a time, each checkpoint names both chains, and
/// the other takes over once the signer's connection ends; the record verifies against the key.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_instance_signs_at_a_time_naming_every_chain_and_another_takes_over() {
    use fleet_server::audit::Audit as _;
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let interval = Duration::from_millis(300);
    let checkpoints = |database: Arc<Database>, held| {
        Box::new(fleet_server::postgres::checkpoint::Checkpoints::new(
            database, held,
        )) as Box<dyn fleet_server::checkpoint::CheckpointSource>
    };
    let (first, first_lease, _) = signing_record(&schema, &key, interval, checkpoints).await;
    let (second, second_lease, _) = signing_record(&schema, &key, interval, checkpoints).await;
    let chains = [
        first_lease.held().borrow().clone().expect("held"),
        second_lease.held().borrow().clone().expect("held"),
    ];
    for _ in 0..3 {
        for audit in [&first, &second] {
            audit
                .record(fleet_server::audit::Entry::new("host.admitted", "admitted"))
                .expect("record");
        }
        tokio::time::sleep(interval).await;
    }
    assert!(
        eventually(Duration::from_secs(10), || checkpoints_in(&schema)
            .iter()
            .any(|(_, parsed)| parsed.heads.len() == 2))
        .await,
        "a checkpoint naming both chains"
    );
    let signed = checkpoints_in(&schema);
    let signer = signed[0].0.clone();
    assert!(
        signed.iter().all(|(chain, _)| *chain == signer),
        "one instance signs: {signed:?}"
    );
    for (n, (_, parsed)) in signed.iter().enumerate() {
        assert_eq!(parsed.checkpoint_seq, n as u64 + 1, "one per number");
    }
    let named: Vec<String> = signed
        .last()
        .expect("one")
        .1
        .heads
        .iter()
        .map(|head| head.chain.clone())
        .collect();
    assert!(
        chains.iter().all(|chain| named.contains(chain)),
        "{named:?}"
    );

    // The signer stops; its lock's connection ends with it, and the other signs.
    let (stopping, stopping_lease, going_on) = if signer == chains[0] {
        (first, first_lease, second)
    } else {
        (second, second_lease, first)
    };
    assert!(stopping.close(Duration::from_secs(5)));
    drop(stopping);
    stopping_lease.release().await;
    let other = chains
        .iter()
        .find(|chain| **chain != signer)
        .expect("other")
        .clone();
    assert!(
        eventually(Duration::from_secs(15), || {
            let _ = going_on.record(fleet_server::audit::Entry::new("host.admitted", "admitted"));
            checkpoints_in(&schema)
                .iter()
                .any(|(chain, _)| *chain == other)
        })
        .await,
        "the other instance takes over the signing"
    );
    let taken_over = checkpoints_in(&schema);
    let closed = taken_over
        .iter()
        .rev()
        .find(|(chain, _)| *chain == other)
        .expect("the other's")
        .1
        .heads
        .iter()
        .find(|head| head.chain == signer)
        .cloned();
    assert!(
        closed.is_some_and(|head| head.closed),
        "the stopped signer's chain is named closed: {taken_over:?}"
    );
    assert!(going_on.close(Duration::from_secs(5)));

    let spki = fleet_server::checkpoint::CheckpointSigner::public_key_der(&key);
    let (config, name) = (schema.config_on(schema.config.port), schema.name.clone());
    let verified = tokio::task::spawn_blocking(move || {
        fleet_server::postgres::audit::verify_checked_on(
            &config,
            Some(&name),
            None,
            Some(fleet_server::checkpoint::Check::new(
                fleet_server::checkpoint::AuditKey::from_der(&spki).expect("key"),
                None,
            )),
        )
    })
    .await
    .expect("verify")
    .expect("read");
    assert!(!verified.failed, "{:?}", verified.lines);

    // A chain removed with its record is named.
    schema.sql(&format!(
        "DELETE FROM audit_entries WHERE chain = '{signer}'::uuid; \
         DELETE FROM audit_chains WHERE id = '{signer}'::uuid"
    ));
    let spki = fleet_server::checkpoint::CheckpointSigner::public_key_der(&key);
    let (config, name) = (schema.config_on(schema.config.port), schema.name.clone());
    let verified = tokio::task::spawn_blocking(move || {
        fleet_server::postgres::audit::verify_checked_on(
            &config,
            Some(&name),
            None,
            Some(fleet_server::checkpoint::Check::new(
                fleet_server::checkpoint::AuditKey::from_der(&spki).expect("key"),
                None,
            )),
        )
    })
    .await
    .expect("verify")
    .expect("read");
    assert!(verified.failed);
    assert!(
        verified
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("chain {signer}")) && line.contains("removed")),
        "{:?}",
        verified.lines
    );
}

/// A source that signs whatever the lock says, as an instance that lost its lock while a
/// checkpoint was in flight does: it reads the newest checkpoint as `Checkpoints` does, and the
/// first time waits for the other instance to have read it too.
struct Racing {
    schema: Arc<Schema>,
    barrier: Arc<std::sync::Barrier>,
    raced: bool,
}

impl fleet_server::checkpoint::CheckpointSource for Racing {
    fn prepare(&mut self) -> Result<Option<fleet_server::checkpoint::Prepared>, String> {
        let newest = self
            .schema
            .query("SELECT seq::text || ' ' || hash FROM audit_checkpoint_head");
        let (seq, hash) = newest[0].split_once(' ').expect("the head row");
        let seq: u64 = seq.parse().expect("seq");
        if !self.raced {
            self.raced = true;
            self.barrier.wait();
        }
        Ok(Some(fleet_server::checkpoint::Prepared {
            tip: (seq > 0).then(|| fleet_server::checkpoint::Tip {
                checkpoint_seq: seq,
                hash: hash.to_string(),
                line: None,
            }),
            heads: Vec::new(),
        }))
    }
}

/// Two instances that both sign the same next checkpoint leave one per number: the insert of the
/// one that comes second fails, and its instance writes `audit.checkpoint_failed`.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_signers_racing_leave_one_checkpoint_per_number() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let schema = Arc::new(schema);
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let racing = |schema: Arc<Schema>, barrier: Arc<std::sync::Barrier>| {
        move |_: Arc<Database>, _| {
            Box::new(Racing {
                schema,
                barrier,
                raced: false,
            }) as Box<dyn fleet_server::checkpoint::CheckpointSource>
        }
    };
    let interval = Duration::from_secs(60);
    let (first, _first_lease, _) = signing_record(
        &schema,
        &key,
        interval,
        racing(schema.clone(), barrier.clone()),
    )
    .await;
    let (second, _second_lease, _) =
        signing_record(&schema, &key, interval, racing(schema.clone(), barrier)).await;
    assert!(
        eventually(Duration::from_secs(15), || !schema
            .query(
                "SELECT line FROM audit_entries \
                 WHERE line LIKE '%\"event\":\"audit.checkpoint_failed\"%'"
            )
            .is_empty())
        .await,
        "the second insert failed as audit.checkpoint_failed"
    );
    assert_eq!(
        schema.query("SELECT count(*)::text FROM audit_entries WHERE checkpoint_seq = 1"),
        vec!["1".to_string()],
        "one checkpoint 1"
    );
    let failed = schema.query(
        "SELECT line FROM audit_entries WHERE line LIKE '%\"event\":\"audit.checkpoint_failed\"%'",
    );
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(
        failed[0].contains("another instance signed first") || failed[0].contains("duplicate"),
        "{}",
        failed[0]
    );
    assert_eq!(checkpoints_in(&schema).len(), 1);
    assert!(first.close(Duration::from_secs(5)));
    assert!(second.close(Duration::from_secs(5)));
}

/// A prune that removes checkpoints names the last of them, and the record still verifies from
/// the first checkpoint left.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prune_names_the_last_checkpoint_it_removes_and_the_record_verifies_after() {
    use fleet_server::audit::Audit as _;
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let interval = Duration::from_millis(200);
    let (audit, lease, database) = signing_record(&schema, &key, interval, |database, held| {
        Box::new(fleet_server::postgres::checkpoint::Checkpoints::new(
            database, held,
        )) as Box<dyn fleet_server::checkpoint::CheckpointSource>
    })
    .await;
    let own = lease.held().borrow().clone().expect("held");
    assert!(
        eventually(Duration::from_secs(15), || {
            let _ = audit.record(fleet_server::audit::Entry::new("host.admitted", "admitted"));
            checkpoints_in(&schema).len() >= 3
        })
        .await,
        "three checkpoints"
    );
    // Everything up to the second checkpoint is past `retain_days`.
    let second: String = schema
        .query("SELECT seq::text FROM audit_entries WHERE checkpoint_seq = 2")
        .pop()
        .expect("checkpoint 2");
    schema.sql(&format!(
        "UPDATE audit_entries SET recorded_at = now() - interval '400 days' \
         WHERE chain = '{own}'::uuid AND seq <= {second}"
    ));
    let (pruning, own_chain) = (audit.clone(), own.clone());
    tokio::task::spawn_blocking(move || {
        fleet_server::postgres::audit_prune::prune(&database, pruning, Some(own_chain), 365)
    })
    .await
    .expect("prune")
    .expect("pruned");
    let pruned = schema
        .query("SELECT line FROM audit_entries WHERE line LIKE '%\"event\":\"audit.pruned\"%'");
    assert!(
        pruned
            .iter()
            .any(|line| line.contains("\"checkpoint_seq\":2,")),
        "the prune names checkpoint 2: {pruned:?}"
    );
    assert!(checkpoints_in(&schema)
        .iter()
        .all(|(_, parsed)| parsed.checkpoint_seq > 2));
    assert!(audit.close(Duration::from_secs(5)));
    let spki = fleet_server::checkpoint::CheckpointSigner::public_key_der(&key);
    let (config, name) = (schema.config_on(schema.config.port), schema.name.clone());
    let verified = tokio::task::spawn_blocking(move || {
        fleet_server::postgres::audit::verify_checked_on(
            &config,
            Some(&name),
            None,
            Some(fleet_server::checkpoint::Check::new(
                fleet_server::checkpoint::AuditKey::from_der(&spki).expect("key"),
                None,
            )),
        )
    })
    .await
    .expect("verify")
    .expect("read");
    assert!(!verified.failed, "{:?}", verified.lines);
}

/// A source that signs while `signs` names its instance: the real one, made when its turn comes
/// and dropped when it ends, so that its lock's connection ends and the next instance takes the
/// lock. How a test hands the signing from one instance to another while both run.
struct Turn {
    database: Arc<Database>,
    held: fleet_server::instance::Held,
    inner: Option<fleet_server::postgres::checkpoint::Checkpoints>,
    signs: Arc<std::sync::Mutex<&'static str>>,
    me: &'static str,
}

impl fleet_server::checkpoint::CheckpointSource for Turn {
    fn prepare(&mut self) -> Result<Option<fleet_server::checkpoint::Prepared>, String> {
        if *self.signs.lock().expect("turn") != self.me {
            self.inner = None;
            return Ok(None);
        }
        let (database, held) = (self.database.clone(), self.held.clone());
        self.inner
            .get_or_insert_with(|| {
                fleet_server::postgres::checkpoint::Checkpoints::new(database, held)
            })
            .prepare()
    }
}

/// A source for `signing_record` that takes turns by `signs`, as `me`.
fn turn(
    signs: &Arc<std::sync::Mutex<&'static str>>,
    me: &'static str,
) -> impl FnOnce(
    Arc<Database>,
    fleet_server::instance::Held,
) -> Box<dyn fleet_server::checkpoint::CheckpointSource> {
    let signs = signs.clone();
    move |database, held| {
        Box::new(Turn {
            database,
            held,
            inner: None,
            signs,
            me,
        }) as Box<dyn fleet_server::checkpoint::CheckpointSource>
    }
}

/// The record in `schema` checked against the checkpoints `key` signed.
async fn verified_against(
    schema: &Schema,
    key: &fleet_server::pkcs11::TokenKey,
) -> fleet_server::postgres::audit::Verified {
    let spki = fleet_server::checkpoint::CheckpointSigner::public_key_der(key);
    let (config, name) = (schema.config_on(schema.config.port), schema.name.clone());
    tokio::task::spawn_blocking(move || {
        fleet_server::postgres::audit::verify_checked_on(
            &config,
            Some(&name),
            None,
            Some(fleet_server::checkpoint::Check::new(
                fleet_server::checkpoint::AuditKey::from_der(&spki).expect("key"),
                None,
            )),
        )
    })
    .await
    .expect("verify")
    .expect("read")
}

/// Waits until two more checkpoints are signed than now, the instances deciding meanwhile: what
/// was written before is then covered.
async fn two_more_checkpoints(schema: &Schema, deciding: &[&fleet_server::audit_log::AuditLog]) {
    use fleet_server::audit::Audit as _;
    let newest = |schema: &Schema| {
        checkpoints_in(schema)
            .last()
            .map_or(0, |(_, parsed)| parsed.checkpoint_seq)
    };
    let from = newest(schema);
    assert!(
        eventually(Duration::from_secs(15), || {
            for audit in deciding {
                let _ = audit.record(fleet_server::audit::Entry::new("host.admitted", "admitted"));
            }
            newest(schema) >= from + 2
        })
        .await,
        "two more checkpoints after {from}"
    );
}

/// Prunes the record on `database` for the chain `own` to 365 days.
async fn prune_now(
    database: Arc<Database>,
    audit: Arc<fleet_server::audit_log::AuditLog>,
    own: String,
) -> Vec<fleet_server::postgres::audit_prune::Pruned> {
    tokio::task::spawn_blocking(move || {
        fleet_server::postgres::audit_prune::prune(&database, audit, Some(own), 365)
    })
    .await
    .expect("prune")
    .expect("pruned")
}

/// After the signing passed from A to B, both chains hold checkpoints. B's prune, though its
/// entries are all past `retain_days`, holds back from its checkpoints while A still holds older
/// ones; A's prune removes A's; then B's goes on. `audit-verify --audit-key` holds after each,
/// once a checkpoint covers the prune's entries.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prune_holds_back_from_checkpoints_newer_than_one_held_in_another_chain() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let signs = Arc::new(std::sync::Mutex::new("a"));
    let interval = Duration::from_millis(200);
    let (a, a_lease, a_database) = signing_record(&schema, &key, interval, turn(&signs, "a")).await;
    let (b, b_lease, b_database) = signing_record(&schema, &key, interval, turn(&signs, "b")).await;
    let (a_chain, b_chain) = (
        a_lease.held().borrow().clone().expect("a"),
        b_lease.held().borrow().clone().expect("b"),
    );
    let signed_in = |chain: &str| {
        checkpoints_in(&schema)
            .iter()
            .filter(|(signer, _)| signer == chain)
            .count()
    };
    two_more_checkpoints(&schema, &[&a, &b]).await;
    assert!(signed_in(&a_chain) >= 2, "A signs");
    *signs.lock().expect("turn") = "b";
    assert!(
        eventually(Duration::from_secs(15), || signed_in(&b_chain) >= 1).await,
        "B signs after A"
    );
    two_more_checkpoints(&schema, &[&a, &b]).await;
    let b_checkpoints: Vec<u64> = checkpoints_in(&schema)
        .iter()
        .filter(|(chain, _)| *chain == b_chain)
        .map(|(_, parsed)| parsed.checkpoint_seq)
        .collect();
    schema.sql("UPDATE audit_entries SET recorded_at = now() - interval '400 days'");

    // B first: it holds back from its checkpoints, as A still holds older ones.
    let rounds = prune_now(b_database.clone(), b.clone(), b_chain.clone()).await;
    assert!(rounds.iter().any(|round| round.held_back), "{rounds:?}");
    let held: Vec<u64> = checkpoints_in(&schema)
        .iter()
        .map(|(_, parsed)| parsed.checkpoint_seq)
        .collect();
    assert!(
        b_checkpoints.iter().all(|seq| held.contains(seq)),
        "B's prune held back from its checkpoints: {held:?}"
    );
    two_more_checkpoints(&schema, &[&a, &b]).await;
    let verified = verified_against(&schema, &key).await;
    assert!(!verified.failed, "{:?}", verified.lines);

    // A's prune removes A's checkpoints; then B's goes on past its old ones.
    prune_now(a_database, a.clone(), a_chain.clone()).await;
    assert_eq!(signed_in(&a_chain), 0, "A's checkpoints are pruned");
    two_more_checkpoints(&schema, &[&a, &b]).await;
    let verified = verified_against(&schema, &key).await;
    assert!(!verified.failed, "{:?}", verified.lines);
    schema.sql(&format!(
        "UPDATE audit_entries SET recorded_at = now() - interval '400 days' \
         WHERE chain = '{b_chain}'::uuid AND checkpoint_seq <= {}",
        b_checkpoints.last().expect("one")
    ));
    prune_now(b_database, b.clone(), b_chain.clone()).await;
    let held: Vec<u64> = checkpoints_in(&schema)
        .iter()
        .map(|(_, parsed)| parsed.checkpoint_seq)
        .collect();
    assert!(
        b_checkpoints.iter().all(|seq| !held.contains(seq)),
        "B's old checkpoints go once A holds none older: {held:?}"
    );
    two_more_checkpoints(&schema, &[&a, &b]).await;
    let verified = verified_against(&schema, &key).await;
    assert!(!verified.failed, "{:?}", verified.lines);
    assert!(a.close(Duration::from_secs(5)));
    assert!(b.close(Duration::from_secs(5)));
}

/// The signing passes back and forth between A and B several times, and B stops, its chain
/// closed. One prune by A, which holds the prune lock, repeats its rounds until nothing more is
/// freed: every old checkpoint goes, in A's chain and in B's closed one, and the record verifies.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_prune_frees_checkpoints_after_several_signer_changes() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let signs = Arc::new(std::sync::Mutex::new("a"));
    let interval = Duration::from_millis(200);
    let (a, a_lease, a_database) = signing_record(&schema, &key, interval, turn(&signs, "a")).await;
    let (b, b_lease, _) = signing_record(&schema, &key, interval, turn(&signs, "b")).await;
    let (a_chain, b_chain) = (
        a_lease.held().borrow().clone().expect("a"),
        b_lease.held().borrow().clone().expect("b"),
    );
    for (n, me) in ["a", "b", "a", "b", "a", "b"].into_iter().enumerate() {
        *signs.lock().expect("turn") = me;
        let chain = if me == "a" { &a_chain } else { &b_chain };
        let before = checkpoints_in(&schema)
            .iter()
            .filter(|(signer, _)| signer == chain)
            .count();
        assert!(
            eventually(Duration::from_secs(15), || {
                use fleet_server::audit::Audit as _;
                let _ = a.record(fleet_server::audit::Entry::new("host.admitted", "admitted"));
                let _ = b.record(fleet_server::audit::Entry::new("host.admitted", "admitted"));
                checkpoints_in(&schema)
                    .iter()
                    .filter(|(signer, _)| signer == chain)
                    .count()
                    > before
            })
            .await,
            "turn {n}: {me} signs"
        );
    }
    let signers: Vec<String> = checkpoints_in(&schema)
        .into_iter()
        .map(|(chain, _)| chain)
        .collect();
    let changes = signers.windows(2).filter(|pair| pair[0] != pair[1]).count();
    assert!(changes >= 5, "the signing changed hands: {signers:?}");
    // B stops; A signs again.
    *signs.lock().expect("turn") = "a";
    assert!(b.close(Duration::from_secs(5)));
    drop(b);
    b_lease.release().await;
    two_more_checkpoints(&schema, &[&a]).await;
    let old = checkpoints_in(&schema)
        .last()
        .map_or(0, |(_, parsed)| parsed.checkpoint_seq);
    schema.sql("UPDATE audit_entries SET recorded_at = now() - interval '400 days'");
    let rounds = prune_now(a_database, a.clone(), a_chain.clone()).await;
    assert!(rounds.len() > 1, "the rounds repeat: {rounds:?}");
    // What was old went, but for the run of A's chain the prune never cuts past its own end.
    let left: Vec<(String, u64)> = checkpoints_in(&schema)
        .into_iter()
        .map(|(chain, parsed)| (chain, parsed.checkpoint_seq))
        .collect();
    assert!(
        left.iter().all(|(chain, _)| *chain == a_chain),
        "B's closed chain holds no checkpoint: {left:?}"
    );
    assert!(
        left.iter().filter(|(_, seq)| *seq <= old).count() <= 1,
        "every old checkpoint went in one prune: {left:?}"
    );
    two_more_checkpoints(&schema, &[&a]).await;
    let verified = verified_against(&schema, &key).await;
    assert!(!verified.failed, "{:?}", verified.lines);
    assert!(a.close(Duration::from_secs(5)));
}

/// A closed chain is named closed by one checkpoint, and by none after it.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_chain_is_named_by_one_checkpoint() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let signs = Arc::new(std::sync::Mutex::new("a"));
    let interval = Duration::from_millis(200);
    let (a, _a_lease, _) = signing_record(&schema, &key, interval, turn(&signs, "a")).await;
    let (b, b_lease, _) = signing_record(&schema, &key, interval, turn(&signs, "b")).await;
    let b_chain = b_lease.held().borrow().clone().expect("b");
    two_more_checkpoints(&schema, &[&a, &b]).await;
    assert!(b.close(Duration::from_secs(5)));
    drop(b);
    b_lease.release().await;
    for _ in 0..5 {
        two_more_checkpoints(&schema, &[&a]).await;
    }
    let named: Vec<(u64, bool)> = checkpoints_in(&schema)
        .iter()
        .filter_map(|(_, parsed)| {
            parsed
                .heads
                .iter()
                .find(|head| head.chain == b_chain)
                .map(|head| (parsed.checkpoint_seq, head.closed))
        })
        .collect();
    assert_eq!(
        named.iter().filter(|(_, closed)| *closed).count(),
        1,
        "named closed once: {named:?}"
    );
    assert_eq!(
        named.last().map(|(_, closed)| *closed),
        Some(true),
        "and not named after: {named:?}"
    );
    let verified = verified_against(&schema, &key).await;
    assert!(!verified.failed, "{:?}", verified.lines);
    assert!(a.close(Duration::from_secs(5)));
}

/// The last entry of `chain` at or below `below`: its `seq` and line.
fn last_entry(schema: &Schema, chain: &str, below: Option<u64>) -> (u64, String) {
    let filter = below.map_or(String::new(), |below| format!(" AND seq <= {below}"));
    let row = schema
        .query(&format!(
            "SELECT seq::text || ' ' || line FROM audit_entries \
             WHERE chain = '{chain}'::uuid{filter} ORDER BY seq DESC LIMIT 1"
        ))
        .pop()
        .expect("a row");
    let (seq, line) = row.split_once(' ').expect("seq and line");
    (seq.parse().expect("seq"), line.to_string())
}

/// An `audit.pruned` naming `last_seq` of `pruned_chain` with `last_hash`, added after the last
/// entry of `into` as whoever can write the database can: its `prev` right, no checkpoint after.
fn forge_pruned(schema: &Schema, into: &str, pruned_chain: &str, last_seq: u64, last_hash: &str) {
    let (seq, line) = last_entry(schema, into, None);
    let entry = fleet_server::audit::Entry::new("audit.pruned", "recorded")
        .with("pruned_chain", pruned_chain.to_string())
        .with("last_seq", last_seq)
        .with("last_hash", last_hash.to_string())
        .with("chain", into.to_string());
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("now")
            .as_millis(),
    )
    .expect("ms");
    let forged = fleet_server::audit_log::render(
        seq + 1,
        now,
        &entry,
        &fleet_server::audit_log::line_hash(&line),
    );
    schema.sql(&format!(
        "INSERT INTO audit_entries (chain, seq, line) VALUES ('{into}'::uuid, {}, $q${forged}$q$)",
        seq + 1
    ));
}

/// A signs, naming B's chain; then the signing stops, so what follows is the unsigned tail.
async fn signed_then_still(
    schema: &Schema,
    key: &fleet_server::pkcs11::TokenKey,
) -> (
    Arc<fleet_server::audit_log::AuditLog>,
    Arc<fleet_server::audit_log::AuditLog>,
    String,
    String,
    [fleet_server::instance::Lease; 2],
) {
    let signs = Arc::new(std::sync::Mutex::new("a"));
    let interval = Duration::from_millis(200);
    let (a, a_lease, _) = signing_record(schema, key, interval, turn(&signs, "a")).await;
    let (b, b_lease, _) = signing_record(schema, key, interval, turn(&signs, "b")).await;
    for _ in 0..3 {
        two_more_checkpoints(schema, &[&a, &b]).await;
    }
    *signs.lock().expect("turn") = "nobody";
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (a_chain, b_chain) = (
        a_lease.held().borrow().clone().expect("a"),
        b_lease.held().borrow().clone().expect("b"),
    );
    (a, b, a_chain, b_chain, [a_lease, b_lease])
}

/// A chain removed whole, with an `audit.pruned` added after the newest checkpoint to explain it,
/// still fails: only a removal a checkpoint covers counts.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_chain_removed_with_a_forged_prune_in_the_unsigned_tail_fails() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let (_a, _b, a_chain, b_chain, _leases) = signed_then_still(&schema, &key).await;
    let before = verified_against(&schema, &key).await;
    assert!(!before.failed, "{:?}", before.lines);
    let (b_seq, b_line) = last_entry(&schema, &b_chain, None);
    schema.sql(&format!(
        "DELETE FROM audit_entries WHERE chain = '{b_chain}'::uuid; \
         DELETE FROM audit_chains WHERE id = '{b_chain}'::uuid"
    ));
    forge_pruned(
        &schema,
        &a_chain,
        &b_chain,
        b_seq,
        &fleet_server::audit_log::line_hash(&b_line),
    );
    let forged = verified_against(&schema, &key).await;
    assert!(forged.failed, "{:?}", forged.lines);
    assert!(
        forged
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("chain {b_chain}")) && line.contains("removed")),
        "{:?}",
        forged.lines
    );
    assert!(
        forged.lines.iter().any(|line| line
            .contains("audit.pruned lies after every signed head the record still holds")),
        "{:?}",
        forged.lines
    );
}

/// A chain's young entries cut, with an `audit.pruned` added after the newest checkpoint naming
/// the last one cut, still fails: the earlier checkpoints name heads among them, and the chain
/// begins where no signed removal says it may.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_young_cut_with_a_forged_prune_in_the_unsigned_tail_fails() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let (_a, _b, a_chain, b_chain, _leases) = signed_then_still(&schema, &key).await;
    let (b_seq, _) = last_entry(&schema, &b_chain, None);
    let (cut, cut_line) = last_entry(&schema, &b_chain, Some(b_seq - 1));
    schema.sql(&format!(
        "DELETE FROM audit_entries WHERE chain = '{b_chain}'::uuid AND seq <= {cut}"
    ));
    forge_pruned(
        &schema,
        &a_chain,
        &b_chain,
        cut,
        &fleet_server::audit_log::line_hash(&cut_line),
    );
    let forged = verified_against(&schema, &key).await;
    assert!(forged.failed, "{:?}", forged.lines);
    assert!(
        forged.lines.iter().any(|line| line.starts_with(&format!(
            "chain {b_chain}, entry {}: the entries before it are gone",
            cut + 1
        ))),
        "{:?}",
        forged.lines
    );
}

/// Every `seq` of `chain` some checkpoint names as a head.
fn named_heads(schema: &Schema, chain: &str) -> Vec<u64> {
    let mut seqs: Vec<u64> = checkpoints_in(schema)
        .iter()
        .flat_map(|(_, parsed)| {
            parsed
                .heads
                .iter()
                .filter(|head| head.chain == chain)
                .map(|head| head.seq)
                .collect::<Vec<_>>()
        })
        .collect();
    seqs.sort_unstable();
    seqs.dedup();
    seqs
}

/// An `audit.pruned` written at `seq` of `into`, chained to `prev_line`, naming `last_seq` of
/// `pruned_chain` with `last_hash`: what whoever can write the database can place inside a run
/// it cut.
fn forge_pruned_at(
    schema: &Schema,
    into: &str,
    seq: u64,
    prev_line: &str,
    pruned_chain: &str,
    last_seq: u64,
    last_hash: &str,
) {
    let entry = fleet_server::audit::Entry::new("audit.pruned", "recorded")
        .with("pruned_chain", pruned_chain.to_string())
        .with("last_seq", last_seq)
        .with("last_hash", last_hash.to_string())
        .with("chain", into.to_string());
    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("now")
            .as_millis(),
    )
    .expect("ms");
    let forged = fleet_server::audit_log::render(
        seq,
        now,
        &entry,
        &fleet_server::audit_log::line_hash(prev_line),
    );
    schema.sql(&format!(
        "INSERT INTO audit_entries (chain, seq, line) VALUES ('{into}'::uuid, {seq}, $q${forged}$q$)"
    ));
}

/// B's young, signed entries cut from an unnamed `seq` on, and one `audit.pruned` placed at that
/// `seq` naming B's own newest head: no head that is still there covers it, so it fails.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_young_cut_excused_by_a_prune_inside_it_fails() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let (_a, _b, _a_chain, b_chain, _leases) = signed_then_still(&schema, &key).await;
    assert!(!verified_against(&schema, &key).await.failed);
    let named = named_heads(&schema, &b_chain);
    let head = *named.last().expect("b named");
    let cut = (2..head)
        .rev()
        .find(|seq| !named.contains(seq))
        .expect("an unnamed seq below the head");
    let (_, head_line) = last_entry(&schema, &b_chain, Some(head));
    let (_, prev_line) = last_entry(&schema, &b_chain, Some(cut - 1));
    schema.sql(&format!(
        "DELETE FROM audit_entries WHERE chain = '{b_chain}'::uuid AND seq >= {cut}"
    ));
    forge_pruned_at(
        &schema,
        &b_chain,
        cut,
        &prev_line,
        &b_chain,
        head,
        &fleet_server::audit_log::line_hash(&head_line),
    );
    let forged = verified_against(&schema, &key).await;
    assert!(forged.failed, "{:?}", forged.lines);
    assert!(
        forged
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("chain {b_chain}, entry {head}"))),
        "{:?}",
        forged.lines
    );
}

/// Three instances. C removed whole and B's tail cut, with prunes placed in B's cut run naming
/// C's newest head and B's own; then, separately, B and C both cut with prunes that excuse each
/// other. Each fails.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chains_cut_with_prunes_that_excuse_each_other_fail() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let signs = Arc::new(std::sync::Mutex::new("a"));
    let interval = Duration::from_millis(200);
    let (a, _a_lease, _) = signing_record(&schema, &key, interval, turn(&signs, "a")).await;
    let (b, b_lease, _) = signing_record(&schema, &key, interval, turn(&signs, "b")).await;
    let (c, c_lease, _) = signing_record(&schema, &key, interval, turn(&signs, "c")).await;
    for _ in 0..3 {
        two_more_checkpoints(&schema, &[&a, &b, &c]).await;
    }
    *signs.lock().expect("turn") = "nobody";
    tokio::time::sleep(Duration::from_millis(600)).await;
    let b_chain = b_lease.held().borrow().clone().expect("b");
    let c_chain = c_lease.held().borrow().clone().expect("c");
    assert!(!verified_against(&schema, &key).await.failed);
    // Where each chain can be cut: two unnamed seqs below its newest named head.
    let cut_of = |chain: &str| {
        let named = named_heads(&schema, chain);
        let head = *named.last().expect("named");
        let cut = (2..head - 1)
            .rev()
            .find(|seq| !named.contains(seq) && !named.contains(&(seq + 1)))
            .expect("two unnamed seqs");
        let (_, head_line) = last_entry(&schema, chain, Some(head));
        let (_, prev_line) = last_entry(&schema, chain, Some(cut - 1));
        (
            head,
            fleet_server::audit_log::line_hash(&head_line),
            cut,
            prev_line,
        )
    };
    let (b_head, b_hash, b_cut, b_prev) = cut_of(&b_chain);
    let (c_head, c_hash, c_cut, c_prev) = cut_of(&c_chain);
    let backup = |chain: &str| {
        schema.query(&format!(
            "SELECT seq::text || ' ' || line FROM audit_entries WHERE chain = '{chain}'::uuid"
        ))
    };
    let (b_rows, c_rows) = (backup(&b_chain), backup(&c_chain));
    let restore = |chain: &str, rows: &[String]| {
        schema.sql(&format!(
            "DELETE FROM audit_entries WHERE chain = '{chain}'::uuid"
        ));
        for row in rows {
            let (seq, line) = row.split_once(' ').expect("seq and line");
            schema.sql(&format!(
                "INSERT INTO audit_entries (chain, seq, line) \
                 VALUES ('{chain}'::uuid, {seq}, $q${line}$q$)"
            ));
        }
    };

    // The two chains cut, each with a prune in its cut run naming the other's newest head.
    schema.sql(&format!(
        "DELETE FROM audit_entries WHERE chain = '{b_chain}'::uuid AND seq >= {b_cut}; \
         DELETE FROM audit_entries WHERE chain = '{c_chain}'::uuid AND seq >= {c_cut}"
    ));
    forge_pruned_at(&schema, &b_chain, b_cut, &b_prev, &c_chain, c_head, &c_hash);
    forge_pruned_at(&schema, &c_chain, c_cut, &c_prev, &b_chain, b_head, &b_hash);
    let cycle = verified_against(&schema, &key).await;
    assert!(cycle.failed, "{:?}", cycle.lines);
    for (chain, head) in [(&b_chain, b_head), (&c_chain, c_head)] {
        assert!(
            cycle
                .lines
                .iter()
                .any(|line| line.starts_with(&format!("chain {chain}, entry {head}"))),
            "{chain}: {:?}",
            cycle.lines
        );
    }
    restore(&b_chain, &b_rows);
    restore(&c_chain, &c_rows);
    assert!(!verified_against(&schema, &key).await.failed, "restored");

    // C removed whole; B cut, with prunes in its cut run naming C's newest head and B's own.
    schema.sql(&format!(
        "DELETE FROM audit_entries WHERE chain = '{b_chain}'::uuid AND seq >= {b_cut}; \
         DELETE FROM audit_entries WHERE chain = '{c_chain}'::uuid; \
         DELETE FROM audit_chains WHERE id = '{c_chain}'::uuid"
    ));
    forge_pruned_at(&schema, &b_chain, b_cut, &b_prev, &c_chain, c_head, &c_hash);
    let (_, first_forged) = last_entry(&schema, &b_chain, None);
    forge_pruned_at(
        &schema,
        &b_chain,
        b_cut + 1,
        &first_forged,
        &b_chain,
        b_head,
        &b_hash,
    );
    let whole = verified_against(&schema, &key).await;
    assert!(whole.failed, "{:?}", whole.lines);
    assert!(
        whole
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("chain {c_chain}")) && line.contains("removed")),
        "{:?}",
        whole.lines
    );
    let _ = (a, b, c);
}

/// The chain and `seq` of the entry that holds checkpoint `n`.
fn checkpoint_entry(schema: &Schema, n: u64) -> (String, u64) {
    let row = schema
        .query(&format!(
            "SELECT chain::text || ' ' || seq::text FROM audit_entries WHERE checkpoint_seq = {n}"
        ))
        .pop()
        .expect("the checkpoint");
    let (chain, seq) = row.split_once(' ').expect("chain and seq");
    (chain.to_string(), seq.parse().expect("seq"))
}

/// Holds the prune lock in a session of the test's own, as another instance's round does.
fn hold_the_prune_lock(schema: &Schema) -> impl Drop {
    schema.hold(
        "SELECT pg_advisory_lock(hashtextextended('opamp-fleet-server-audit-prune:' || \
         coalesce(current_schema(), ''), 0));",
    )
}

/// A starts and signs, C writes and closes, and one checkpoint names C closed. Everything of A up
/// past that checkpoint and all of C are past `retain_days`; another session holds the prune lock,
/// so A prunes its own chain alone. With `c_pruned_first`, C pruned its own oldest entries while it
/// was open. Answers A's record, A's chain and C's chain.
async fn closed_chain_named_once_then_old(
    schema: &Schema,
    key: &fleet_server::pkcs11::TokenKey,
    c_pruned_first: bool,
) -> (Arc<fleet_server::audit_log::AuditLog>, String, String) {
    let signs = Arc::new(std::sync::Mutex::new("a"));
    let interval = Duration::from_millis(200);
    let (a, a_lease, a_database) = signing_record(schema, key, interval, turn(&signs, "a")).await;
    let (c, c_lease, c_database) = signing_record(schema, key, interval, turn(&signs, "c")).await;
    let a_chain = a_lease.held().borrow().clone().expect("a");
    let c_chain = c_lease.held().borrow().clone().expect("c");
    two_more_checkpoints(schema, &[&a, &c]).await;
    if c_pruned_first {
        schema.sql(&format!(
            "UPDATE audit_entries SET recorded_at = now() - interval '400 days' \
             WHERE chain = '{c_chain}'::uuid AND seq <= 3"
        ));
        prune_now(c_database, c.clone(), c_chain.clone()).await;
        two_more_checkpoints(schema, &[&a, &c]).await;
        assert!(!verified_against(schema, key).await.failed);
    }
    assert!(c.close(Duration::from_secs(5)));
    drop(c);
    c_lease.release().await;
    for _ in 0..3 {
        two_more_checkpoints(schema, &[&a]).await;
    }
    let named_closed = checkpoints_in(schema)
        .iter()
        .filter(|(_, parsed)| {
            parsed
                .heads
                .iter()
                .any(|head| head.chain == c_chain && head.closed)
        })
        .map(|(_, parsed)| parsed.checkpoint_seq)
        .collect::<Vec<_>>();
    assert_eq!(named_closed.len(), 1, "{named_closed:?}");
    let (in_chain, after) = checkpoint_entry(schema, named_closed[0] + 1);
    assert_eq!(in_chain, a_chain);
    schema.sql(&format!(
        "UPDATE audit_entries SET recorded_at = now() - interval '400 days' \
         WHERE (chain = '{a_chain}'::uuid AND seq <= {after}) OR chain = '{c_chain}'::uuid"
    ));
    let lock = hold_the_prune_lock(schema);
    let rounds = prune_now(a_database, a.clone(), a_chain.clone()).await;
    drop(lock);
    assert!(rounds.iter().any(|round| round.held_back), "{rounds:?}");
    assert!(
        checkpoints_in(schema)
            .iter()
            .any(|(_, parsed)| parsed.checkpoint_seq == named_closed[0]),
        "the checkpoint that names C closed is kept while C holds entries"
    );
    two_more_checkpoints(schema, &[&a]).await;
    (a, a_chain, c_chain)
}

/// A closed chain named closed by one checkpoint: A's own prune keeps that checkpoint while the
/// chain still holds entries, so removing the chain afterwards is still found.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_closed_chain_whose_naming_checkpoint_is_old_cannot_be_removed_unseen() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let (a, _a_chain, c_chain) = closed_chain_named_once_then_old(&schema, &key, false).await;
    let kept = verified_against(&schema, &key).await;
    assert!(!kept.failed, "{:?}", kept.lines);
    schema.sql(&format!(
        "DELETE FROM audit_entries WHERE chain = '{c_chain}'::uuid; \
         DELETE FROM audit_chains WHERE id = '{c_chain}'::uuid"
    ));
    let removed = verified_against(&schema, &key).await;
    assert!(removed.failed, "{:?}", removed.lines);
    assert!(
        removed
            .lines
            .iter()
            .any(|line| line.starts_with(&format!("chain {c_chain}")) && line.contains("removed")),
        "{:?}",
        removed.lines
    );
    assert!(a.close(Duration::from_secs(5)));
}

/// As above, but C pruned its own oldest entries while open: the honest record still verifies
/// after A's prune, since the checkpoint naming C closed, which covers C's prune, is kept.
/// Verifies: ADR-0041
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_honest_record_holds_after_a_prune_while_a_closed_self_pruned_chain_remains() {
    let Some(schema) = Schema::named() else {
        return;
    };
    let Some(test) = token::TestToken::named() else {
        return;
    };
    test.generate("audit");
    let key = test.open().key("[audit] key_label", "audit").expect("key");
    let (a, _a_chain, _c_chain) = closed_chain_named_once_then_old(&schema, &key, true).await;
    let honest = verified_against(&schema, &key).await;
    assert!(!honest.failed, "{:?}", honest.lines);
    assert!(a.close(Duration::from_secs(5)));
}
