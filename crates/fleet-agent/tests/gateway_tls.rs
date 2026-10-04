//! The downstream hop's TLS (ADR-0009, ADR-0017, ADR-0040): a Gateway serves the downstream
//! endpoint over mutual TLS 1.3 only, and *requires* a downstream Agent to present a certificate
//! that chains to `client_ca_file`. Without the full `[gateway.tls]` section it does not start.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fleet_agent::config::ClientConfig;
use fleet_agent::shutdown::shutdown_channel;
use fleet_server::fleet::AppState;
use opamp::proto::{AgentCapabilities, AgentToServer, ServerToAgent};
use opamp::uid::InstanceUid;
use prost::Message as _;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};

/// A throwaway PKI: one CA that both signs the Gateway's server certificate and mints the client
/// certificates a downstream Agent presents.
struct Pki {
    ca_pem: String,
    ca_key_pem: String,
}

impl Pki {
    fn new() -> Self {
        let key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(vec!["opamp-fleet-gateway-test-ca".to_string()])
            .expect("params");
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).expect("ca");
        Pki {
            ca_pem: cert.pem(),
            ca_key_pem: key.serialize_pem(),
        }
    }

    fn issuer(&self) -> Issuer<'static, KeyPair> {
        let key = KeyPair::from_pem(&self.ca_key_pem).expect("ca key");
        Issuer::from_ca_cert_pem(&self.ca_pem, key).expect("issuer")
    }

    /// A certificate and key signed by this CA for `name` — an IP like `127.0.0.1` becomes an IP
    /// SAN, which is what lets a client verify the Gateway it dialled by address.
    fn issue(&self, name: &str) -> (String, String) {
        let key = KeyPair::generate().expect("key");
        let params = CertificateParams::new(vec![name.to_string()]).expect("params");
        let cert = params.signed_by(&key, &self.issuer()).expect("signed");
        (cert.pem(), key.serialize_pem())
    }
}

/// The real Server on an ephemeral plaintext port — the upstream a Gateway folds onto.
async fn spawn_server() -> (SocketAddr, Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(AppState::new(dir.path().join("fleet-configs")).expect("state"));
    let app = fleet_server::agent_app(state.clone(), fleet_server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (addr, state, dir)
}

/// The Gateway's TOML, `[gateway.tls]` naming the files in `dir`; `client_ca` adds
/// `client_ca_file`.
fn gateway_toml(
    server: SocketAddr,
    listen: SocketAddr,
    pki: &Pki,
    dir: &std::path::Path,
    client_ca: bool,
) -> String {
    let (cert, key) = pki.issue("127.0.0.1");
    let cert_file = dir.join("gateway-cert.pem");
    let key_file = dir.join("gateway-key.pem");
    let ca_file = dir.join("ca.pem");
    std::fs::write(&cert_file, &cert).expect("write cert");
    std::fs::write(&key_file, &key).expect("write key");
    std::fs::write(&ca_file, &pki.ca_pem).expect("write ca");
    let client_ca_line = if client_ca {
        format!("client_ca_file = {:?}", ca_file.display().to_string())
    } else {
        String::new()
    };
    format!(
        r#"
        endpoint = "ws://{server}/v1/opamp"
        [gateway]
        listen = "{listen}"
        upstream_connections = 4
        [gateway.tls]
        cert_file = {:?}
        key_file = {:?}
        {client_ca_line}
        "#,
        cert_file.display().to_string(),
        key_file.display().to_string(),
    )
}

/// A Gateway whose downstream endpoint serves mutual TLS with `pki`'s CA as `client_ca_file`.
async fn spawn_tls_gateway(
    server: SocketAddr,
    pki: &Pki,
) -> (
    SocketAddr,
    tokio::sync::watch::Sender<bool>,
    tempfile::TempDir,
) {
    spawn_bounded_gateway(server, pki, opamp::server::listen::HEADER_READ_TIMEOUT).await
}

/// [`spawn_tls_gateway`], with the header bound tightened to `header_read_timeout`.
async fn spawn_bounded_gateway(
    server: SocketAddr,
    pki: &Pki,
    header_read_timeout: Duration,
) -> (
    SocketAddr,
    tokio::sync::watch::Sender<bool>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    // Bound here and handed over, so no parallel test can take the port before the Gateway uses it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let listen = listener.local_addr().expect("addr");
    let toml = gateway_toml(server, listen, pki, dir.path(), true);
    let config: ClientConfig = toml::from_str(&toml).expect("gateway config");
    let (tx, shutdown) = shutdown_channel();
    tokio::spawn(async move {
        fleet_agent::gateway::run_on_bounded(
            Arc::new(config),
            listener,
            shutdown,
            header_read_timeout,
        )
        .await
        .expect("gateway");
    });
    // Wait for the listener to accept before anyone dials it.
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(listen).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (listen, tx, dir)
}

fn report(uid: &InstanceUid) -> AgentToServer {
    AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    }
}

/// A reqwest client that trusts `pki`'s CA and, given an identity, presents it as a client
/// certificate.
fn client(pki: &Pki, identity: Option<(String, String)>) -> reqwest::Client {
    opamp::tls::install_ring_provider();
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .tls_certs_only([reqwest::Certificate::from_pem(pki.ca_pem.as_bytes()).expect("ca")]);
    if let Some((cert, key)) = identity {
        let mut pem = key.into_bytes();
        pem.extend_from_slice(cert.as_bytes());
        builder = builder.identity(reqwest::Identity::from_pem(&pem).expect("identity"));
    }
    builder.build().expect("client")
}

/// With a client CA configured, a downstream Agent that presents a certificate reaches the Server
/// through the Gateway over TLS — and its reply comes back addressed to it. This is the hop working
/// end to end, encrypted, with the CA accepting a valid peer.
/// Verifies: ADR-0040, ADR-0039
#[tokio::test]
async fn a_downstream_agent_with_a_certificate_reaches_the_server_over_tls() {
    let (server, state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let (gateway, _stop, _dir) = spawn_tls_gateway(server, &pki).await;

    let (cert, key) = pki.issue("edge-01");
    let uid = InstanceUid::default();
    let response = client(&pki, Some((cert, key)))
        .post(format!("https://{gateway}/v1/opamp"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .body(report(&uid).encode_to_vec())
        .send()
        .await
        .expect("the TLS request reaches the gateway");
    assert!(response.status().is_success(), "{:?}", response.status());
    let reply =
        ServerToAgent::decode(response.bytes().await.expect("body")).expect("decode the reply");
    assert_eq!(
        InstanceUid::from_wire(&reply.instance_uid),
        Some(uid),
        "the reply came back addressed to the Agent that asked"
    );
    assert_eq!(
        state.snapshot().len(),
        1,
        "the Server saw the Agent behind the Gateway"
    );
}

/// A peer presenting *no* certificate is turned away at the handshake.
///
/// Verifies: ADR-0040, ADR-0039
#[tokio::test]
async fn a_downstream_peer_without_a_certificate_is_refused() {
    let (server, _state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let (gateway, _stop, _dir) = spawn_tls_gateway(server, &pki).await;

    let result = client(&pki, None)
        .post(format!("https://{gateway}/v1/opamp"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await;
    assert!(
        result.is_err(),
        "a peer with no client certificate must not be admitted, got {result:?}"
    );
}

/// A Gateway serving TLS does not also answer plaintext on the same port: a cleartext HTTP request
/// fails rather than exposing the hop the section was configured to protect.
/// Verifies: ADR-0040
#[tokio::test]
async fn the_tls_endpoint_does_not_answer_plaintext() {
    opamp::tls::install_ring_provider();
    let (server, _state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let (gateway, _stop, _dir) = spawn_tls_gateway(server, &pki).await;

    let result = reqwest::Client::new()
        .post(format!("http://{gateway}/v1/opamp"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await;
    assert!(
        result.is_err(),
        "a plaintext request to a TLS endpoint must fail, got {result:?}"
    );
}

/// A peer presenting a certificate from *another* CA is turned away at the handshake: only the
/// configured `client_ca_file` admits.
///
/// Verifies: ADR-0040
#[tokio::test]
async fn a_downstream_peer_with_a_certificate_from_another_ca_is_refused() {
    let (server, state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let (gateway, _stop, _dir) = spawn_tls_gateway(server, &pki).await;

    let stranger = Pki::new().issue("edge-01");
    let result = client(&pki, Some(stranger))
        .post(format!("https://{gateway}/v1/opamp"))
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await;
    assert!(
        result.is_err(),
        "a peer with a certificate from another CA must not be admitted, got {result:?}"
    );
    assert!(state.snapshot().is_empty(), "nothing reached the Server");
}

/// A Gateway whose `[gateway.tls]` lacks `client_ca_file` does not start: the load refuses the
/// file, and `run_on` refuses the configuration rather than serving without client certificates.
///
/// Verifies: ADR-0040
#[tokio::test]
async fn a_gateway_without_a_client_ca_does_not_start() {
    let (server, _state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let listen = listener.local_addr().expect("addr");
    let toml = gateway_toml(server, listen, &pki, dir.path(), false);

    let path = dir.path().join("supervisor.toml");
    std::fs::write(&path, &toml).expect("write config");
    let err =
        ClientConfig::load(&path).expect_err("the load refuses a Gateway without a client CA");
    assert!(err.contains("client_ca_file"), "{err}");

    let config: ClientConfig = toml::from_str(&toml).expect("gateway config");
    let (_stop, shutdown) = shutdown_channel();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        fleet_agent::gateway::run_on(Arc::new(config), listener, shutdown),
    )
    .await
    .expect("run_on returns rather than serving");
    let err = result.expect_err("a Gateway without a client CA must not serve");
    assert!(err.contains("client_ca_file"), "{err}");
}

/// A Gateway without `[gateway.tls]` does not start, on the loopback too: `run_on` refuses the
/// configuration rather than serving the downstream endpoint in plaintext.
///
/// Verifies: ADR-0040
#[tokio::test]
async fn a_gateway_without_tls_on_loopback_does_not_start() {
    let (server, _state, _server_dir) = spawn_server().await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let listen = listener.local_addr().expect("addr");
    let toml = format!(
        "endpoint = \"ws://{server}/v1/opamp\"\n[gateway]\nlisten = \"{listen}\"\n\
         upstream_connections = 4\n"
    );
    let config: ClientConfig = toml::from_str(&toml).expect("gateway config");
    let (_stop, shutdown) = shutdown_channel();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        fleet_agent::gateway::run_on(Arc::new(config), listener, shutdown),
    )
    .await
    .expect("run_on returns rather than serving");
    let err = result.expect_err("a Gateway without TLS must not serve");
    assert!(err.contains("[gateway.tls] is required"), "{err}");
}

/// The connection-setup bound on the Gateway's own surface (H18): a downstream peer that completes
/// the mutual-TLS handshake and then never finishes its request headers is hung up on, as the
/// Server's Agent plane does (`connection_setup.rs`). The handshake bound is 10 seconds; closing well
/// inside it shows the header bound did the work.
/// Verifies: ADR-0036, ADR-0040
#[tokio::test]
async fn a_downstream_connection_that_never_finishes_its_headers_is_hung_up_on() {
    use std::io::{Read as _, Write as _};
    let (server, _state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let (listen, _shutdown, _dir) =
        spawn_bounded_gateway(server, &pki, Duration::from_secs(1)).await;
    let (cert, key) = pki.issue("edge-01");
    let ca = pki.ca_pem.clone();
    let (read, elapsed) = tokio::task::spawn_blocking(move || {
        opamp::tls::install_ring_provider();
        let config = opamp::tls::client_builder()
            .with_root_certificates(opamp::tls::root_store(ca.as_bytes()).expect("roots"))
            .with_client_auth_cert(
                opamp::tls::certificates(cert.as_bytes()).expect("cert"),
                opamp::tls::private_key(key.as_bytes()).expect("key"),
            )
            .expect("client auth");
        let name = rustls::pki_types::ServerName::try_from("127.0.0.1").expect("name");
        let connection = rustls::ClientConnection::new(Arc::new(config), name).expect("tls");
        let tcp = std::net::TcpStream::connect(listen).expect("connect");
        tcp.set_read_timeout(Some(Duration::from_secs(8)))
            .expect("read timeout");
        let mut tls = rustls::StreamOwned::new(connection, tcp);
        // The handshake runs on the first write; a request line and one header, never ended.
        tls.write_all(b"GET /v1/opamp HTTP/1.1\r\nHost: localhost\r\n")
            .expect("handshake and a partial request");
        tls.flush().expect("flush");
        let started = std::time::Instant::now();
        let mut buffer = Vec::new();
        (tls.read_to_end(&mut buffer), started.elapsed())
    })
    .await
    .expect("join");
    let timed_out = matches!(&read, Err(e) if matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    assert!(
        !timed_out,
        "the Gateway left a connection open that never finished its headers"
    );
    assert!(elapsed < Duration::from_secs(8), "{elapsed:?}");
}
