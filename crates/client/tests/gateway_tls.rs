//! The downstream hop's TLS (ADR-0037, ADR-0035): a Gateway configured with `[gateway.tls]` serves
//! the downstream endpoint over TLS, and when a `client_ca_file` is set it *requires* a downstream
//! Agent to present a certificate that chains to it.
//!
//! This is the boundary a `[gateway.tls]` section exists for. Before it was wired in, the section
//! was parsed and then ignored: the endpoint stayed plaintext and the client CA gated nobody, so
//! the `Authorization` credential rode the hop in the clear and any peer could connect. These tests
//! pin the fix — TLS is actually served, and the CA is actually enforced.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use client::config::ClientConfig;
use client::service::runtime::shutdown_channel;
use opamp::proto::{AgentCapabilities, AgentToServer, ServerToAgent};
use opamp::uid::InstanceUid;
use prost::Message as _;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};
use server::fleet::AppState;

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
    let app = server::app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (addr, state, dir)
}

/// A Gateway whose downstream endpoint serves TLS. `require_client_ca` writes the CA as
/// `client_ca_file`, turning the hop into mutual TLS.
async fn spawn_tls_gateway(
    server: SocketAddr,
    pki: &Pki,
    require_client_ca: bool,
) -> (
    SocketAddr,
    tokio::sync::watch::Sender<bool>,
    tempfile::TempDir,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let (cert, key) = pki.issue("127.0.0.1");
    let cert_file = dir.path().join("gateway-cert.pem");
    let key_file = dir.path().join("gateway-key.pem");
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&cert_file, &cert).expect("write cert");
    std::fs::write(&key_file, &key).expect("write key");
    std::fs::write(&ca_file, &pki.ca_pem).expect("write ca");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let listen = listener.local_addr().expect("addr");
    drop(listener); // reserve the port number; the Gateway binds it itself

    let client_ca_line = if require_client_ca {
        format!("client_ca_file = {:?}", ca_file.display().to_string())
    } else {
        String::new()
    };
    let toml = format!(
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
    );
    let config: ClientConfig = toml::from_str(&toml).expect("gateway config");
    let (tx, shutdown) = shutdown_channel();
    tokio::spawn(async move {
        client::gateway::run(Arc::new(config), shutdown)
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
    client::tls::install_ring_provider();
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
#[tokio::test]
async fn a_downstream_agent_with_a_certificate_reaches_the_server_over_tls() {
    let (server, state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let (gateway, _stop, _dir) = spawn_tls_gateway(server, &pki, true).await;

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

/// The fix's core: with a client CA configured, a peer presenting *no* certificate is turned away at
/// the handshake. Before the section was wired in, this peer connected freely.
#[tokio::test]
async fn a_downstream_peer_without_a_certificate_is_refused() {
    let (server, _state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let (gateway, _stop, _dir) = spawn_tls_gateway(server, &pki, true).await;

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
#[tokio::test]
async fn the_tls_endpoint_does_not_answer_plaintext() {
    let (server, _state, _server_dir) = spawn_server().await;
    let pki = Pki::new();
    let (gateway, _stop, _dir) = spawn_tls_gateway(server, &pki, true).await;

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
