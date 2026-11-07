//! The two tests that decide G-20: a fleet is served on when one of three Server instances is lost,
//! and the database carries the presence and the notifications of 100 000 Agents
//! (ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
//! clause 20 and the Enforcement).
//!
//! Each runs three `server` processes at the defaults, `max_connections` and `max_agents` raised
//! for the fleet, on the database the `FLEET_TEST_POSTGRES_*` variables name, in a role and schema
//! of the test's own, and on the bucket the `FLEET_TEST_S3_*` variables name. In front of the Agent
//! planes stands a layer-4 balancer of the test's own: it hashes the peer's address over the
//! instances that answer its probe, and connects to the instance from the peer's own address, which
//! on the loopback every test address is, so the instances see each peer as it is. Each simulated
//! host has a certificate and a loopback address of its own and carries its Agents on one
//! connection through the `crates/opamp` client.
//!
//! They need the whole stack and run for minutes, so they run only where `FLEET_G20_STACK=1` names
//! it, and skip, saying so, elsewhere. `scripts/check-g20.sh` runs them in the release profile and
//! fails on a skip.
//!
//! The load test's fleet, its Agents per connection and its length may be set by
//! `FLEET_G20_LOAD_AGENTS`, `FLEET_G20_AGENTS_PER_HOST` and `FLEET_G20_LOAD_SECS` for a shorter run
//! on a smaller machine; it then says it did not decide G-20, and a CI run refuses them.
//! `FLEET_G20_TRACE=<dir>` keeps each instance's log there and prints what the database sessions
//! do every ten seconds while the load test's fleet connects; `FLEET_G20_SERVER_LOG` sets the
//! instances' `RUST_LOG`.
#![cfg(unix)]

mod ports;

use std::collections::{HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures_util::TryStreamExt as _;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::path::Path as Key;
use object_store::{ObjectStore as _, ObjectStoreExt as _};
use opamp::client::ws::{run_from, Connector, Settings};
use opamp::client::{AfterReply, Ended, ReportSink, Session, StopSignal};
use opamp::proto::{
    server_error_response, AgentCapabilities, AgentDescription, AgentToServer, AnyValue, KeyValue,
    ServerErrorResponseType, ServerToAgent, ServerToAgentFlags,
};
use opamp::uid::InstanceUid;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair, SanType, SerialNumber};
use tracing::Instrument as _;

/// G-20's failover fleet: 10 000 Agents, one on each connection.
const FAILOVER_AGENTS: usize = 10_000;
/// G-20's load fleet.
const LOAD_AGENTS: usize = 100_000;
/// G-20's load run: ten minutes.
const LOAD_SECS: u64 = 600;
/// The Agents each simulated host carries in the load test unless `FLEET_G20_AGENTS_PER_HOST`
/// says otherwise: ten, so that one machine holds the simulated fleet. One on each connection would
/// take 100 000 connections, about 200 KiB each in an instance and as much again in the simulator
/// and its balancer, measured at 10 000 and 20 000.
const LOAD_AGENTS_PER_HOST: usize = 10;
/// G-20's bound on the failover: `lease_secs` plus two heartbeat intervals at the defaults.
const CARRIED_WITHIN: Duration = Duration::from_secs(30 + 2 * 30);
/// G-20's bound on a revocation: one `lease_renew_secs` at the default.
const REVOKED_WITHIN: Duration = Duration::from_secs(10);
/// The default heartbeat, which every simulated Agent keeps.
const HEARTBEAT: Duration = Duration::from_secs(30);
/// The instances' `presence_interval_secs` at the default.
const PRESENCE_INTERVAL: Duration = Duration::from_secs(30);
/// The new connections a second the simulated fleet opens while it connects: below the three
/// instances' bound of 200 each, so no connection is refused for it.
const RAMP_PER_SEC: u64 = 450;
/// The least rate at which the instances hold the new Agents of a fleet that connects.
const NEW_AGENTS_PER_SEC: u64 = 150;
/// The Gateway's pool: ten connections from one address, each carrying ten Agents.
const GATEWAY_CONNECTIONS: usize = 10;
const GATEWAY_AGENTS_PER_CONNECTION: usize = 10;
/// The `service.name` every simulated Agent reports.
const SERVICE: &str = "otelcol";

fn variable(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut random = [0u8; N];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random).expect("random");
    random
}

fn random_hex() -> String {
    hex::encode(random_bytes::<8>())
}

/// A small generator for the test's own choices: which hosts, which Agents.
struct Draw(u64);

impl Draw {
    fn new() -> Self {
        Draw(u64::from_le_bytes(random_bytes::<8>()) | 1)
    }

    fn below(&mut self, bound: usize) -> usize {
        // xorshift64
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % bound as u64).expect("below a usize")
    }
}

/// Whether the stack is named; says so and skips where it is not.
fn stack_named() -> bool {
    if variable("FLEET_G20_STACK").as_deref() == Some("1") {
        return true;
    }
    eprintln!(
        "skipped: no G-20 stack — set FLEET_G20_STACK=1 beside the FLEET_TEST_POSTGRES_* and \
         FLEET_TEST_S3_* variables to run the tests that decide G-20"
    );
    false
}

/// The database the variables name, as its administrator, and a role of this test's own whose
/// default schema is the schema of its name; both are dropped with it.
struct Role {
    host: String,
    port: u16,
    dbname: String,
    user: String,
    password: String,
    ca_file: Option<PathBuf>,
    name: String,
}

impl Role {
    fn named() -> Self {
        let named = |name: &str| {
            variable(name).unwrap_or_else(|| panic!("FLEET_G20_STACK=1 needs {name} named"))
        };
        let role = Role {
            host: named("FLEET_TEST_POSTGRES_HOST"),
            port: variable("FLEET_TEST_POSTGRES_PORT")
                .map_or(5432, |port| port.parse().expect("FLEET_TEST_POSTGRES_PORT")),
            dbname: named("FLEET_TEST_POSTGRES_DBNAME"),
            user: named("FLEET_TEST_POSTGRES_USER"),
            password: std::fs::read_to_string(named("FLEET_TEST_POSTGRES_PASSWORD_FILE"))
                .expect("the password")
                .trim()
                .to_string(),
            ca_file: variable("FLEET_TEST_POSTGRES_CA_FILE").map(Into::into),
            name: format!("fleet_g20_{}", random_hex()),
        };
        role.sql(&format!(
            "CREATE ROLE {name} LOGIN PASSWORD '{name}'; \
             CREATE SCHEMA {name} AUTHORIZATION {name};",
            name = role.name
        ));
        role
    }

    /// Runs `statements` as the administrator, in the role's schema.
    fn sql(&self, statements: &str) {
        self.query(statements, None::<fn(&tokio_postgres::Row) -> ()>);
    }

    /// One number `query` answers, in the role's schema.
    fn number(&self, query: &str) -> i64 {
        self.query(
            query,
            Some(|row: &tokio_postgres::Row| row.get::<_, i64>(0)),
        )
        .expect("a number")
    }

    /// The first column of every row `query` answers, as text, in the role's schema.
    fn texts(&self, query: &str) -> Vec<String> {
        let wrapped =
            format!("SELECT coalesce(string_agg(t, E'\\n'), '') FROM ({query}) AS rows(t)");
        self.text(&wrapped).lines().map(str::to_string).collect()
    }

    /// What the role's sessions are doing, counted, for a run that traces the database.
    fn activity(&self) -> String {
        let mut counted: HashMap<String, usize> = HashMap::new();
        for line in self.texts(&format!(
            "SELECT coalesce(wait_event_type, '-') || '/' || coalesce(wait_event, '-') || ' ' || \
             coalesce(state, '-') || ' ' || left(regexp_replace(query, '\\s+', ' ', 'g'), 100) \
             FROM pg_stat_activity WHERE usename = '{}'",
            self.name
        )) {
            *counted.entry(line).or_default() += 1;
        }
        let mut lines: Vec<String> = counted
            .into_iter()
            .map(|(line, count)| format!("{count:>4} {line}"))
            .collect();
        lines.sort();
        lines.join("\n")
    }

    /// The mean time one statement takes from this process to the database and back, over one
    /// connection: what every call of an instance pays at least once.
    fn round_trip(&self) -> Duration {
        let mut pg = tokio_postgres::Config::new();
        pg.host(&self.host)
            .port(self.port)
            .dbname(&self.dbname)
            .user(&self.user)
            .password(&self.password)
            .connect_timeout(Duration::from_secs(10));
        let roots = self.ca_file.as_ref().map(|path| {
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
                    const TIMES: u32 = 50;
                    let started = Instant::now();
                    for _ in 0..TIMES {
                        client.simple_query("SELECT 1").await.expect("a round trip");
                    }
                    started.elapsed() / TIMES
                })
        })
        .join()
        .expect("the round trips")
    }

    /// One text `query` answers, in the role's schema.
    fn text(&self, query: &str) -> String {
        self.query(
            query,
            Some(|row: &tokio_postgres::Row| row.get::<_, String>(0)),
        )
        .expect("a text")
    }

    /// Runs `statement` as the administrator, in the role's schema; with `read`, it is a query of
    /// one row, read by it.
    fn query<T: Send + 'static>(
        &self,
        statement: &str,
        read: Option<impl FnOnce(&tokio_postgres::Row) -> T + Send + 'static>,
    ) -> Option<T> {
        let mut pg = tokio_postgres::Config::new();
        pg.host(&self.host)
            .port(self.port)
            .dbname(&self.dbname)
            .user(&self.user)
            .password(&self.password)
            .connect_timeout(Duration::from_secs(10));
        let path = format!("SET search_path = {}", self.name);
        let statement = statement.to_string();
        let roots = self.ca_file.as_ref().map(|path| {
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
                    client.batch_execute(&path).await.expect("search path");
                    match read {
                        Some(read) => Some(read(
                            &client
                                .query_one(statement.as_str(), &[])
                                .await
                                .unwrap_or_else(|e| panic!("{statement}: {e}")),
                        )),
                        None => {
                            client
                                .batch_execute(&statement)
                                .await
                                .unwrap_or_else(|e| panic!("{statement}: {e}"));
                            None
                        }
                    }
                })
        })
        .join()
        .expect("the statement's thread")
    }

    /// The same administrator and role, for a thread of its own; dropping it drops nothing.
    fn admin_of(role: &Role) -> Role {
        Role {
            host: role.host.clone(),
            port: role.port,
            dbname: role.dbname.clone(),
            user: role.user.clone(),
            password: role.password.clone(),
            ca_file: role.ca_file.clone(),
            name: role.name.clone(),
        }
    }

    /// `[database.postgres]` for the role, its password in a file under `dir`.
    fn section(&self, dir: &Path) -> String {
        let password_file = dir.join("database-password");
        std::fs::write(&password_file, &self.name).expect("password");
        std::fs::set_permissions(&password_file, std::fs::Permissions::from_mode(0o600))
            .expect("chmod");
        let tls = match &self.ca_file {
            Some(ca) => format!("ca_file = {:?}\n", ca.display().to_string()),
            None => "tls = false\n".to_string(),
        };
        format!(
            "[database.postgres]\nhost = {:?}\nport = {}\ndbname = {:?}\nuser = {:?}\n\
             password_file = {:?}\n{tls}",
            self.host,
            self.port,
            self.dbname,
            self.name,
            password_file.display().to_string(),
        )
    }

    /// How much of the queue of notifications no listener has read yet, in millionths.
    fn unread_notifications(&self) -> i64 {
        self.number("SELECT (pg_notification_queue_usage() * 1000000)::bigint")
    }
}

impl Drop for Role {
    fn drop(&mut self) {
        self.sql(&format!(
            "SET search_path = public; DROP SCHEMA IF EXISTS {name} CASCADE; \
             DROP ROLE IF EXISTS {name};",
            name = self.name
        ));
    }
}

/// The bucket the variables name, under a prefix of this test's own, emptied afterwards.
struct Bucket {
    section: String,
    prefix: String,
    store: AmazonS3,
}

impl Bucket {
    fn named() -> Self {
        let named = |name: &str| {
            variable(name).unwrap_or_else(|| panic!("FLEET_G20_STACK=1 needs {name} named"))
        };
        let (endpoint, bucket) = (
            named("FLEET_TEST_S3_ENDPOINT"),
            named("FLEET_TEST_S3_BUCKET"),
        );
        let region = variable("FLEET_TEST_S3_REGION").unwrap_or_else(|| "us-east-1".to_string());
        let prefix = format!("g20-{}/", random_hex());
        let mut section = format!(
            "[blobs]\nstore = \"s3\"\n[blobs.s3]\nendpoint = {endpoint:?}\nbucket = {bucket:?}\n\
             region = {region:?}\nprefix = {prefix:?}\n"
        );
        let mut builder = AmazonS3Builder::new()
            .with_endpoint(&endpoint)
            .with_bucket_name(&bucket)
            .with_region(&region)
            .with_allow_http(endpoint.starts_with("http://"));
        if let (Some(id), Some(file)) = (
            variable("FLEET_TEST_S3_ACCESS_KEY_ID"),
            variable("FLEET_TEST_S3_SECRET_ACCESS_KEY_FILE"),
        ) {
            section += &format!("access_key_id = {id:?}\nsecret_access_key_file = {file:?}\n");
            builder = builder
                .with_access_key_id(&id)
                .with_secret_access_key(std::fs::read_to_string(&file).expect("the secret").trim());
        }
        Bucket {
            section,
            prefix,
            store: builder.build().expect("an S3 client"),
        }
    }
}

impl Drop for Bucket {
    fn drop(&mut self) {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let root = Key::from(self.prefix.trim_end_matches('/'));
        let keys = runtime
            .block_on(self.store.list(Some(&root)).try_collect::<Vec<_>>())
            .unwrap_or_default();
        for object in keys {
            let _ = runtime.block_on(self.store.delete(&object.location));
        }
    }
}

/// The client CA and the instances' own certificate.
struct Pki {
    ca_pem: String,
    ca_key_pem: String,
    server_pem: String,
    server_key_pem: String,
}

/// One simulated host's client material.
struct HostTls {
    name: String,
    serial: u64,
    connector: Connector,
}

impl Pki {
    fn new() -> Self {
        let key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "opamp-fleet-g20-ca");
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = params.self_signed(&key).expect("ca");
        let server = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_string()])
            .expect("the instances' certificate");
        Pki {
            ca_pem: ca.pem(),
            ca_key_pem: key.serialize_pem(),
            server_pem: server.cert.pem(),
            server_key_pem: server.signing_key.serialize_pem(),
        }
    }

    /// The client material of `count` hosts, `name(i)` each, with serials from `first`, made on
    /// every core.
    fn hosts(
        &self,
        count: usize,
        first: u64,
        name: impl Fn(usize) -> String + Sync,
    ) -> Vec<HostTls> {
        let threads = std::thread::available_parallelism().map_or(4, usize::from);
        let chunk = count.div_ceil(threads).max(1);
        let roots = opamp::tls::root_store(self.server_pem.as_bytes()).expect("roots");
        let name = &name;
        std::thread::scope(|scope| {
            let parts: Vec<_> = (0..count)
                .step_by(chunk)
                .map(|start| {
                    let roots = roots.clone();
                    scope.spawn(move || {
                        let issuer = Issuer::from_ca_cert_pem(
                            &self.ca_pem,
                            KeyPair::from_pem(&self.ca_key_pem).expect("ca key"),
                        )
                        .expect("issuer");
                        (start..(start + chunk).min(count))
                            .map(|i| {
                                let host = name(i);
                                let serial = first + i as u64;
                                let key = KeyPair::generate().expect("key");
                                let mut params =
                                    CertificateParams::new(Vec::<String>::new()).expect("params");
                                params
                                    .distinguished_name
                                    .push(rcgen::DnType::CommonName, host.as_str());
                                params.subject_alt_names = vec![SanType::URI(
                                    format!("urn:opamp-fleet:host:{host}")
                                        .try_into()
                                        .expect("a host URI"),
                                )];
                                params.serial_number = Some(SerialNumber::from(serial));
                                let cert = params.signed_by(&key, &issuer).expect("signed");
                                let config = opamp::tls::client_builder()
                                    .with_root_certificates(roots.clone())
                                    .with_client_auth_cert(
                                        vec![cert.der().clone()],
                                        rustls::pki_types::PrivateKeyDer::Pkcs8(
                                            key.serialize_der().into(),
                                        ),
                                    )
                                    .expect("client auth");
                                HostTls {
                                    name: host,
                                    serial,
                                    connector: Connector::Rustls(Arc::new(config)),
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            parts
                .into_iter()
                .flat_map(|part| part.join().expect("certificates"))
                .collect()
        })
    }
}

/// One `server` process, killed on drop.
struct Instance {
    child: Option<Child>,
    agent: SocketAddr,
    operator: u16,
    log: Arc<Mutex<Vec<String>>>,
    _files: tempfile::TempDir,
}

impl Instance {
    fn start(pki: &Pki, role: &Role, bucket: &Bucket) -> Self {
        let files = tempfile::tempdir().expect("tempdir");
        for (file, pem) in [
            ("server.pem", &pki.server_pem),
            ("server-key.pem", &pki.server_key_pem),
            ("ca.pem", &pki.ca_pem),
        ] {
            std::fs::write(files.path().join(file), pem).expect("write");
        }
        let (agent, operator) = (ports::spare_port(), ports::spare_port());
        let path = |file: &str| files.path().join(file).display().to_string();
        let config = files.path().join("server.toml");
        std::fs::write(
            &config,
            format!(
                "listen = \"127.0.0.1:{agent}\"\nmax_connections = 80000\nmax_agents = 250000\n\
                 [tls]\ncert_file = {cert:?}\nkey_file = {key:?}\nclient_ca_file = {ca:?}\n\
                 [rest]\nlisten = \"127.0.0.1:{operator}\"\n\
                 [database]\nstore = \"postgres\"\n{database}{blobs}",
                cert = path("server.pem"),
                key = path("server-key.pem"),
                ca = path("ca.pem"),
                database = role.section(files.path()),
                blobs = bucket.section,
            ),
        )
        .expect("write server.toml");
        let mut child = Command::new(env!("CARGO_BIN_EXE_server"))
            .arg("--config")
            .arg(&config)
            .env(
                "RUST_LOG",
                variable("FLEET_G20_SERVER_LOG").unwrap_or_else(|| "info".to_string()),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start the server");
        let log = Arc::new(Mutex::new(Vec::new()));
        let (up, came_up) = std::sync::mpsc::channel();
        let stderr = child.stderr.take().expect("stderr");
        let lines = log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.contains("serving the REST API") {
                    let _ = up.send(());
                }
                lines.lock().expect("log").push(line);
            }
        });
        let instance = Instance {
            child: Some(child),
            agent: SocketAddr::from(([127, 0, 0, 1], agent)),
            operator,
            log,
            _files: files,
        };
        assert!(
            came_up.recv_timeout(Duration::from_secs(180)).is_ok(),
            "an instance never came up:\n{}",
            instance.log.lock().expect("log").join("\n")
        );
        instance
    }

    /// Stops the process without warning.
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            child.kill().expect("SIGKILL");
            let _ = child.wait();
        }
    }

    /// Freezes the process without warning (`SIGSTOP`): it closes nothing and answers nothing,
    /// while its kernel keeps its connections. Dropping it kills it.
    fn freeze(&self) {
        if let Some(child) = self.child.as_ref() {
            let stopped = Command::new("kill")
                .arg("-STOP")
                .arg(child.id().to_string())
                .status()
                .expect("run kill");
            assert!(stopped.success(), "SIGSTOP");
        }
    }

    /// Its resident memory, in MiB.
    fn rss_mib(&self) -> u64 {
        self.child
            .as_ref()
            .map_or(0, |child| rss_mib(&format!("/proc/{}/status", child.id())))
    }

    /// The lines it logged from `from` on that contain `text`.
    fn logged(&self, from: usize, text: &str) -> Vec<String> {
        self.log
            .lock()
            .expect("log")
            .iter()
            .skip(from)
            .filter(|line| line.contains(text))
            .cloned()
            .collect()
    }

    fn log_len(&self) -> usize {
        self.log.lock().expect("log").len()
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.kill();
        // A run that traces keeps what each instance logged, under the directory it names.
        if let Some(dir) = variable("FLEET_G20_TRACE") {
            let _ = std::fs::create_dir_all(&dir);
            let file = Path::new(&dir).join(format!("instance-{}.log", self.operator));
            let _ = std::fs::write(file, self.log.lock().expect("log").join("\n"));
        }
    }
}

fn rss_mib(status: &str) -> u64 {
    std::fs::read_to_string(status)
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kib| kib.parse::<u64>().ok())
        })
        .map_or(0, |kib| kib / 1024)
}

/// Where the balancer prefers to send `peer`, best first: the highest hash of the peer's address
/// and the instance (rendezvous hashing), so the peers of a lost instance spread over the others
/// and no other peer moves.
fn preference(peer: IpAddr, backends: usize) -> Vec<usize> {
    let mut order: Vec<(u64, usize)> = (0..backends)
        .map(|backend| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (peer, backend).hash(&mut hasher);
            (hasher.finish(), backend)
        })
        .collect();
    order.sort_unstable_by(|a, b| b.cmp(a));
    order.into_iter().map(|(_, backend)| backend).collect()
}

/// A layer-4 balancer that keeps the peer's address: source-address affinity over the instances
/// that answer its probe, and each connection to an instance made from the peer's own address.
struct Balancer {
    address: SocketAddr,
    /// The instance each peer address was last carried to.
    placed: Arc<Mutex<HashMap<IpAddr, usize>>>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl Balancer {
    fn start(backends: Vec<SocketAddr>) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("balancer")
            .enable_all()
            .build()
            .expect("the balancer's runtime");
        let backends = Arc::new(backends);
        let up: Arc<Vec<AtomicBool>> =
            Arc::new(backends.iter().map(|_| AtomicBool::new(true)).collect());
        let placed = Arc::new(Mutex::new(HashMap::new()));
        let listener = runtime.block_on(async {
            let socket = tokio::net::TcpSocket::new_v4().expect("socket");
            socket.set_reuseaddr(true).expect("reuseaddr");
            socket.bind(([127, 0, 0, 1], 0).into()).expect("bind");
            socket.listen(8192).expect("listen")
        });
        let address = listener.local_addr().expect("address");
        // The probe: an instance that does not answer gets no connection. A TCP connection alone
        // would not tell, since the kernel of a frozen process still accepts it; the probe sends
        // bytes that are no TLS and waits for the instance to answer or close.
        for (backend, &target) in backends.iter().enumerate() {
            let up = up.clone();
            runtime.spawn(async move {
                loop {
                    let answers = tokio::time::timeout(Duration::from_secs(1), async {
                        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                        let mut stream = tokio::net::TcpStream::connect(target).await.ok()?;
                        stream.write_all(b"probe\r\n\r\n").await.ok()?;
                        let mut answer = [0u8; 64];
                        stream.read(&mut answer).await.ok()
                    })
                    .await
                    .is_ok_and(|answered| answered.is_some());
                    up[backend].store(answers, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            });
        }
        let carried = placed.clone();
        runtime.spawn(async move {
            loop {
                let (client, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        eprintln!("balancer: accept: {e}");
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                };
                tokio::spawn(carry(
                    client,
                    peer,
                    backends.clone(),
                    up.clone(),
                    carried.clone(),
                ));
            }
        });
        Balancer {
            address,
            placed,
            runtime: Some(runtime),
        }
    }

    fn placed(&self) -> HashMap<IpAddr, usize> {
        self.placed.lock().expect("placed").clone()
    }
}

impl Drop for Balancer {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

/// Carries one connection to the first instance of the peer's preference that answers, from the
/// peer's own address.
async fn carry(
    mut client: tokio::net::TcpStream,
    peer: SocketAddr,
    backends: Arc<Vec<SocketAddr>>,
    up: Arc<Vec<AtomicBool>>,
    placed: Arc<Mutex<HashMap<IpAddr, usize>>>,
) {
    for backend in preference(peer.ip(), backends.len()) {
        if !up[backend].load(Ordering::SeqCst) {
            continue;
        }
        let Ok(socket) = tokio::net::TcpSocket::new_v4() else {
            return;
        };
        if socket.bind(SocketAddr::new(peer.ip(), 0)).is_err() {
            return;
        }
        let Ok(Ok(mut upstream)) =
            tokio::time::timeout(Duration::from_secs(2), socket.connect(backends[backend])).await
        else {
            continue;
        };
        let _ = client.set_nodelay(true);
        let _ = upstream.set_nodelay(true);
        placed.lock().expect("placed").insert(peer.ip(), backend);
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        return;
    }
}

/// What one simulated host's connection went through, as its driver logged it and its session saw.
#[derive(Default)]
struct HostLog {
    /// Each time a connection came up and was admitted.
    connects: Vec<Instant>,
    /// Each lost connection, with the delay the driver drew for it.
    losses: Vec<(Instant, Option<u64>)>,
    /// Each refusal: an admission that failed, or a throttled one.
    refusals: Vec<(Instant, String)>,
}

/// The logs of every host of one test.
struct Record {
    hosts: Vec<Mutex<HostLog>>,
}

impl Record {
    fn new(hosts: usize) -> Arc<Self> {
        Arc::new(Record {
            hosts: (0..hosts).map(|_| Mutex::default()).collect(),
        })
    }

    fn host(&self, host: usize) -> std::sync::MutexGuard<'_, HostLog> {
        self.hosts[host].lock().expect("host log")
    }
}

/// The record the driver's log lines go to.
static RECORDING: Mutex<Option<Arc<Record>>> = Mutex::new(None);

/// The host whose driver a span runs.
struct HostSpan(usize);

/// Sends the log lines of each host's driver to its [`HostLog`].
struct Hosts;

#[derive(Default)]
struct Fields {
    host: Option<usize>,
    message: String,
    delay_ms: Option<u64>,
}

impl tracing::field::Visit for Fields {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "host" => self.host = usize::try_from(value).ok(),
            "delay_ms" => self.delay_ms = Some(value),
            _ => {}
        }
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.record_u64(field, u64::try_from(value).unwrap_or(0));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for Hosts
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        if let (Some(host), Some(span)) = (fields.host, ctx.span(id)) {
            span.extensions_mut().insert(HostSpan(host));
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
        let Some(host) = ctx.event_scope(event).and_then(|mut scope| {
            scope.find_map(|span| span.extensions().get::<HostSpan>().map(|host| host.0))
        }) else {
            return;
        };
        let mut fields = Fields::default();
        event.record(&mut fields);
        let Some(record) = RECORDING.lock().expect("recording").clone() else {
            return;
        };
        let now = Instant::now();
        if fields.message.contains("connection lost; reconnecting") {
            record.host(host).losses.push((now, fields.delay_ms));
        } else if fields.message.contains("admission failed")
            || fields.message.contains("throttling")
        {
            record.host(host).refusals.push((now, fields.message));
        }
    }
}

/// Installs the [`Hosts`] layer once for the test binary, and records into `record` from now on.
fn record_into(record: &Arc<Record>) {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        use tracing_subscriber::layer::SubscriberExt as _;
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(
            tracing_subscriber::Layer::with_filter(
                Hosts,
                tracing_subscriber::filter::LevelFilter::INFO,
            ),
        ))
        .expect("the test's subscriber");
    });
    *RECORDING.lock().expect("recording") = Some(record.clone());
}

/// One simulated Agent.
struct SimAgent {
    uid: InstanceUid,
    sequence_num: u64,
    /// Whether it reports `g20.group = rollout`, which the load test's Deployment aims at.
    rollout: bool,
    /// Whether the Server asked for its full state.
    owes_full: bool,
}

fn pair(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_string(),
        value: Some(AnyValue {
            value: Some(opamp::proto::any_value::Value::StringValue(
                value.to_string(),
            )),
        }),
    }
}

impl SimAgent {
    fn new(rollout: bool) -> Self {
        SimAgent {
            uid: InstanceUid::from_wire(&random_bytes::<16>()).expect("a uid"),
            sequence_num: 0,
            rollout,
            owes_full: false,
        }
    }

    fn capabilities() -> u64 {
        AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::AcceptsRemoteConfig as u64
            | AgentCapabilities::ReportsHeartbeat as u64
    }

    fn report(&mut self, host: &str, full: bool) -> AgentToServer {
        self.sequence_num += 1;
        self.owes_full = false;
        AgentToServer {
            instance_uid: self.uid.as_bytes().to_vec(),
            sequence_num: self.sequence_num,
            capabilities: Self::capabilities(),
            agent_description: full.then(|| AgentDescription {
                identifying_attributes: vec![
                    pair("service.name", SERVICE),
                    pair("service.instance.id", &self.uid.to_string()),
                ],
                non_identifying_attributes: vec![
                    pair("host.name", host),
                    pair("os.type", "linux"),
                    pair("g20.group", if self.rollout { "rollout" } else { "none" }),
                ],
            }),
            ..Default::default()
        }
    }
}

/// One simulated host's session: its Agents, a full report each on connect and a heartbeat each
/// at the default interval.
struct Sim {
    host: usize,
    name: String,
    agents: Vec<SimAgent>,
    record: Arc<Record>,
    /// Set to have the host drop its connection and connect again at its next reply.
    churn: Arc<AtomicBool>,
}

impl Session for Sim {
    fn connected(&mut self) -> Vec<AgentToServer> {
        self.record.host(self.host).connects.push(Instant::now());
        let name = self.name.clone();
        self.agents
            .iter_mut()
            .map(|agent| agent.report(&name, true))
            .collect()
    }

    fn routine(&mut self) -> Vec<AgentToServer> {
        let name = self.name.clone();
        self.agents
            .iter_mut()
            .map(|agent| agent.report(&name, false))
            .collect()
    }

    fn owed(&mut self) -> Vec<AgentToServer> {
        let name = self.name.clone();
        self.agents
            .iter_mut()
            .filter(|agent| agent.owes_full)
            .map(|agent| agent.report(&name, true))
            .collect()
    }

    fn on_reply(&mut self, reply: &ServerToAgent) -> Option<Duration> {
        // As the `opamp` client settles a reply (`client::protocol`): a command carries nothing
        // else, and an error ends the reply there; `Unavailable` keeps the host away for the time
        // it names, half a minute without a hint, and counts as a refusal.
        if reply.command.is_some() {
            return None;
        }
        if let Some(response) = &reply.error_response {
            if response.r#type != ServerErrorResponseType::Unavailable as i32 {
                return None;
            }
            let nanos = match &response.details {
                Some(server_error_response::Details::RetryInfo(info)) => {
                    info.retry_after_nanoseconds
                }
                _ => 30_000_000_000,
            };
            tracing::warn!(
                "the server answered unavailable ({}); throttling",
                response.error_message
            );
            return Some(Duration::from_nanos(nanos));
        }
        let agent = self
            .agents
            .iter_mut()
            .find(|agent| agent.uid.as_bytes().as_slice() == reply.instance_uid.as_slice())?;
        if reply.flags & ServerToAgentFlags::ReportFullState as u64 != 0 {
            agent.owes_full = true;
        }
        // An identity the Server assigns is the Agent's from here on, as the `opamp` client
        // adopts it.
        if let Some(new) = reply
            .agent_identification
            .as_ref()
            .and_then(|identification| InstanceUid::from_wire(&identification.new_instance_uid))
        {
            agent.uid = new;
        }
        None
    }

    async fn after_reply<K: ReportSink>(&mut self, _: &mut K) -> AfterReply {
        if self.churn.swap(false, Ordering::SeqCst) {
            AfterReply::Reconnect
        } else {
            AfterReply::Continue
        }
    }

    async fn changed(&mut self) {
        std::future::pending().await
    }

    fn exchange_failed(&mut self) {
        for agent in &mut self.agents {
            agent.owes_full = true;
        }
    }

    async fn stop(&mut self) {}

    fn goodbyes(&mut self) -> Vec<AgentToServer> {
        Vec::new()
    }
}

/// The end of a test's fleet.
struct Stop(tokio::sync::watch::Receiver<bool>);

impl StopSignal for Stop {
    fn requested(&mut self) -> impl std::future::Future<Output = ()> + Send {
        let mut stopped = self.0.clone();
        async move {
            let _ = stopped.wait_for(|stopped| *stopped).await;
        }
    }
}

/// One simulated host as the fleet knows it: its address, its certificate's serial, its Agents'
/// identities, and the switch that makes it reconnect.
struct SimHost {
    address: IpAddr,
    serial: u64,
    uids: Vec<InstanceUid>,
    churn: Arc<AtomicBool>,
}

/// The loopback address of simulated host `i`: 127.10.0.1 on, never the loopback's own.
fn host_address(i: usize) -> IpAddr {
    let n = (10u32 << 16) + 1 + u32::try_from(i).expect("a host number");
    IpAddr::V4(Ipv4Addr::from((127u32 << 24) | n))
}

/// The simulated fleet.
struct Fleet {
    hosts: Vec<SimHost>,
    record: Arc<Record>,
    stop: tokio::sync::watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Fleet {
    fn new(record: Arc<Record>) -> Self {
        Fleet {
            hosts: Vec::new(),
            record,
            stop: tokio::sync::watch::channel(false).0,
            tasks: Vec::new(),
        }
    }

    /// Starts a host's connection from `address`, carrying `agents`; returns its number.
    fn connect(
        &mut self,
        endpoint: &str,
        address: IpAddr,
        tls: &HostTls,
        agents: Vec<SimAgent>,
    ) -> usize {
        let host = self.hosts.len();
        let churn = Arc::new(AtomicBool::new(false));
        self.hosts.push(SimHost {
            address,
            serial: tls.serial,
            uids: agents.iter().map(|agent| agent.uid).collect(),
            churn: churn.clone(),
        });
        let settings = Settings {
            endpoint: endpoint.to_string(),
            headers: Default::default(),
            connector: Some(tls.connector.clone()),
            max_message_size: 4 << 20,
            heartbeat: Some(HEARTBEAT),
        };
        let mut session = Sim {
            host,
            name: tls.name.clone(),
            agents,
            record: self.record.clone(),
            churn,
        };
        let mut stop = Stop(self.stop.subscribe());
        self.tasks.push(tokio::spawn(
            async move {
                // A reconnect the host asked for (churn) is made at once; anything else ends it.
                while let Ok(Ended::Reconnect) =
                    run_from(&settings, Some(address), &mut session, &mut stop).await
                {
                }
            }
            .instrument(tracing::info_span!("host", host = host as u64)),
        ));
        host
    }

    /// The hosts whose connection has come up at least once.
    fn connected_hosts(&self) -> usize {
        (0..self.hosts.len())
            .filter(|&host| !self.record.host(host).connects.is_empty())
            .count()
    }

    fn agents(&self) -> usize {
        self.hosts.iter().map(|host| host.uids.len()).sum()
    }

    async fn stop(self) {
        let _ = self.stop.send(true);
        for task in self.tasks {
            let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
        }
    }
}

/// What went wrong, for a failed wait: the hosts' refusals and losses, and what the instances
/// warned of last.
fn diagnosis(fleet: &Fleet, instances: &[Instance]) -> String {
    let mut refusals: HashMap<String, usize> = HashMap::new();
    let (mut losses, mut never) = (0, 0);
    for host in 0..fleet.hosts.len() {
        let log = fleet.record.host(host);
        for (_, why) in &log.refusals {
            *refusals.entry(why.clone()).or_default() += 1;
        }
        losses += log.losses.len();
        never += usize::from(log.connects.is_empty());
    }
    let mut text =
        format!("hosts never connected {never}, losses {losses}, refusals {refusals:?}\n");
    for (i, instance) in instances.iter().enumerate() {
        let log = instance.log.lock().expect("log");
        let warned: Vec<&String> = log
            .iter()
            .filter(|line| line.contains("WARN") || line.contains("ERROR"))
            .collect();
        text += &format!("instance {i}: {} warnings, the last:\n", warned.len());
        for line in warned.iter().rev().take(15).rev() {
            text += &format!("  {line}\n");
        }
    }
    text
}

/// Starts `count` hosts of `per_host` Agents each, `per_sec` connections a second, every
/// `rollout_every`-th Agent in the rollout's group.
async fn ramp(
    fleet: &mut Fleet,
    endpoint: &str,
    tls: &[HostTls],
    per_host: usize,
    rollout_every: usize,
    per_sec: u64,
) {
    let started = Instant::now();
    for (i, host) in tls.iter().enumerate() {
        let due = started + Duration::from_millis(i as u64 * 1000 / per_sec);
        tokio::time::sleep_until(due.into()).await;
        let agents = (0..per_host)
            .map(|a| SimAgent::new((i * per_host + a).is_multiple_of(rollout_every)))
            .collect();
        fleet.connect(endpoint, host_address(i), host, agents);
    }
}

/// Waits, a second at a time, until `done` answers true, for at most `within`; answers whether it
/// did.
async fn eventually(within: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// A client of the instances' Operator planes.
fn operator_client(pki: &Pki) -> reqwest::Client {
    reqwest::Client::builder()
        .tls_certs_only([
            reqwest::Certificate::from_pem(pki.server_pem.as_bytes()).expect("the root")
        ])
        .timeout(Duration::from_secs(60))
        .build()
        .expect("a client")
}

fn operator_url(instance: &Instance, path: &str) -> String {
    format!("https://127.0.0.1:{}/api/v1/{path}", instance.operator)
}

/// Revokes host `host`'s certificate through `instance`.
async fn revoke(client: &reqwest::Client, instance: &Instance, host: &SimHost) {
    let answer = client
        .post(operator_url(instance, "revocations"))
        .json(&serde_json::json!({
            "certificate": {"authority": "client", "serial": format!("{:x}", host.serial)}
        }))
        .send()
        .await
        .expect("revoke");
    let status = answer.status();
    assert!(
        status.is_success(),
        "the revocation is taken: {status} {}",
        answer.text().await.unwrap_or_default()
    );
}

/// The sentinels each live instance is sent, how far apart they go, and how soon each has to end
/// its session.
const SENTINELS: usize = 4;
const SENTINEL_EVERY: Duration = Duration::from_millis(2_500);
const SENTINEL_WITHIN: Duration = Duration::from_secs(2);

/// Whether each live instance reads its notifications and handles them in order: for each instance
/// in `live`, [`SENTINELS`] hosts it carries are revoked through another live instance,
/// [`SENTINEL_EVERY`] apart, and each host's session has to end within [`SENTINEL_WITHIN`]. Over
/// the listener that takes some 0.1 to 0.3 s.
///
/// A revocation has a second way to an instance: each lease renewal reads the revocation list's
/// version and reads the ledger again when it moved, at most one renewal every `lease_renew_secs`
/// (10 s at the default) while renewals succeed. The sentinels are spaced so that one renewal ends
/// at most one of them in time: four, 2.5 s apart, span 7.5 s, and with the 2 s each may take,
/// less than one renewal interval. So an instance whose listener reads nothing, or whose handling
/// is stuck or lags, fails at least three of its four. What it proves for the other kinds of
/// notification is that the listener reads and the handling keeps up, as they share both; it
/// does not apply a label or an assignment itself.
///
/// A host counts as carried when its last connection came up after its last loss, it was never
/// refused, and no reconnect is pending for it. Answers the hosts revoked and how long each took.
async fn heard_by_every(
    client: &reqwest::Client,
    instances: &[Instance],
    live: &[usize],
    placed: &HashMap<IpAddr, usize>,
    fleet: &Fleet,
    record: &Record,
    skip: &HashSet<usize>,
) -> Vec<(usize, Duration)> {
    let mut used = skip.clone();
    let mut heard = Vec::new();
    for round in 0..SENTINELS {
        let started = Instant::now();
        let mut sent = Vec::new();
        for &carrier in live {
            let through = *live
                .iter()
                .find(|&&other| other != carrier)
                .expect("another live instance");
            let host = (0..fleet.hosts.len())
                .find(|&host| {
                    let log = record.host(host);
                    !used.contains(&host)
                        && placed.get(&fleet.hosts[host].address) == Some(&carrier)
                        && !fleet.hosts[host].churn.load(Ordering::SeqCst)
                        && log.refusals.is_empty()
                        && log
                            .connects
                            .last()
                            .is_some_and(|&up| log.losses.last().is_none_or(|&(lost, _)| up > lost))
                })
                .unwrap_or_else(|| panic!("a connected host instance {carrier} carries"));
            used.insert(host);
            let at = Instant::now();
            revoke(client, &instances[through], &fleet.hosts[host]).await;
            sent.push((carrier, through, host, at));
        }
        for (carrier, through, host, at) in sent {
            let deadline = at + SENTINEL_WITHIN + Duration::from_secs(1);
            let took = loop {
                let took = record
                    .host(host)
                    .losses
                    .iter()
                    .find(|&&(lost, _)| lost >= at)
                    .map(|&(lost, _)| lost - at);
                if took.is_some() || Instant::now() >= deadline {
                    break took;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            assert!(
                took.is_some_and(|took| took <= SENTINEL_WITHIN),
                "instance {carrier} heard and handled sentinel {round} committed through instance \
                 {through} within {SENTINEL_WITHIN:?}: host {host}'s session ended after {took:?}"
            );
            heard.push((host, took.expect("ended")));
        }
        tokio::time::sleep_until((started + SENTINEL_EVERY).into()).await;
    }
    heard
}

/// The instances' and this process's resident memory, for the report.
fn memory(instances: &[Instance]) -> String {
    format!(
        "instances {} MiB, simulator and balancer {} MiB",
        instances
            .iter()
            .map(|instance| instance.rss_mib().to_string())
            .collect::<Vec<_>>()
            .join(" + "),
        rss_mib("/proc/self/status"),
    )
}

fn in_list(uids: impl IntoIterator<Item = InstanceUid>) -> String {
    let quoted: Vec<String> = uids.into_iter().map(|uid| format!("'{uid}'")).collect();
    if quoted.is_empty() {
        "('')".to_string()
    } else {
        format!("({})", quoted.join(","))
    }
}

/// G-20's failover: of 10 000 simulated Agents, one on each connection, across three instances,
/// one instance is killed. Every Agent is held within 60 seconds of the last connection opened, at
/// least 150 new Agents a second, and none is re-keyed as a duplicate, which an Agent whose
/// connection was ended for silence would be. Every Agent the killed instance carried is carried by the other two within `lease_secs`
/// plus two heartbeat intervals, with one holder write each, none reconnecting before the delay it
/// drew; a revocation committed meanwhile ends its session on either instance within
/// `lease_renew_secs`; no notification is left unread; and a Gateway's ten pool connections from
/// one address are refused by neither the admission throttle nor the bound on new connections.
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
#[test]
fn g20_a_killed_instance_s_agents_are_carried_by_the_others_within_the_bounds() {
    failover(Loss::Killed);
}

/// G-20's failover with the instance frozen (`SIGSTOP`) rather than killed: it closes nothing, its
/// kernel keeps the connections, and its Agents notice it only by the silence of one heartbeat
/// interval. The same bounds hold as for a killed instance.
/// Verifies: ADR-draft-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
#[test]
fn g20_a_frozen_instance_s_agents_are_carried_by_the_others_within_the_bounds() {
    failover(Loss::Frozen);
}

/// How the failover loses its instance.
#[derive(Clone, Copy, Debug)]
enum Loss {
    /// `SIGKILL`: the kernel resets every connection.
    Killed,
    /// `SIGSTOP`: nothing is closed and nothing answers.
    Frozen,
}

fn failover(loss: Loss) {
    if !stack_named() {
        return;
    }
    opamp::tls::install_ring_provider();
    let role = Role::named();
    let bucket = Bucket::named();
    let pki = Pki::new();
    let made = Instant::now();
    let tls = pki.hosts(FAILOVER_AGENTS, 1_000, |i| format!("sim-{i}"));
    let gateway_tls = pki
        .hosts(1, 900, |_| "sim-gateway".to_string())
        .pop()
        .expect("the Gateway's certificate");
    eprintln!(
        "made {} certificates in {:?}",
        tls.len() + 1,
        made.elapsed()
    );

    let mut instances: Vec<Instance> = (0..3)
        .map(|_| Instance::start(&pki, &role, &bucket))
        .collect();
    let balancer = Balancer::start(instances.iter().map(|instance| instance.agent).collect());
    let endpoint = format!("wss://127.0.0.1:{}/v1/opamp", balancer.address.port());
    // The instance that is killed is the third; the Gateway's address is one it prefers.
    const KILLED: usize = 2;
    let gateway_address = (0..)
        .map(|i| IpAddr::V4(Ipv4Addr::new(127, 20, 0, 1 + i)))
        .find(|address| preference(*address, 3)[0] == KILLED)
        .expect("an address the third instance is preferred for");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the fleet's runtime");
    let record = Record::new(FAILOVER_AGENTS + GATEWAY_CONNECTIONS);
    record_into(&record);
    let client = operator_client(&pki);
    runtime.block_on(async {
        let mut fleet = Fleet::new(record.clone());
        let started = Instant::now();
        ramp(&mut fleet, &endpoint, &tls, 1, usize::MAX, RAMP_PER_SEC).await;
        let gateway: Vec<usize> = (0..GATEWAY_CONNECTIONS)
            .map(|_| {
                let agents = (0..GATEWAY_AGENTS_PER_CONNECTION)
                    .map(|_| SimAgent::new(false))
                    .collect();
                fleet.connect(&endpoint, gateway_address, &gateway_tls, agents)
            })
            .collect();
        // Every connection is opened by now: what the fleet takes to be held is counted from here.
        let opened = started.elapsed();
        let agents = fleet.agents();
        assert!(
            eventually(Duration::from_secs(180), || fleet.connected_hosts()
                == fleet.hosts.len())
            .await,
            "the fleet connected: {} of {} hosts\n{}",
            fleet.connected_hosts(),
            fleet.hosts.len(),
            diagnosis(&fleet, &instances)
        );
        let held = |role: &Role| {
            role.number("SELECT count(*) FROM agents WHERE connected AND holder IS NOT NULL")
                as usize
        };
        // Every Agent is held within 60 seconds of the last connection opened.
        let bound = (opened + Duration::from_secs(60)).saturating_sub(started.elapsed());
        assert!(
            eventually(bound, || held(&role) == agents).await,
            "every Agent is held within 60 s of the last connection opened: {} of {agents} after \
             {:?}, {:?} after it; a database round trip {:?}",
            held(&role),
            started.elapsed(),
            started.elapsed().saturating_sub(opened),
            role.round_trip()
        );
        let ramp_took = started.elapsed();
        let rate = agents as f64 / ramp_took.as_secs_f64();
        // A connection ended for silence comes back while its old one is still marked connected,
        // and its Agent is re-keyed as a duplicate: none may be.
        let rekeyed: usize = instances
            .iter()
            .map(|instance| instance.logged(0, "duplicate instance_uid; rekeying").len())
            .sum();
        eprintln!(
            "{agents} Agents on {} connections across 3 instances, held after {ramp_took:?}, {:?} \
             after the last connection opened, {rate:.0} new Agents a second, {rekeyed} re-keyed \
             as duplicates; a database round trip {:?}; {}",
            fleet.hosts.len(),
            ramp_took.saturating_sub(opened),
            role.round_trip(),
            memory(&instances)
        );
        assert!(
            rate >= NEW_AGENTS_PER_SEC as f64,
            "at least {NEW_AGENTS_PER_SEC} new Agents a second: {rate:.0}"
        );
        assert_eq!(rekeyed, 0, "no connection was ended for silence");
        let answer = client
            .put(operator_url(&instances[0], "hosts/sim-gateway/gateway"))
            .json(&serde_json::json!({"gateway": true}))
            .send()
            .await
            .expect("mark the Gateway");
        assert!(
            answer.status().is_success(),
            "the Gateway is marked: {}",
            answer.status()
        );

        // What the killed instance carries: by the balancer's placement, and by the rows.
        let placed = balancer.placed();
        let on = |backend: usize| -> Vec<usize> {
            (0..fleet.hosts.len())
                .filter(|&host| placed.get(&fleet.hosts[host].address) == Some(&backend))
                .collect()
        };
        let killed_hosts = on(KILLED);
        assert!(
            gateway.iter().all(|host| killed_hosts.contains(host)),
            "the Gateway's pool is on the instance that is killed"
        );
        let killed_id = role.text(&format!(
            "SELECT holder::text FROM agents WHERE instance_uid = '{}'",
            fleet.hosts[killed_hosts[0]].uids[0]
        ));
        role.sql(&format!(
            "CREATE TABLE g20_before AS SELECT instance_uid, takes FROM agents \
             WHERE holder = '{killed_id}'"
        ));
        let killed_agents: usize = killed_hosts
            .iter()
            .map(|&host| fleet.hosts[host].uids.len())
            .sum();
        assert_eq!(
            role.number("SELECT count(*) FROM g20_before") as usize,
            killed_agents,
            "the rows name the killed instance as the holder of what it carries"
        );

        let killed_at = Instant::now();
        match loss {
            Loss::Killed => instances[KILLED].kill(),
            Loss::Frozen => instances[KILLED].freeze(),
        }
        eprintln!(
            "lost instance {KILLED} ({loss:?}), which carried {killed_agents} Agents on {} \
             connections",
            killed_hosts.len()
        );

        // A revocation through each instance left, of a host the other carries, and one of a
        // host of the killed instance once another carries it.
        tokio::time::sleep(Duration::from_secs(5)).await;
        let on_a = on(0)[0];
        let on_b = on(1)[0];
        let moved = {
            let mut moved = None;
            let found = eventually(Duration::from_secs(30), || {
                let now = balancer.placed();
                moved = killed_hosts.iter().copied().find(|&host| {
                    !gateway.contains(&host)
                        && now
                            .get(&fleet.hosts[host].address)
                            .is_some_and(|&to| to != KILLED)
                        && record
                            .host(host)
                            .connects
                            .last()
                            .is_some_and(|&at| at > killed_at)
                });
                moved.is_some()
            })
            .await;
            assert!(found, "a host of the killed instance is carried by another");
            moved.expect("a moved host")
        };
        let moved_to = balancer.placed()[&fleet.hosts[moved].address];
        let mut revocations = Vec::new();
        for (host, through) in [(on_a, 1), (on_b, 0), (moved, 1 - moved_to)] {
            let at = Instant::now();
            revoke(&client, &instances[through], &fleet.hosts[host]).await;
            revocations.push((host, at));
        }
        let mut revoked_within = Vec::new();
        for &(host, at) in &revocations {
            let ended = eventually(REVOKED_WITHIN + Duration::from_secs(5), || {
                record.host(host).losses.iter().any(|&(lost, _)| lost >= at)
            })
            .await;
            assert!(ended, "the session of revoked host {host} ended");
            let lost = record
                .host(host)
                .losses
                .iter()
                .find(|&&(lost, _)| lost >= at)
                .map(|&(lost, _)| lost - at)
                .expect("a loss");
            assert!(
                lost <= REVOKED_WITHIN,
                "the revocation of host {host} ended its session within {REVOKED_WITHIN:?}: \
                 {lost:?}"
            );
            revoked_within.push(lost);
        }
        let revoked: HashSet<usize> = revocations.iter().map(|&(host, _)| host).collect();
        let excluded = in_list(
            revoked
                .iter()
                .flat_map(|&host| fleet.hosts[host].uids.clone()),
        );

        // Every Agent of the killed instance is carried by another within the bound.
        let expected = killed_agents - fleet.hosts[moved].uids.len();
        let carried = |role: &Role| {
            role.number(&format!(
                "SELECT count(*) FROM agents a JOIN g20_before b USING (instance_uid) \
                 WHERE a.connected AND a.holder IS NOT NULL AND a.holder <> '{killed_id}' \
                 AND a.instance_uid NOT IN {excluded}"
            )) as usize
        };
        let all_carried = eventually(CARRIED_WITHIN.saturating_sub(killed_at.elapsed()), || {
            carried(&role) == expected
        })
        .await;
        let carried_after = killed_at.elapsed();
        assert!(
            all_carried,
            "every Agent of the killed instance is carried within {CARRIED_WITHIN:?}: {} of \
             {expected} after {carried_after:?}",
            carried(&role)
        );

        // None reconnected before the delay it drew.
        for &host in &killed_hosts {
            if revoked.contains(&host) {
                continue;
            }
            let log = record.host(host);
            let &(lost, drawn) = log
                .losses
                .iter()
                .find(|&&(lost, _)| lost >= killed_at)
                .unwrap_or_else(|| panic!("host {host} logged its lost connection"));
            let drawn = drawn.unwrap_or_else(|| panic!("host {host} logged no delay"));
            assert!(
                drawn < 30_000,
                "host {host} drew over [0, 30 s): {drawn} ms"
            );
            let back = log
                .connects
                .iter()
                .find(|&&at| at > lost)
                .unwrap_or_else(|| panic!("host {host} came back"));
            assert!(
                *back - lost >= Duration::from_millis(drawn),
                "host {host} came back after {:?}, before the {drawn} ms it drew",
                *back - lost
            );
        }

        // No notification is left unread once they are carried: each instance left hears and
        // handles the sentinels committed through the other in time, and the database's queue is
        // read.
        // Not a host the killed instance carried, whose holder writes are counted below.
        let mut skip: HashSet<usize> = revoked.iter().copied().collect();
        skip.extend(gateway.iter().copied());
        skip.extend(killed_hosts.iter().copied());
        let heard = heard_by_every(
            &client,
            &instances,
            &[0, 1],
            &balancer.placed(),
            &fleet,
            &record,
            &skip,
        )
        .await;
        let revoked: HashSet<usize> = revoked
            .into_iter()
            .chain(heard.iter().map(|&(host, _)| host))
            .collect();
        assert!(
            eventually(Duration::from_secs(10), || role.unread_notifications() == 0).await,
            "no notification is left unread: {} millionths of the queue",
            role.unread_notifications()
        );

        // One holder write per Agent taken over: still so once a presence interval has passed.
        tokio::time::sleep(HEARTBEAT + Duration::from_secs(5)).await;
        let rewritten = role.number(&format!(
            "SELECT count(*) FROM agents a JOIN g20_before b USING (instance_uid) \
             WHERE a.takes <> b.takes + 1 AND a.instance_uid NOT IN {excluded}"
        ));
        assert_eq!(rewritten, 0, "one holder write per Agent taken over");

        // The Gateway's pool came back, refused by nothing; and no host was throttled.
        for &host in &gateway {
            let log = record.host(host);
            assert!(
                log.connects.iter().any(|&at| at > killed_at),
                "the Gateway's connection {host} came back"
            );
            assert!(
                log.refusals.is_empty(),
                "the Gateway's connection {host}: {:?}",
                log.refusals
            );
        }
        let throttled: Vec<String> = (0..fleet.hosts.len())
            .filter(|host| !revoked.contains(host))
            .flat_map(|host| {
                record
                    .host(host)
                    .refusals
                    .iter()
                    .map(|(_, why)| format!("host {host}: {why}"))
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(
            throttled.is_empty(),
            "no host was refused or throttled: {} {:?}",
            throttled.len(),
            &throttled[..throttled.len().min(10)]
        );

        eprintln!(
            "G-20 failover: {agents} Agents, 1 per connection, a Gateway of {GATEWAY_CONNECTIONS} \
             connections, 3 instances, instance {KILLED} lost ({loss:?}) carrying {killed_agents}; carried \
             by the others after {carried_after:?} (bound {CARRIED_WITHIN:?}); revocations ended \
             their sessions after {revoked_within:?} (bound {REVOKED_WITHIN:?}); each instance \
             left ended its sentinels' sessions after {:?}; ramp {ramp_took:?}; {}",
            heard.iter().map(|&(_, took)| took).collect::<Vec<_>>(),
            memory(&instances)
        );
        fleet.stop().await;
    });
}

/// G-20's load: the presence batches and notifications of 100 000 simulated Agents at the default
/// heartbeat, ten on each connection, about 3 300 reports a second, across three instances, for ten
/// minutes, while one Agent in a hundred reconnects each minute, labels are set on 100 Agents every
/// five seconds, a Configuration is rolled out to 10 000 Agents and a certificate revoked once a
/// minute, and a page of the Agents is read every ten seconds: no presence batch is written later
/// than its interval after it was due, and no notification is left unread.
/// Verifies: ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
#[test]
fn g20_three_instances_carry_the_presence_and_notifications_of_100_000_agents() {
    if !stack_named() {
        return;
    }
    let size = |name: &str, default: u64| {
        variable(name).map_or(default, |value| {
            value.parse().unwrap_or_else(|_| panic!("{name}"))
        })
    };
    let agents = usize::try_from(size("FLEET_G20_LOAD_AGENTS", LOAD_AGENTS as u64)).expect("size");
    let per_host = usize::try_from(size(
        "FLEET_G20_AGENTS_PER_HOST",
        LOAD_AGENTS_PER_HOST as u64,
    ))
    .expect("k");
    let secs = size("FLEET_G20_LOAD_SECS", LOAD_SECS);
    // The fleet connects no faster than the instances hold new Agents, so that no connection
    // waits behind the others' first reports, unless `FLEET_G20_RAMP_PER_SEC` names the
    // connections a second.
    let paced = (NEW_AGENTS_PER_SEC / per_host as u64).clamp(1, RAMP_PER_SEC);
    let per_sec = size("FLEET_G20_RAMP_PER_SEC", paced).max(1);
    let the_target = agents == LOAD_AGENTS
        && secs == LOAD_SECS
        && per_host == LOAD_AGENTS_PER_HOST
        && variable("FLEET_G20_RAMP_PER_SEC").is_none();
    assert!(
        the_target || variable("CI").as_deref() != Some("true"),
        "a CI run measures G-20's target: no FLEET_G20_LOAD_AGENTS, FLEET_G20_AGENTS_PER_HOST, \
         FLEET_G20_LOAD_SECS or FLEET_G20_RAMP_PER_SEC"
    );
    if !the_target {
        eprintln!(
            "NOTE: {agents} Agents, {per_host} on each connection, {per_sec} connections a second, \
             for {secs} s is not G-20's target of {LOAD_AGENTS}, {LOAD_AGENTS_PER_HOST} on each, \
             {paced} a second, for {LOAD_SECS} s; this run does not decide G-20"
        );
    }
    opamp::tls::install_ring_provider();
    let role = Role::named();
    let bucket = Bucket::named();
    let pki = Pki::new();
    let hosts = agents.div_ceil(per_host);
    let made = Instant::now();
    let tls = pki.hosts(hosts, 1_000, |i| format!("sim-{i}"));
    eprintln!("made {hosts} certificates in {:?}", made.elapsed());

    let instances = [
        Instance::start(&pki, &role, &bucket),
        Instance::start(&pki, &role, &bucket),
        Instance::start(&pki, &role, &bucket),
    ];
    let balancer = Balancer::start(instances.iter().map(|instance| instance.agent).collect());
    let endpoint = format!("wss://127.0.0.1:{}/v1/opamp", balancer.address.port());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the fleet's runtime");
    let record = Record::new(hosts);
    record_into(&record);
    let client = operator_client(&pki);
    runtime.block_on(async {
        let mut fleet = Fleet::new(record.clone());
        let started = Instant::now();
        let tracing_db = variable("FLEET_G20_TRACE").is_some().then(|| {
            // Never dropped, so it drops no schema.
            let role = std::mem::ManuallyDrop::new(Role::admin_of(&role));
            std::thread::spawn(move || loop {
                eprintln!("--- database at {:?}\n{}", Instant::now(), role.activity());
                std::thread::sleep(Duration::from_secs(10));
            })
        });
        let _ = tracing_db;
        ramp(&mut fleet, &endpoint, &tls, per_host, 10, per_sec).await;
        let agents = fleet.agents();
        let ramp_bound = Duration::from_secs(hosts as u64 / per_sec + 300);
        assert!(
            eventually(ramp_bound, || fleet.connected_hosts() == hosts).await,
            "the fleet connected: {} of {hosts} hosts\n{}",
            fleet.connected_hosts(),
            diagnosis(&fleet, &instances)
        );
        let connected = |role: &Role| {
            role.number("SELECT count(*) FROM agents WHERE connected AND holder IS NOT NULL")
                as usize
        };
        // The fleet is held at the failover's least rate of new Agents, and two minutes to spare.
        let held_bound = (Duration::from_secs(agents as u64 / NEW_AGENTS_PER_SEC + 120))
            .saturating_sub(started.elapsed());
        assert!(
            eventually(held_bound, || connected(&role) == agents).await,
            "every Agent is held: {} of {agents} after {:?}",
            connected(&role),
            started.elapsed()
        );
        let ramp_took = started.elapsed();
        let rekeyed = |from: &[usize]| -> usize {
            instances
                .iter()
                .zip(from)
                .map(|(instance, &from)| {
                    instance
                        .logged(from, "duplicate instance_uid; rekeying")
                        .len()
                })
                .sum()
        };
        eprintln!(
            "{agents} Agents on {hosts} connections ({per_host} each) across 3 instances, held \
             after {ramp_took:?}, {:.0} new Agents a second, {} re-keyed as duplicates, which the \
             run does not count; {}",
            agents as f64 / ramp_took.as_secs_f64(),
            rekeyed(&[0, 0, 0]),
            memory(&instances)
        );

        // What the rollout releases: one Configuration in a Deployment aimed at a tenth of the
        // fleet.
        let put = |path: &str, body: serde_json::Value| {
            let request = client.put(operator_url(&instances[0], path)).json(&body);
            async move {
                let answer = request.send().await.expect("put");
                assert!(answer.status().is_success(), "{}", answer.status());
            }
        };
        put(
            "configurations/g20",
            serde_json::json!({"body": "receivers: {}\n", "service_name": SERVICE}),
        )
        .await;
        put(
            "deployments/g20",
            serde_json::json!({"selector": {"g20.group": "rollout"}}),
        )
        .await;
        put(
            "deployments/g20/configurations/g20",
            serde_json::json!(null),
        )
        .await;

        let logged_from: Vec<usize> = instances.iter().map(Instance::log_len).collect();
        let measured = Instant::now();
        let end = measured + Duration::from_secs(secs);
        let failures = Arc::new(Mutex::new(Vec::<String>::new()));
        let slowest = Arc::new(AtomicU64::new(0));
        let mut most_unread = 0;
        let mut revoked = HashSet::new();
        let mut draw = Draw::new();
        let mut ticks = 0u64;
        let mut acts = Vec::new();
        while Instant::now() < end {
            tokio::time::sleep(Duration::from_secs(1)).await;
            ticks += 1;
            let through = &instances[usize::try_from(ticks % 3).expect("instance")];
            // One Agent in a hundred reconnects each minute, a host of `per_host` at a time,
            // counted from the start so that no remainder is lost.
            let churned = |ticks: u64| ticks * agents as u64 / (6000 * per_host as u64);
            for _ in churned(ticks - 1)..churned(ticks) {
                fleet.hosts[draw.below(hosts)]
                    .churn
                    .store(true, Ordering::SeqCst);
            }
            let act = |request: reqwest::RequestBuilder, what: String| {
                let (failures, slowest) = (failures.clone(), slowest.clone());
                tokio::spawn(async move {
                    let at = Instant::now();
                    match request.send().await {
                        Ok(answer) if answer.status().is_success() => {}
                        Ok(answer) => failures
                            .lock()
                            .expect("failures")
                            .push(format!("{what}: {}", answer.status())),
                        Err(e) => failures
                            .lock()
                            .expect("failures")
                            .push(format!("{what}: {e}")),
                    }
                    slowest.fetch_max(
                        u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX),
                        Ordering::SeqCst,
                    );
                })
            };
            if ticks.is_multiple_of(5) {
                for _ in 0..100 {
                    let host = &fleet.hosts[draw.below(hosts)];
                    let uid = host.uids[draw.below(host.uids.len())];
                    acts.push(act(
                        client
                            .put(operator_url(through, &format!("agents/{uid}/labels")))
                            .json(&serde_json::json!({"labels": {"g20.label": ticks.to_string()}})),
                        format!("labels on {uid}"),
                    ));
                }
            }
            if ticks % 60 == 30 {
                acts.push(act(
                    client
                        .put(operator_url(through, "configurations/g20"))
                        .json(&serde_json::json!({
                            "body": format!("receivers: {{}}\n# {ticks}\n"),
                            "service_name": SERVICE
                        })),
                    "a Configuration saved".to_string(),
                ));
            }
            if ticks % 60 == 35 {
                acts.push(act(
                    client.post(operator_url(through, "deployments/g20/rollout")),
                    "a rollout".to_string(),
                ));
            }
            if ticks % 60 == 45 {
                let host = draw.below(hosts);
                revoked.insert(host);
                acts.push(act(
                    client
                        .post(operator_url(through, "revocations"))
                        .json(&serde_json::json!({
                            "certificate": {
                                "authority": "client",
                                "serial": format!("{:x}", fleet.hosts[host].serial)
                            }
                        })),
                    format!("a revocation of host {host}"),
                ));
            }
            if ticks.is_multiple_of(10) {
                acts.push(act(
                    client.get(operator_url(through, "agents?limit=100")),
                    "a page of the Agents".to_string(),
                ));
            }
            if ticks.is_multiple_of(5) {
                most_unread = most_unread.max(role.unread_notifications());
            }
        }
        for act in acts {
            let _ = tokio::time::timeout(Duration::from_secs(60), act).await;
        }
        let measured_for = measured.elapsed();

        // Nothing is left unread once the acts end: each instance hears and handles the sentinels
        // committed through another in time, and the database's queue is read.
        let heard = heard_by_every(
            &client,
            &instances,
            &[0, 1, 2],
            &balancer.placed(),
            &fleet,
            &record,
            &revoked,
        )
        .await;
        revoked.extend(heard.iter().map(|&(host, _)| host));
        assert!(
            eventually(Duration::from_secs(15), || role.unread_notifications() == 0).await,
            "no notification is left unread: {} millionths of the queue",
            role.unread_notifications()
        );
        // No presence batch hangs unseen: every Agent a connection carries was written seen within
        // a heartbeat and two presence intervals.
        let unseen = role.number(&format!(
            "SELECT count(*) FROM agents WHERE connected AND holder IS NOT NULL \
             AND last_seen_ms < (extract(epoch FROM clock_timestamp()) * 1000)::bigint - {}",
            (HEARTBEAT + 2 * PRESENCE_INTERVAL).as_millis()
        ));
        let late: Vec<String> = instances
            .iter()
            .zip(&logged_from)
            .flat_map(|(instance, &from)| {
                instance.logged(from, "later than its interval after it was due")
            })
            .collect();
        let failed_batches: Vec<String> = instances
            .iter()
            .zip(&logged_from)
            .flat_map(|(instance, &from)| instance.logged(from, "could not write the presence"))
            .collect();
        // Every Agent not revoked is held, by one row each, once the hosts that reconnected are
        // back: no identity is re-keyed and no row stays held without its connection.
        let expected = agents - revoked.len() * per_host;
        let all_held = eventually(Duration::from_secs(60), || connected(&role) == expected).await;
        let still_held = connected(&role);
        let rekeyed_measured = rekeyed(&logged_from);
        let refused: Vec<String> = (0..hosts)
            .filter(|host| !revoked.contains(host))
            .flat_map(|host| {
                record
                    .host(host)
                    .refusals
                    .iter()
                    .filter(|&&(at, _)| at >= measured)
                    .map(|(_, why)| format!("host {host}: {why}"))
                    .collect::<Vec<_>>()
            })
            .collect();
        eprintln!(
            "G-20 load: {agents} Agents, {per_host} per connection, 3 instances, measured for \
             {measured_for:?} after a ramp of {ramp_took:?}; late presence batches {}, failed \
             batches {}, Agents held unseen for {:?} {unseen}; the most of the notification queue \
             unread at a sample {most_unread} millionths, the sentinels' sessions ended \
             after {:?}; slowest operator act {} ms; {} acts failed; {} hosts refused or \
             throttled; {still_held} of {expected} Agents held at the end, {} hosts revoked, \
             {rekeyed_measured} Agents re-keyed as duplicates; {}",
            late.len(),
            failed_batches.len(),
            HEARTBEAT + 2 * PRESENCE_INTERVAL,
            heard.iter().map(|&(_, took)| took).collect::<Vec<_>>(),
            slowest.load(Ordering::SeqCst),
            failures.lock().expect("failures").len(),
            refused.len(),
            revoked.len(),
            memory(&instances)
        );
        assert!(
            late.is_empty(),
            "no presence batch is written late: {late:#?}"
        );
        assert!(
            failed_batches.is_empty(),
            "no presence batch fails: {failed_batches:#?}"
        );
        assert_eq!(
            unseen, 0,
            "no connected Agent's presence is older than its bound"
        );
        assert_eq!(rekeyed_measured, 0, "no Agent was re-keyed as a duplicate");
        assert!(
            all_held,
            "every Agent not revoked is held by one row: {still_held} of {expected}"
        );
        assert!(
            refused.is_empty(),
            "no host was refused or throttled: {:?}",
            &refused[..refused.len().min(10)]
        );
        let failures = failures.lock().expect("failures").clone();
        assert!(
            failures.is_empty(),
            "every operator act is answered: {failures:#?}"
        );
        fleet.stop().await;
    });
}
