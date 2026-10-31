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
    agent: SocketAddr,
    operator: SocketAddr,
    revocations: Arc<Revocations>,
    held: fleet_server::instance::Held,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    _dir: tempfile::TempDir,
}

impl Instance {
    /// An instance on `config`, in the test's schema, sharing the database.
    async fn start(pki: &Pki, schema: &Schema, config: PostgresConfig) -> Self {
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
        let state = Arc::new(
            fleet_server::fleet::AppState::on(&records)
                .expect("state")
                .with_revocations(Some(revocations.clone())),
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
        let operators = fleet_server::operator_app(state, None);

        let lease = {
            let database = database.clone();
            fleet_server::instance::Lease::take(database, TIMES, "test-host").expect("lease")
        };
        fleet_server::instance::follow_revocations(&database, revocations.clone());
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
            agent,
            operator,
            revocations,
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
