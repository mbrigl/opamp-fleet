//! The listener an OpAMP endpoint is served on (ADR-0024): the bound on connection setup, the
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
/// Verifies: ADR-0024, ADR-0023
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
/// Verifies: ADR-0024
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
/// Verifies: ADR-0024
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
