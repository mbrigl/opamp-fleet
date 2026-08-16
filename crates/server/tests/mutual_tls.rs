//! Mutual TLS and the CSR flow, end to end over the real listener (ADR-0035).
//!
//! What these cover is the part that cannot be unit-tested: the handshake actually carrying a
//! client certificate into the OpAMP route, and the admission rule that every configured proof
//! must succeed. The signing itself is covered where it lives, in `server::ca`.

use std::sync::Arc;

use opamp::proto::{
    AgentCapabilities, AgentToServer, CertificateRequest, ConnectionSettingsRequest,
    OpAmpConnectionSettingsRequest, ServerErrorResponseType, ServerToAgent,
};
use opamp::uid::InstanceUid;
use prost::Message;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};
use server::ca::ClientCa;
use server::fleet::AppState;
use server::transport::{Admission, OpampAuth};

/// A throwaway PKI: one CA, a server certificate for `localhost`, and the ability to mint a client
/// certificate from it — the shape an operator's `[client_ca]` has.
struct Pki {
    ca_pem: String,
    ca_key_pem: String,
}

impl Pki {
    fn new() -> Self {
        let key = KeyPair::generate().expect("ca key");
        let mut params =
            CertificateParams::new(vec!["opamp-fleet-test-ca".to_string()]).expect("ca params");
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

    /// A certificate and key signed by this CA, for `name`.
    fn issue(&self, name: &str) -> (String, String) {
        let key = KeyPair::generate().expect("key");
        let params = CertificateParams::new(vec![name.to_string()]).expect("params");
        let cert = params.signed_by(&key, &self.issuer()).expect("signed");
        (cert.pem(), key.serialize_pem())
    }
}

fn report(uid: &InstanceUid) -> AgentToServer {
    AgentToServer {
        instance_uid: uid.as_bytes().to_vec(),
        sequence_num: 1,
        capabilities: AgentCapabilities::ReportsStatus as u64,
        ..Default::default()
    }
}

fn csr_for(name: &str) -> (Vec<u8>, KeyPair) {
    let key = KeyPair::generate().expect("client key");
    let params = CertificateParams::new(vec![name.to_string()]).expect("params");
    let csr = params
        .serialize_request(&key)
        .expect("csr")
        .pem()
        .expect("csr pem");
    (csr.into_bytes(), key)
}

/// Serves both planes over TLS on ephemeral ports (ADR-0066), the Agent plane through the acceptor
/// that carries the peer certificate into the request — the thing under test. Answers with the
/// OpAMP endpoint, the Operator plane's port, and the CA a client must trust.
async fn serve(
    pki: &Pki,
    admission: Admission,
    client_ca: Option<ClientCa>,
) -> (String, u16, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("state")
            .with_client_ca(client_ca),
    );
    let (server_cert, server_key) = pki.issue("localhost");

    let cert_file = dir.path().join("server-cert.pem");
    let key_file = dir.path().join("server-key.pem");
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&cert_file, &server_cert).expect("write cert");
    std::fs::write(&key_file, &server_key).expect("write key");
    std::fs::write(&ca_file, &pki.ca_pem).expect("write ca");

    let tls = toml::from_str::<server::config::TlsConfig>(&format!(
        "cert_file = {:?}\nkey_file = {:?}\nclient_ca_file = {:?}\n",
        cert_file.display().to_string(),
        key_file.display().to_string(),
        ca_file.display().to_string(),
    ))
    .expect("tls config");
    let rustls_config = server::tls::server_config(&tls).expect("server config");

    let agent_acceptor = server::tls::PeerCertAcceptor::new(rustls_config.clone());
    let operator_acceptor = server::tls::PeerCertAcceptor::new(rustls_config);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the Agent plane");
    let addr = listener.local_addr().expect("addr");
    let agents = server::agent_app(state.clone(), admission);
    tokio::spawn(async move {
        axum_server::from_tcp(listener)
            .acceptor(agent_acceptor)
            .serve(agents.into_make_service())
            .await
            .expect("serve the Agent plane");
    });
    // The Operator plane, over the same certificate on its own listener (ADR-0066) — the half a
    // browser reaches, and the reason the verifier stays optional is no longer that it is here.
    let operator_listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind the Operator plane");
    let operator_addr = operator_listener.local_addr().expect("addr");
    let operators = server::operator_app(state);
    tokio::spawn(async move {
        axum_server::from_tcp(operator_listener)
            .acceptor(operator_acceptor)
            .serve(operators.into_make_service())
            .await
            .expect("serve the Operator plane");
    });
    // The temp dir must outlive the server task; leak it deliberately for the test's lifetime.
    let endpoint = format!("https://localhost:{}/v1/opamp", addr.port());
    std::mem::forget(dir);
    (endpoint, operator_addr.port(), pki.ca_pem.clone())
}

fn client(ca_pem: &str, identity: Option<(&str, &str)>) -> reqwest::Client {
    server::tls::install_ring_provider();
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        .tls_certs_only([reqwest::Certificate::from_pem(ca_pem.as_bytes()).expect("ca")])
        // The certificate is for `localhost`, the listener is on 127.0.0.1.
        .resolve(
            "localhost",
            "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
        );
    if let Some((cert, key)) = identity {
        let mut pem = key.as_bytes().to_vec();
        pem.extend_from_slice(cert.as_bytes());
        builder = builder.identity(reqwest::Identity::from_pem(&pem).expect("identity"));
    }
    builder.build().expect("client")
}

async fn post(
    client: &reqwest::Client,
    endpoint: &str,
    message: AgentToServer,
) -> reqwest::Response {
    client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .body(message.encode_to_vec())
        .send()
        .await
        .expect("send")
}

/// The channel half: with a client CA configured, a peer that presents a certificate reaches the
/// OpAMP endpoint and one that presents none is refused — while the Operator plane on its own
/// listener (ADR-0066) keeps serving the REST API to a peer with no certificate at all, and the
/// package download on the *same* listener as OpAMP stays reachable without one too. Those two are
/// why client authentication is optional at the TLS layer and required on the route (ADR-0035).
#[tokio::test]
async fn a_client_certificate_is_required_on_the_opamp_route_and_nowhere_else() {
    let pki = Pki::new();
    let (endpoint, operator_port, ca_pem) = serve(&pki, Admission::new(None, true), None).await;
    let (cert, key) = pki.issue("edge-01");

    let with_certificate = client(&ca_pem, Some((&cert, &key)));
    let uid = InstanceUid::default();
    let response = post(&with_certificate, &endpoint, report(&uid)).await;
    assert!(response.status().is_success(), "{:?}", response.status());

    let without = client(&ca_pem, None);
    let response = post(&without, &endpoint, report(&InstanceUid::default())).await;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a peer with no certificate must not reach the OpAMP endpoint"
    );

    // The Operator plane, on its own listener, stays reachable without one: a browser presents
    // nothing, and ADR-0013 leaves that plane open on purpose.
    let agents = format!("https://localhost:{operator_port}/api/v1/agents");
    let response = without.get(&agents).send().await.expect("send");
    assert!(response.status().is_success(), "{:?}", response.status());

    // And on the Agent plane the artifact download is deliberately outside the guard (ADR-0066):
    // a Client fetching a package presents no certificate, so this must reach the handler — `404`
    // for a package nobody uploaded, never the `401` the OpAMP route answers above.
    let download = endpoint.replace(
        "/v1/opamp",
        "/api/v1/packages/otelcol/otelcol/1.0.0/file?os=linux&arch=amd64",
    );
    let response = without.get(&download).send().await.expect("send");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND,
        "the download must reach the handler without a certificate"
    );
}

/// Every configured proof must succeed, not the first that happens to pass: with both a credential
/// and a client CA configured, a valid certificate alone is not admission.
#[tokio::test]
async fn a_certificate_does_not_stand_in_for_the_credential() {
    let pki = Pki::new();
    let auth = OpampAuth::from_config(
        &toml::from_str::<server::config::AuthConfig>("bearer_tokens = [\"secret\"]")
            .expect("auth config"),
    );
    let (endpoint, _, ca_pem) = serve(&pki, Admission::new(Some(auth), true), None).await;
    let (cert, key) = pki.issue("edge-01");
    let client = client(&ca_pem, Some((&cert, &key)));

    let response = post(&client, &endpoint, report(&InstanceUid::default())).await;
    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a certificate is one proof of two while [auth] is configured"
    );

    let response = client
        .post(&endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::AUTHORIZATION, "Bearer secret")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await
        .expect("send");
    assert!(response.status().is_success(), "{:?}", response.status());
}

/// The CSR flow: an Agent that asks over a connection it was admitted on gets a certificate back
/// in an ordinary connection-settings offer, and the Server declares the capability that says so.
#[tokio::test]
async fn a_csr_is_answered_with_an_issued_certificate() {
    let pki = Pki::new();
    let ca_cert = pki.ca_pem.clone();
    let ca_key = pki.ca_key_pem.clone();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ca.pem"), &ca_cert).expect("write");
    std::fs::write(dir.path().join("ca-key.pem"), &ca_key).expect("write");
    let client_ca = ClientCa::from_config(
        &toml::from_str::<server::config::ClientCaConfig>(&format!(
            "cert_file = {:?}\nkey_file = {:?}\nvalidity_days = 30\n",
            dir.path().join("ca.pem").display().to_string(),
            dir.path().join("ca-key.pem").display().to_string(),
        ))
        .expect("client_ca config"),
    )
    .expect("client ca");

    let (endpoint, _, ca_pem) = serve(&pki, Admission::open(), Some(client_ca)).await;
    let (bootstrap_cert, bootstrap_key) = pki.issue("bootstrap");
    let http = client(&ca_pem, Some((&bootstrap_cert, &bootstrap_key)));

    let (csr, _key) = csr_for("edge-01");
    let mut message = report(&InstanceUid::default());
    message.connection_settings_request = Some(ConnectionSettingsRequest {
        opamp: Some(OpAmpConnectionSettingsRequest {
            certificate_request: Some(CertificateRequest { csr }),
        }),
    });
    let response = post(&http, &endpoint, message).await;
    assert!(response.status().is_success());
    let reply = ServerToAgent::decode(response.bytes().await.expect("body")).expect("decode");

    assert_ne!(
        reply.capabilities
            & opamp::proto::ServerCapabilities::AcceptsConnectionSettingsRequest as u64,
        0,
        "a Server with a [client_ca] declares that it signs"
    );
    let settings = reply
        .connection_settings
        .expect("an offer")
        .opamp
        .expect("opamp settings");
    let certificate = settings.certificate.expect("an issued certificate");
    let issued = String::from_utf8(certificate.cert).expect("pem");
    assert!(
        issued.starts_with("-----BEGIN CERTIFICATE-----"),
        "{issued}"
    );
    assert!(
        certificate.private_key.is_empty(),
        "the Agent keeps its own key — the Server has none to send"
    );
}

/// The Baseline's MUST: a request the Server cannot act on is answered with a `BadRequest` error
/// response. Here the Server signs nothing at all, so no Agent should be asking.
#[tokio::test]
async fn a_csr_to_a_server_that_signs_nothing_is_a_bad_request() {
    let pki = Pki::new();
    let (endpoint, _, ca_pem) = serve(&pki, Admission::open(), None).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&ca_pem, Some((&cert, &key)));

    let (csr, _key) = csr_for("edge-01");
    let mut message = report(&InstanceUid::default());
    message.connection_settings_request = Some(ConnectionSettingsRequest {
        opamp: Some(OpAmpConnectionSettingsRequest {
            certificate_request: Some(CertificateRequest { csr }),
        }),
    });
    let response = post(&http, &endpoint, message).await;
    let reply = ServerToAgent::decode(response.bytes().await.expect("body")).expect("decode");
    let error = reply.error_response.expect("an error response");
    assert_eq!(error.r#type, ServerErrorResponseType::BadRequest as i32);
    assert!(reply.connection_settings.is_none());
}
