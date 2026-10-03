//! Mutual TLS, enrolment and the CSR flow, end to end over the real listener (ADR-0039).
//!
//! What these cover is the part that cannot be unit-tested: the handshake actually carrying a
//! client certificate into the OpAMP route, and the admission rule that every configured proof
//! must succeed. The signing itself is covered where it lives, in `fleet_server::ca`.

use std::sync::Arc;

use fleet_server::ca::ClientCa;
use fleet_server::fleet::AppState;
use fleet_server::transport::{Admission, OpampAuth};
use opamp::proto::{
    AgentCapabilities, AgentToServer, CertificateRequest, ConnectionSettingsRequest,
    OpAmpConnectionSettingsRequest, ServerErrorResponseType, ServerToAgent,
};
use opamp::uid::InstanceUid;
use prost::Message;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};

/// A throwaway PKI: one CA, a server certificate for `localhost`, and the ability to mint a client
/// certificate from it — the shape an operator's `[client_ca]` has.
struct Pki {
    ca_pem: String,
    ca_key_pem: String,
}

impl Pki {
    fn new() -> Self {
        Pki::named("opamp-fleet-test-ca")
    }

    /// A CA of its own subject — what a bootstrap CA must have to be told apart from the client CA.
    fn named(subject: &str) -> Self {
        let key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(vec![subject.to_string()]).expect("ca params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, subject);
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
    let mut params = CertificateParams::new(vec![name.to_string()]).expect("params");
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    let csr = params
        .serialize_request(&key)
        .expect("csr")
        .pem()
        .expect("csr pem");
    (csr.into_bytes(), key)
}

/// What a test serves with, as `main` would build it from `server.toml`.
#[derive(Default)]
struct Setup<'a> {
    /// `[auth]`'s one token; `None` leaves the credential check out.
    token: Option<&'static str>,
    client_ca: Option<ClientCa>,
    /// The bootstrap CA of `[enrolment]`.
    bootstrap: Option<&'a Pki>,
    throttle: Option<fleet_server::throttle::Limits>,
    /// `[connection_offer]`, which carries a credential.
    offer: Option<fleet_server::fleet::ConnectionOffer>,
    /// `[rest.auth]`, guarding the Operator plane.
    operator_auth: Option<fleet_server::api::OperatorAuth>,
}

/// What `serve` hands a test.
struct Served {
    endpoint: String,
    operator_port: u16,
    ca_pem: String,
    state: Arc<AppState>,
}

/// Serves both planes over TLS on ephemeral ports (ADR-0038), the Agent plane requiring a client
/// certificate in the handshake (ADR-0039) and the Operator plane asking for none, exactly as the
/// binary builds them.
async fn serve(pki: &Pki, setup: Setup<'_>) -> Served {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
    let enrolment = setup
        .bootstrap
        .map(|_| Arc::new(fleet_server::enrolment::Enrolment::new(clock.clone())));
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("state")
            .with_client_ca(setup.client_ca)
            .with_connection_offer(setup.offer)
            .with_enrolment(enrolment.clone()),
    );
    let (server_cert, server_key) = pki.issue("localhost");

    let cert_file = dir.path().join("server-cert.pem");
    let key_file = dir.path().join("server-key.pem");
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&cert_file, &server_cert).expect("write cert");
    std::fs::write(&key_file, &server_key).expect("write key");
    std::fs::write(&ca_file, &pki.ca_pem).expect("write ca");
    let tls = toml::from_str::<fleet_server::config::TlsConfig>(&format!(
        "cert_file = {:?}\nkey_file = {:?}\nclient_ca_file = {:?}\n",
        cert_file.display().to_string(),
        key_file.display().to_string(),
        ca_file.display().to_string(),
    ))
    .expect("tls config");
    let enrolment_config = setup.bootstrap.map(|bootstrap| {
        let file = dir.path().join("bootstrap-ca.pem");
        std::fs::write(&file, &bootstrap.ca_pem).expect("write bootstrap ca");
        fleet_server::config::EnrolmentConfig {
            bootstrap_ca_file: file,
        }
    });
    let planes =
        fleet_server::tls::server_tls(&tls, enrolment_config.as_ref()).expect("server material");
    let agent_tls = planes.agent.rustls_config().expect("agent plane config");
    let operator_tls = planes
        .operator
        .rustls_config()
        .expect("operator plane config");

    let auth = setup.token.map(|token| {
        OpampAuth::from_config(
            &toml::from_str::<fleet_server::config::AuthConfig>(&format!(
                "bearer_tokens = [{token:?}]"
            ))
            .expect("auth config"),
        )
    });
    let mut admission = Admission::new(auth, true).with_enrolment(planes.issuers, enrolment);
    if let Some(limits) = setup.throttle {
        admission = admission.with_throttle(Arc::new(fleet_server::throttle::Throttle::new(
            limits,
            clock.clone(),
        )));
    }

    let handle = opamp::server::listen::Handle::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind the Agent plane");
    let addr = listener.local_addr().expect("addr");
    let agents = fleet_server::agent_app(state.clone(), admission);
    tokio::spawn(
        fleet_server::listen::plane(listener, Some(agent_tls), 64, handle.clone()).serve(agents),
    );
    let operator_listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind the Operator plane");
    let operator_addr = operator_listener.local_addr().expect("addr");
    let operators = fleet_server::operator_app(state.clone(), setup.operator_auth);
    tokio::spawn(
        fleet_server::listen::plane(operator_listener, Some(operator_tls), 64, handle)
            .serve(operators),
    );
    // The temp dir must outlive the server task; leak it deliberately for the test's lifetime.
    std::mem::forget(dir);
    Served {
        endpoint: format!("https://localhost:{}/v1/opamp", addr.port()),
        operator_port: operator_addr.port(),
        ca_pem: pki.ca_pem.clone(),
        state,
    }
}

fn client(ca_pem: &str, identity: Option<(&str, &str)>) -> reqwest::Client {
    opamp::tls::install_ring_provider();
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

/// The handshake half: a peer without a certificate, or with one from a CA the Server does not
/// trust, never reaches the Agent plane — neither the OpAMP endpoint nor the package download —
/// while the Operator plane on its own listener serves a browser that presents none (ADR-0038,
/// ADR-0039).
/// Verifies: ADR-0039, ADR-0038
#[tokio::test]
async fn a_client_certificate_is_required_in_the_handshake_on_the_agent_plane() {
    let pki = Pki::new();
    let served = serve(&pki, Setup::default()).await;
    let (cert, key) = pki.issue("edge-01");

    let with_certificate = client(&served.ca_pem, Some((&cert, &key)));
    let response = post(
        &with_certificate,
        &served.endpoint,
        report(&InstanceUid::default()),
    )
    .await;
    assert!(response.status().is_success(), "{:?}", response.status());

    let without = client(&served.ca_pem, None);
    let refused = without
        .post(&served.endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await;
    assert!(
        refused.is_err(),
        "a peer without a certificate passed the handshake"
    );
    let (stranger_cert, stranger_key) = Pki::named("another-ca").issue("edge-01");
    let stranger = client(&served.ca_pem, Some((&stranger_cert, &stranger_key)));
    let refused = stranger
        .post(&served.endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await;
    assert!(
        refused.is_err(),
        "a certificate from another CA passed the handshake"
    );

    let download = served.endpoint.replace(
        "/v1/opamp",
        "/api/v1/packages/otelcol/1.0.0/file?os=linux&arch=amd64",
    );
    assert!(
        without.get(&download).send().await.is_err(),
        "the download sits behind the same handshake"
    );
    let response = with_certificate.get(&download).send().await.expect("send");
    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND,
        "a member reaches the download handler"
    );

    let agents = format!("https://localhost:{}/api/v1/agents", served.operator_port);
    let response = without.get(&agents).send().await.expect("send");
    assert!(response.status().is_success(), "{:?}", response.status());
}

/// Every configured proof must succeed, not the first that happens to pass: with both a credential
/// and a client CA configured, a valid certificate alone is not admission.
/// Verifies: ADR-0039
#[tokio::test]
async fn a_certificate_does_not_stand_in_for_the_credential() {
    let pki = Pki::new();
    let Served {
        endpoint, ca_pem, ..
    } = serve(
        &pki,
        Setup {
            token: Some("secret"),
            ..Setup::default()
        },
    )
    .await;
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

/// Renewal: a member that asks over a connection it was admitted on gets a certificate back at
/// once, in an ordinary connection-settings offer, and the Server declares the capability that
/// says so (ADR-0039 clause 9).
/// Verifies: ADR-0039
#[tokio::test]
async fn a_csr_is_answered_with_an_issued_certificate() {
    let pki = Pki::new();
    let ca_cert = pki.ca_pem.clone();
    let ca_key = pki.ca_key_pem.clone();
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ca.pem"), &ca_cert).expect("write");
    std::fs::write(dir.path().join("ca-key.pem"), &ca_key).expect("write");
    let client_ca = ClientCa::from_config(
        &toml::from_str::<fleet_server::config::ClientCaConfig>(&format!(
            "cert_file = {:?}\nkey_file = {:?}\nvalidity_days = 30\n",
            dir.path().join("ca.pem").display().to_string(),
            dir.path().join("ca-key.pem").display().to_string(),
        ))
        .expect("client_ca config"),
    )
    .expect("client ca");

    let Served {
        endpoint, ca_pem, ..
    } = serve(
        &pki,
        Setup {
            client_ca: Some(client_ca),
            ..Setup::default()
        },
    )
    .await;
    let (member_cert, member_key) = pki.issue("edge-01");
    let http = client(&ca_pem, Some((&member_cert, &member_key)));

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
/// Verifies: ADR-0039, ADR-0041
#[tokio::test]
async fn a_csr_to_a_server_that_signs_nothing_is_a_bad_request() {
    let pki = Pki::new();
    let Served {
        endpoint, ca_pem, ..
    } = serve(&pki, Setup::default()).await;
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

/// A report carrying a CSR, as an enrolling Client sends it.
fn csr_report(csr: Vec<u8>) -> AgentToServer {
    let mut message = report(&InstanceUid::default());
    message.connection_settings_request = Some(ConnectionSettingsRequest {
        opamp: Some(OpAmpConnectionSettingsRequest {
            certificate_request: Some(CertificateRequest { csr }),
        }),
    });
    message
}

/// A CA of its own for the CSRs an enrolment approves.
fn client_ca_of(pki: &Pki) -> ClientCa {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("ca.pem"), &pki.ca_pem).expect("write");
    std::fs::write(dir.path().join("ca-key.pem"), &pki.ca_key_pem).expect("write");
    let ca = ClientCa::from_config(
        &toml::from_str::<fleet_server::config::ClientCaConfig>(&format!(
            "cert_file = {:?}\nkey_file = {:?}\nvalidity_days = 30\n",
            dir.path().join("ca.pem").display().to_string(),
            dir.path().join("ca-key.pem").display().to_string(),
        ))
        .expect("client_ca config"),
    )
    .expect("client ca");
    std::mem::forget(dir);
    ca
}

fn operator(served: &Served, path: &str) -> String {
    format!("https://localhost:{}{path}", served.operator_port)
}

async fn decode(response: reqwest::Response) -> ServerToAgent {
    assert!(response.status().is_success(), "{:?}", response.status());
    ServerToAgent::decode(response.bytes().await.expect("body")).expect("decode")
}

/// A bootstrap certificate opens nothing outside an enrolment window: the handshake passes, and
/// the endpoint answers `503` — the credential was right, so it is no failure (ADR-0039 clauses
/// 20, 21, 24).
/// Verifies: ADR-0039
#[tokio::test]
async fn a_bootstrap_certificate_is_refused_outside_an_enrolment_window() {
    let pki = Pki::new();
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let served = serve(
        &pki,
        Setup {
            client_ca: Some(client_ca_of(&pki)),
            bootstrap: Some(&bootstrap),
            ..Setup::default()
        },
    )
    .await;
    let (cert, key) = bootstrap.issue("bootstrap");
    let enrolling = client(&served.ca_pem, Some((&cert, &key)));
    let response = post(
        &enrolling,
        &served.endpoint,
        report(&InstanceUid::default()),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

/// The whole enrolment: an operator opens the window, the host's request waits in the queue
/// without becoming an Agent, an operator approves it, and the host is handed its certificate. A
/// bootstrap certificate never reaches the package download, and the credential-bearing offer
/// the fleet's members get never reaches an enrolling host (ADR-0039 clauses 4, 20 to 23).
/// Verifies: ADR-0039
#[tokio::test]
async fn an_enrolment_request_waits_for_an_operator_and_is_issued_on_approval() {
    let pki = Pki::new();
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let offer =
        toml::from_str::<fleet_server::config::ConnectionOfferConfig>("bearer_token = \"rotated\"")
            .expect("offer config");
    let served = serve(
        &pki,
        Setup {
            client_ca: Some(client_ca_of(&pki)),
            bootstrap: Some(&bootstrap),
            offer: Some(fleet_server::fleet::ConnectionOffer::from_config(&offer).expect("offer")),
            ..Setup::default()
        },
    )
    .await;
    let operator_client = client(&served.ca_pem, None);
    let opened = operator_client
        .post(operator(&served, "/api/v1/enrolment/window"))
        .json(&serde_json::json!({ "open_for_secs": 600 }))
        .send()
        .await
        .expect("open window");
    assert_eq!(opened.status(), 200);

    let (cert, key) = bootstrap.issue("bootstrap");
    let enrolling = client(&served.ca_pem, Some((&cert, &key)));
    // A first report without a CSR is told the Server signs, and nothing else.
    let hello = decode(
        post(
            &enrolling,
            &served.endpoint,
            report(&InstanceUid::default()),
        )
        .await,
    )
    .await;
    assert_ne!(
        hello.capabilities
            & opamp::proto::ServerCapabilities::AcceptsConnectionSettingsRequest as u64,
        0
    );
    assert!(
        hello.connection_settings.is_none() && hello.remote_config.is_none(),
        "an enrolling host is offered neither the credential nor a configuration"
    );

    let (csr, _key) = csr_for("edge-01");
    let waiting = decode(post(&enrolling, &served.endpoint, csr_report(csr.clone())).await).await;
    assert!(
        waiting.connection_settings.is_none(),
        "nothing is issued before an operator decides"
    );
    assert!(
        served.state.snapshot().is_empty(),
        "an enrolling host is no Agent of the fleet"
    );

    let pending: serde_json::Value = operator_client
        .get(operator(&served, "/api/v1/enrolments"))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    let pending = pending.as_array().expect("array");
    assert_eq!(pending.len(), 1, "{pending:?}");
    let id = pending[0]["id"].as_str().expect("id").to_string();
    assert_eq!(pending[0]["subject"], "CN=edge-01");

    let download = served.endpoint.replace(
        "/v1/opamp",
        "/api/v1/packages/otelcol/1.0.0/file?os=linux&arch=amd64",
    );
    let response = enrolling.get(&download).send().await.expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);

    let approved = operator_client
        .post(operator(
            &served,
            &format!("/api/v1/enrolments/{id}/approve"),
        ))
        .send()
        .await
        .expect("approve");
    assert_eq!(approved.status(), 204);

    let issued = decode(post(&enrolling, &served.endpoint, csr_report(csr)).await).await;
    let certificate = issued
        .connection_settings
        .and_then(|offer| offer.opamp)
        .and_then(|opamp| opamp.certificate)
        .expect("the issued certificate");
    assert!(String::from_utf8_lossy(&certificate.cert).starts_with("-----BEGIN CERTIFICATE-----"));
}

/// A rejected request is answered `BadRequest`, and closing the window shuts out every bootstrap
/// certificate again (ADR-0039 clauses 20, 21).
/// Verifies: ADR-0039
#[tokio::test]
async fn a_rejected_request_is_refused_and_closing_the_window_shuts_enrolment() {
    let pki = Pki::new();
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let served = serve(
        &pki,
        Setup {
            client_ca: Some(client_ca_of(&pki)),
            bootstrap: Some(&bootstrap),
            ..Setup::default()
        },
    )
    .await;
    let operator_client = client(&served.ca_pem, None);
    operator_client
        .post(operator(&served, "/api/v1/enrolment/window"))
        .json(&serde_json::json!({ "open_for_secs": 600 }))
        .send()
        .await
        .expect("open window");
    let (cert, key) = bootstrap.issue("bootstrap");
    let enrolling = client(&served.ca_pem, Some((&cert, &key)));
    let (csr, _key) = csr_for("edge-02");
    decode(post(&enrolling, &served.endpoint, csr_report(csr.clone())).await).await;
    let id = served.state.enrolment().expect("enrolment").pending()[0]
        .id
        .clone();
    let rejected = operator_client
        .post(operator(
            &served,
            &format!("/api/v1/enrolments/{id}/reject"),
        ))
        .send()
        .await
        .expect("reject");
    assert_eq!(rejected.status(), 204);
    let answer = decode(post(&enrolling, &served.endpoint, csr_report(csr)).await).await;
    assert_eq!(
        answer.error_response.expect("an error").r#type,
        ServerErrorResponseType::BadRequest as i32
    );

    let closed = operator_client
        .delete(operator(&served, "/api/v1/enrolment/window"))
        .send()
        .await
        .expect("close");
    assert_eq!(closed.status(), 204);
    let response = post(
        &enrolling,
        &served.endpoint,
        report(&InstanceUid::default()),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

/// A peer address that fails admission too often is answered `429` with `Retry-After`, before its
/// credential is compared — the right credential included (ADR-0039 clause 24).
/// Verifies: ADR-0039
#[tokio::test]
async fn repeated_failures_from_one_address_are_throttled() {
    let pki = Pki::new();
    let served = serve(
        &pki,
        Setup {
            token: Some("secret"),
            throttle: Some(fleet_server::throttle::Limits {
                max_failures: 3,
                window_secs: 60,
                backoff_secs: 300,
            }),
            ..Setup::default()
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    for _ in 0..3 {
        let response = post(&http, &served.endpoint, report(&InstanceUid::default())).await;
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
    let response = http
        .post(&served.endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::AUTHORIZATION, "Bearer secret")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = response.headers()[reqwest::header::RETRY_AFTER]
        .to_str()
        .expect("text")
        .parse()
        .expect("seconds");
    assert!(wait > 0 && wait <= 300, "{wait}");
}

/// A bootstrap CA must be told apart from the client CA by its subject, so one that shares a
/// subject with it is refused when the Server builds its TLS material (ADR-0039 clause 19).
/// Verifies: ADR-0039
#[test]
fn a_bootstrap_ca_sharing_its_subject_with_the_client_ca_is_refused() {
    let pki = Pki::new();
    let twin = Pki::named("opamp-fleet-test-ca");
    let dir = tempfile::tempdir().expect("tempdir");
    let (server_cert, server_key) = pki.issue("localhost");
    let file = |name: &str, body: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, body).expect("write");
        path.display().to_string()
    };
    let tls = toml::from_str::<fleet_server::config::TlsConfig>(&format!(
        "cert_file = {:?}\nkey_file = {:?}\nclient_ca_file = {:?}\n",
        file("server-cert.pem", &server_cert),
        file("server-key.pem", &server_key),
        file("ca.pem", &pki.ca_pem),
    ))
    .expect("tls config");
    let enrolment = fleet_server::config::EnrolmentConfig {
        bootstrap_ca_file: file("bootstrap-ca.pem", &twin.ca_pem).into(),
    };
    let Err(err) = fleet_server::tls::server_tls(&tls, Some(&enrolment)) else {
        panic!("a bootstrap CA indistinguishable from the client CA was accepted");
    };
    assert!(
        err.contains("shares a CA with [tls] client_ca_file"),
        "{err}"
    );
}

/// An operator deciding a request that is not pending is told so: an unknown id is `404` for an
/// approval and a rejection alike (ADR-0039 clause 22).
/// Verifies: ADR-0039
#[tokio::test]
async fn an_unknown_enrolment_request_is_answered_404() {
    let pki = Pki::new();
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let served = serve(
        &pki,
        Setup {
            client_ca: Some(client_ca_of(&pki)),
            bootstrap: Some(&bootstrap),
            ..Setup::default()
        },
    )
    .await;
    let operator_client = client(&served.ca_pem, None);
    for decision in ["approve", "reject"] {
        let response = operator_client
            .post(operator(
                &served,
                &format!("/api/v1/enrolments/no-such-request/{decision}"),
            ))
            .send()
            .await
            .expect("send");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{decision}"
        );
    }
}

/// The Operator plane counts its own `401`s in a table of its own: an address in back-off there
/// is still admitted on the Agent plane (ADR-0039 clause 24).
/// Verifies: ADR-0039
#[tokio::test]
async fn the_operator_plane_counts_its_failures_in_a_table_of_its_own() {
    let pki = Pki::new();
    let limits = || fleet_server::throttle::Limits {
        max_failures: 3,
        window_secs: 60,
        backoff_secs: 300,
    };
    let rest_auth =
        toml::from_str::<fleet_server::config::RestAuthConfig>("[basic_users]\nops = \"s3cret\"\n")
            .expect("rest auth config");
    let operator_auth = fleet_server::api::OperatorAuth::from_config(&rest_auth).with_throttle(
        Arc::new(fleet_server::throttle::Throttle::new(
            limits(),
            Arc::new(fleet_server::clock::SystemClock),
        )),
    );
    let served = serve(
        &pki,
        Setup {
            token: Some("secret"),
            throttle: Some(limits()),
            operator_auth: Some(operator_auth),
            ..Setup::default()
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let agents = operator(&served, "/api/v1/agents");
    for _ in 0..3 {
        let response = http.get(&agents).send().await.expect("send");
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
    let response = http
        .get(&agents)
        .basic_auth("ops", Some("s3cret"))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    let response = http
        .post(&served.endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::AUTHORIZATION, "Bearer secret")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await
        .expect("send");
    assert!(
        response.status().is_success(),
        "the Agent plane's count is its own: {:?}",
        response.status()
    );
}
