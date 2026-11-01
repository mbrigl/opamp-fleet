//! Several Server instances on one PostgreSQL database, each through its real listeners
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it):
//! both hold a lease and serve, a revocation committed on one ends the session it concerns on the
//! other, and an instance whose database stops answering closes its listeners until it holds a new
//! lease.
//!
//! They run against the database the `FLEET_TEST_POSTGRES_*` variables name, in a schema of their
//! own, over TLS against `FLEET_TEST_POSTGRES_CA_FILE` where it is named and over plaintext to the
//! loopback where it is not, and skip, saying so, where no database is named.
//! `scripts/check-store.sh postgres` runs them, and fails on a skip.

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use fleet_server::config::{LeaseTimes, PostgresConfig};
use fleet_server::enrolment::Enrolment;
use fleet_server::floors::PackageFloors;
use fleet_server::postgres::{Database, Exclusive};
use fleet_server::revocation::{CertId, Revocations};
use fleet_server::Records;
use opamp::proto::{AgentCapabilities, AgentToServer};
use opamp::uid::InstanceUid;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};

/// A lease short enough for a test to watch it lapse.
const TIMES: LeaseTimes = LeaseTimes {
    renew: Duration::from_secs(1),
    lapse: Duration::from_secs(3),
};

/// The database the variables name, in a schema of this test's own that is dropped with it.
struct Schema {
    config: PostgresConfig,
    name: String,
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
            ca_file,
        };
        let mut random = [0u8; 8];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
            .expect("random");
        let schema = Schema {
            config,
            name: format!("fleet_test_{}", hex::encode(random)),
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
        }
    }

    /// Runs `statements`, past every adapter.
    fn sql(&self, statements: &str) {
        self.query(statements);
    }

    /// The first column of every row `query` answers in this schema, as text.
    fn query(&self, query: &str) -> Vec<String> {
        let password = std::fs::read_to_string(&self.config.password_file).expect("password");
        let mut pg = tokio_postgres::Config::new();
        pg.host(&self.config.host)
            .port(self.config.port)
            .dbname(&self.config.dbname)
            .user(&self.config.user)
            .password(password.trim());
        let query = format!("SET search_path = {};", self.name) + query;
        let roots = self.config.ca_file.as_ref().map(|path| {
            opamp::tls::root_store(&std::fs::read(path).expect("FLEET_TEST_POSTGRES_CA_FILE"))
                .expect("roots")
        });
        if roots.is_some() {
            pg.ssl_mode(tokio_postgres::config::SslMode::Require);
        }
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(async move {
                    let client = match roots {
                        Some(roots) => {
                            opamp::tls::install_ring_provider();
                            let tls = tokio_postgres_rustls::MakeRustlsConnect::new(
                                opamp::tls::client_builder()
                                    .with_root_certificates(roots)
                                    .with_no_client_auth(),
                            );
                            let (client, connection) = pg.connect(tls).await.expect("db");
                            tokio::spawn(connection);
                            client
                        }
                        None => {
                            let (client, connection) =
                                pg.connect(tokio_postgres::NoTls).await.expect("db");
                            tokio::spawn(connection);
                            client
                        }
                    };
                    client
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
}

impl Drop for Schema {
    fn drop(&mut self) {
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
        let key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "opamp-fleet-test-ca");
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

/// One running instance, as the binary composes it.
struct Instance {
    state: Arc<fleet_server::fleet::AppState>,
    agent: SocketAddr,
    operator: SocketAddr,
    revocations: Arc<Revocations>,
    floors: Arc<PackageFloors>,
    enrolment: Arc<Enrolment>,
    following: fleet_server::instance::Following,
    database: Arc<Database>,
    held: fleet_server::instance::Held,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
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
        Self::start_with(pki, schema, config, true, false).await
    }

    /// Starts taking up what other instances commit, for an instance started deaf.
    fn follow(&mut self) {
        self.following = fleet_server::instance::follow(&self.database, self.state.clone(), None);
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
        Self::start_with(pki, schema, config, renewals, true).await
    }

    /// [`start_following`](Self::start_following), following other instances where `follows`
    /// says so.
    async fn start_with(
        pki: &Pki,
        schema: &Schema,
        config: PostgresConfig,
        renewals: bool,
        follows: bool,
    ) -> Self {
        opamp::tls::install_ring_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let name = schema.name.clone();
        let database = tokio::task::spawn_blocking(move || {
            Database::open_in(&config, Exclusive::Shared, Some(&name))
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
        let packages = fleet_server::packages::PackageStore::open_on(
            &records,
            dir.path(),
            &toml::from_str::<fleet_server::config::BlobsConfig>("").expect("blobs"),
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
                .with_enrolment(Some(enrolment.clone())),
        );
        let (server_cert, server_key) = pki.issue("localhost");
        for (file, pem) in [
            ("server-cert.pem", &server_cert),
            ("server-key.pem", &server_key),
            ("ca.pem", &pki.ca_pem),
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
        let planes = fleet_server::tls::server_tls(&tls, None, None).expect("server material");
        let agents = fleet_server::agent_app(
            state.clone(),
            fleet_server::transport::Admission::new(true)
                .with_revocations(Some(revocations.clone())),
        );
        let operators = fleet_server::operator_app(state.clone(), None);

        let lease = {
            let database = database.clone();
            fleet_server::instance::Lease::take(database, TIMES, "test-host").expect("lease")
        };
        state.serve_under(lease.held());
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
            database,
            held,
            stop: Some(stop),
            _dir: dir,
        }
    }

    /// The instance id of the lease it holds, if it holds one.
    fn instance_id(&self) -> Option<String> {
        self.held.borrow().clone()
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
/// plane; a revocation committed on A ends the session of that certificate on B within one
/// renewal interval, and no other session.
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-draft-admission-by-client-certificate-and-revocation
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
        closed_within(&mut revoked, TIMES.renew).await,
        Some((1008, "revoked".to_string())),
        "B did not end the revoked session within one renewal interval"
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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

/// The store as one instance opens it, in the test's schema: what each instance writes through.
fn records(schema: &Schema) -> Records {
    Records::Postgres(
        Database::open_in(&schema.config, Exclusive::Shared, Some(&schema.name))
            .expect("open the database"),
    )
}

/// Two instances that each revoke a certificate at the same moment keep both revocations: each
/// adds its own row, and neither writes the list it holds over the other's.
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, Q-1
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0028, Q-6
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0026
#[test]
fn two_revisions_rolled_out_at_once_on_two_instances_are_both_kept() {
    use fleet_server::configs::{revision_hash, ConfigStore, Revision};
    let Some(schema) = Schema::named() else {
        return;
    };
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0027
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0026, ADR-0027
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
    /// A certificate and key naming `host` the way the Server's own signer does (ADR-0022 clause
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-draft-admission-by-client-certificate-and-revocation
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revocation_committed_while_the_listening_connection_was_cut_ends_its_session_once_back()
{
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let network = Cuttable::to(database_address(&schema));
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    // Without the renewals' comparison, which would catch the revocation on its own.
    let b = Instance::start_following(&pki, &schema, schema.config_on(network.port), false).await;
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-draft-admission-by-client-certificate-and-revocation
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0028
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
    let revocations = instance.revocations.clone();
    let record = move |signed: &fleet_server::revocation::Signed,
                       instance_uid: &[u8],
                       bootstrap: &CertId| {
        revocations
            .record_approved(signed.facts.clone(), instance_uid, bootstrap)
            .map_err(fleet_server::enrolment::DecisionError::Store)
    };
    instance
        .enrolment
        .approve(key, &signer, &format!("host-{key}"), &record, &|_| false)
        .expect("approve");
}

/// An enrolment request waiting on B, approved on A, is answered with its certificate on B, which
/// wakes the connection that asked: B takes up the window A opened and the decision A took.
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0022
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_enrolment_approved_on_one_instance_is_handed_out_by_the_other() {
    use fleet_server::enrolment::Submitted;
    let Some(schema) = Schema::named() else {
        return;
    };
    let pki = Pki::new();
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    a.enrolment.open(600).expect("A opens the window");
    assert!(
        eventually(Duration::from_secs(10), || b.enrolment.is_open()).await,
        "B never took up the window A opened"
    );
    let (request, requester) = enrolment_request("k1");
    let mut asking = b.enrolment.subscribe();
    asking.borrow_and_update();
    let on_b = b.enrolment.clone();
    let (sent, by) = (request.clone(), requester.clone());
    assert_eq!(
        tokio::task::spawn_blocking(move || on_b.submit(sent, by))
            .await
            .expect("submit"),
        Submitted::Waiting
    );

    assert!(
        eventually(Duration::from_secs(10), || a
            .enrolment
            .pending()
            .iter()
            .any(|pending| pending.id == "k1"))
        .await,
        "A never listed the request B took in"
    );
    approve(&a, &pki, "k1");
    // What the connection that asked does: it waits for a change, then asks again.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let issued = loop {
        assert!(
            tokio::time::timeout_at(deadline, asking.changed())
                .await
                .is_ok(),
            "B's waiting connection was never woken"
        );
        let on_b = b.enrolment.clone();
        let (sent, by) = (request.clone(), requester.clone());
        match tokio::task::spawn_blocking(move || on_b.submit(sent, by))
            .await
            .expect("submit")
        {
            Submitted::Issued(pem) => break pem,
            Submitted::Waiting => continue,
            other => panic!("B answered {other:?}"),
        }
    };
    assert!(
        issued.starts_with("-----BEGIN CERTIFICATE-----"),
        "{issued}"
    );
}

/// During an enrolment wave on A, A reads nothing again for its own notifications, and B takes up
/// every certificate and Host the wave registered. A then takes up the one change B commits, and
/// that once: what it heard of its own acts before was passed over.
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0026
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

    // A knows the Agent from the database it loads at startup, as every instance does until it
    // keeps only the Agents it carries.
    let a = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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

/// A revocation whose notification never arrives, while the listening connection stays up, is
/// caught by the next lease renewal: it finds the list's version moved, and the instance reads the
/// ledger again and ends the session it concerns.
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it, ADR-0021
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

/// An Agent carried by A that went quiet is forgotten through B, which decides from the presence
/// A wrote: refused while it reports, forgotten once it is silent past the budget, and A then ends
/// its session within one second.
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts, ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
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
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts, ADR-0024
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
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts
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
/// loaded it: the move announces the old identity forgotten, so B holds no record of it under a
/// name no store keeps.
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts, ADR-0024
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
    let on_a = a.clone();
    tokio::task::spawn_blocking(move || on_a.process(report(0, 1), Transport::WebSocket, Some(7)))
        .await
        .expect("report");
    // B loads the record at startup, and listens before A moves it.
    let b = Instance::start(&pki, &schema, schema.config_on(schema.config.port)).await;
    assert!(b.has_read(1).await, "B follows");
    assert!(view_of(&b, uid).is_some(), "B loaded it");

    let on_a = a.clone();
    let rekeyed = tokio::task::spawn_blocking(move || {
        on_a.process(
            report(
                opamp::proto::AgentToServerFlags::RequestInstanceUid as u64,
                2,
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
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts, ADR-0026
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
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts
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
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts
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
/// there, as B wrote it: no instance holds it any more, and B heard it last.
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts
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
    // B heard it after A did.
    schema.sql(&format!(
        "UPDATE agents SET last_seen_ms = last_seen_ms + 1000 WHERE instance_uid = '{uid}'"
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

/// An Agent that has just connected to A is not forgotten through B before A's next presence
/// batch: the report that connects it writes that A holds it, at once.
/// Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts
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
