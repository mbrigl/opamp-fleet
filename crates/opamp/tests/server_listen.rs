//! The listener an OpAMP endpoint is served on (ADR-0036): the bound on connection setup, the
//! client-certificate rules, and what the handshake carries into a request.

use std::net::SocketAddr;
use std::time::Duration;

use axum::extract::ConnectInfo;
use axum::routing::get;
use axum::{Extension, Router};
use opamp::server::listen::{ClientAuth, Handle, Listener, PeerCertificate, ServerTls};
use opamp::tls::Identity;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A throwaway CA that issues the server's and the clients' certificates.
struct Pki {
    ca_pem: String,
    ca_key_pem: String,
}

impl Pki {
    fn new() -> Self {
        let key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(vec!["opamp-test-ca".to_string()]).expect("params");
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).expect("ca");
        Pki {
            ca_pem: cert.pem(),
            ca_key_pem: key.serialize_pem(),
        }
    }

    fn issue(&self, name: &str) -> Identity {
        let ca_key = KeyPair::from_pem(&self.ca_key_pem).expect("ca key");
        let issuer = Issuer::from_ca_cert_pem(&self.ca_pem, ca_key).expect("issuer");
        let key = KeyPair::generate().expect("key");
        let params = CertificateParams::new(vec![name.to_string()]).expect("params");
        let cert = params.signed_by(&key, &issuer).expect("signed");
        Identity {
            cert_pem: cert.pem().into_bytes(),
            key_pem: key.serialize_pem().into_bytes(),
        }
    }
}

/// Answers with what the listener put into the request.
fn router() -> Router {
    Router::new().route(
        "/",
        get(
            |Extension(peer): Extension<PeerCertificate>,
             ConnectInfo(addr): ConnectInfo<SocketAddr>| async move {
                format!("certificate={} peer={}", peer.present(), addr.ip())
            },
        ),
    )
}

fn spawn(listener: Listener) {
    tokio::spawn(async move { listener.serve(router()).await.expect("serve") });
}

fn bind() -> (std::net::TcpListener, u16) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    (listener, port)
}

/// A TLS listener on an ephemeral port, with `client_auth`, and the port it is on.
fn spawn_tls(pki: &Pki, client_auth: ClientAuth) -> u16 {
    opamp::tls::install_ring_provider();
    let tls = ServerTls {
        identity: pki.issue("localhost"),
        client_auth,
    }
    .rustls_config()
    .expect("server config");
    let (listener, port) = bind();
    spawn(Listener::new(listener, Handle::new()).with_tls(tls));
    port
}

fn client(pki: &Pki, identity: Option<&Identity>) -> reqwest::Client {
    opamp::tls::install_ring_provider();
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .tls_certs_only([reqwest::Certificate::from_pem(pki.ca_pem.as_bytes()).expect("ca")]);
    if let Some(identity) = identity {
        let mut pem = identity.key_pem.clone();
        pem.extend_from_slice(&identity.cert_pem);
        builder = builder.identity(reqwest::Identity::from_pem(&pem).expect("identity"));
    }
    builder.build().expect("client")
}

async fn get_text(client: &reqwest::Client, port: u16) -> reqwest::Result<String> {
    client
        .get(format!("https://localhost:{port}/"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
}

/// A peer that sends a request line and then falls silent is hung up on, rather than holding the
/// connection for as long as it likes.
/// Verifies: ADR-0036, ADR-0038
#[tokio::test]
async fn a_connection_that_never_finishes_its_headers_is_hung_up_on() {
    let bound = Duration::from_secs(1);
    let (listener, port) = bind();
    spawn(Listener::new(listener, Handle::new()).with_header_read_timeout(bound));

    let mut socket = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    socket
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
        .await
        .expect("write a partial request");
    let mut buffer = Vec::new();
    let closed = tokio::time::timeout(bound * 8, socket.read_to_end(&mut buffer)).await;
    assert!(closed.is_ok(), "the listener left a silent connection open");
}

/// A plain listener puts the peer's address into the request, and no certificate.
#[tokio::test]
async fn a_plain_listener_carries_the_peer_and_no_certificate() {
    opamp::tls::install_ring_provider();
    let (listener, port) = bind();
    spawn(Listener::new(listener, Handle::new()));
    let text = reqwest::get(format!("http://127.0.0.1:{port}/"))
        .await
        .expect("get")
        .text()
        .await
        .expect("text");
    assert_eq!(text, "certificate=false peer=127.0.0.1");
}

/// With an optional client certificate the handshake succeeds either way, and the request says
/// whether a verified certificate came with it — so the application can require one per route.
/// Verifies: ADR-0036
#[tokio::test]
async fn an_optional_client_certificate_is_carried_into_the_request() {
    let pki = Pki::new();
    let port = spawn_tls(
        &pki,
        ClientAuth::Optional {
            ca_pem: pki.ca_pem.clone().into_bytes(),
        },
    );
    let with = get_text(&client(&pki, Some(&pki.issue("agent"))), port)
        .await
        .expect("with a certificate");
    assert_eq!(with, "certificate=true peer=127.0.0.1");
    let without = get_text(&client(&pki, None), port)
        .await
        .expect("without a certificate");
    assert_eq!(without, "certificate=false peer=127.0.0.1");
}

/// With a required client certificate, a peer without one never gets past the handshake, and one
/// from another CA neither.
/// Verifies: ADR-0036
#[tokio::test]
async fn a_required_client_certificate_refuses_the_handshake_without_one() {
    let pki = Pki::new();
    let port = spawn_tls(
        &pki,
        ClientAuth::Required {
            ca_pem: pki.ca_pem.clone().into_bytes(),
        },
    );
    assert!(get_text(&client(&pki, None), port).await.is_err());
    let stranger = Pki::new().issue("agent");
    assert!(get_text(&client(&pki, Some(&stranger)), port)
        .await
        .is_err());
    let with = get_text(&client(&pki, Some(&pki.issue("agent"))), port)
        .await
        .expect("with a certificate");
    assert_eq!(with, "certificate=true peer=127.0.0.1");
}

/// Material that cannot be used is refused before anything listens, naming the part.
#[test]
fn unusable_material_names_the_part() {
    let pki = Pki::new();
    let mut identity = pki.issue("localhost");
    identity.key_pem = b"no key here".to_vec();
    let error = ServerTls {
        identity,
        client_auth: ClientAuth::None,
    }
    .rustls_config()
    .expect_err("no key");
    assert!(error.starts_with("the TLS key:"), "{error}");
}

/// A client that offers TLS 1.2 alone never completes the handshake: the listener speaks TLS 1.3
/// and nothing older. The client is built from the full ring provider, which still has its TLS 1.2
/// suites, so the refusal is the listener's.
/// Verifies: ADR-0036, ADR-0038, ADR-0040, Q-3
#[tokio::test]
async fn a_client_offering_only_tls_1_2_is_refused() {
    let pki = Pki::new();
    let port = spawn_tls(&pki, ClientAuth::None);
    let mut roots = rustls::RootCertStore::empty();
    for cert in opamp::tls::certificates(pki.ca_pem.as_bytes()).expect("ca") {
        roots.add(cert).expect("anchor");
    }
    let old = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS12])
    .expect("tls 1.2")
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connected = tokio_tungstenite::connect_async_tls_with_config(
        format!("wss://localhost:{port}/"),
        None,
        false,
        Some(tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(
            old,
        ))),
    )
    .await;
    let error = connected
        .expect_err("a TLS 1.2 client was accepted")
        .to_string();
    assert!(
        error.contains("version") || error.contains("Version") || error.contains("alert"),
        "{error}"
    );
}

/// A listener at its cap closes the next connection on accept, keeps the ones it holds, and takes
/// a new one once a held one has gone.
/// Verifies: ADR-0038
#[tokio::test]
async fn connections_past_the_cap_are_refused_while_established_ones_keep_working() {
    opamp::tls::install_ring_provider();
    let (listener, port) = bind();
    spawn(Listener::new(listener, Handle::new()).with_max_connections(2));

    let first = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("first");
    let second = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("second");
    // Give the listener time to accept both before the third arrives.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut third = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("third");
    let mut buffer = Vec::new();
    let closed = tokio::time::timeout(Duration::from_secs(5), third.read_to_end(&mut buffer)).await;
    assert!(closed.is_ok(), "a connection past the cap was held open");

    // A held connection still answers.
    let mut held = first;
    held.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .expect("request");
    let mut answer = Vec::new();
    held.read_to_end(&mut answer).await.expect("answer");
    assert!(
        answer.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&answer)
    );
    drop(second);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Both slots are free again, so a new peer is served.
    let text = reqwest::get(format!("http://127.0.0.1:{port}/"))
        .await
        .expect("a freed slot serves")
        .text()
        .await
        .expect("text");
    assert_eq!(text, "certificate=false peer=127.0.0.1");
}

/// A listener without TLS on an address other than the loopback literals is refused before it
/// accepts anything: plaintext is for the loopback alone.
/// Verifies: ADR-0038, Q-1
#[tokio::test]
async fn a_plaintext_listener_off_the_loopback_is_refused() {
    let listener = std::net::TcpListener::bind("0.0.0.0:0").expect("bind");
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        Listener::new(listener, Handle::new()).serve(router()),
    )
    .await
    .expect("serve returns rather than listening")
    .expect_err("a plaintext listener off the loopback was served");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{error}"
    );
    assert!(error.to_string().contains("without TLS"), "{error}");
}
