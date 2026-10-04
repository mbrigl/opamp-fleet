//! Mutual TLS, enrolment and the CSR flow, end to end over the real listener (ADR-0026).
//!
//! What these cover is the part that cannot be unit-tested: the handshake actually carrying a
//! client certificate into the OpAMP route, and the admission rule that every configured proof
//! must succeed. The signing itself is covered where it lives, in `fleet_server::ca`.

use std::sync::Arc;

use fleet_server::ca::ClientCa;
use fleet_server::fleet::AppState;
use fleet_server::revocation::{CertId, Revocations};
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

    /// A CA whose subject has several parts, one with a comma in it — what an issuer typed as
    /// text would fail to match.
    fn with_organisation() -> Self {
        let key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        params
            .distinguished_name
            .push(rcgen::DnType::CountryName, "DE");
        params
            .distinguished_name
            .push(rcgen::DnType::OrganizationName, "Acme, Inc.");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "fleet client CA");
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).expect("ca");
        Pki {
            ca_pem: cert.pem(),
            ca_key_pem: key.serialize_pem(),
        }
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
    /// The register and the revocation list (ADR-0031).
    revocations: bool,
    /// The audit record (ADR-0030).
    audit: Option<Arc<dyn fleet_server::audit::Audit>>,
}

/// The CAs a revocation can name, as `main` takes them from `[tls]` and `[enrolment]`.
fn authorities(pki: &Pki, bootstrap: Option<&Pki>) -> Vec<fleet_server::revocation::Authority> {
    let of = |role: &str, ca: &Pki| {
        let der = opamp::tls::certificates(ca.ca_pem.as_bytes()).expect("pem");
        let facts = fleet_server::ca::facts(der[0].as_ref()).expect("facts");
        fleet_server::revocation::Authority {
            role: role.to_string(),
            subject: facts.id.issuer,
            name: facts.issuer_name,
        }
    };
    let mut authorities = vec![of("client", pki)];
    authorities.extend(bootstrap.map(|b| of("bootstrap", b)));
    authorities
}

/// What `serve` hands a test.
struct Served {
    endpoint: String,
    operator_port: u16,
    ca_pem: String,
    state: Arc<AppState>,
    revocations: Option<Arc<Revocations>>,
}

/// Serves both planes over TLS on ephemeral ports (ADR-0023), the Agent plane requiring a client
/// certificate in the handshake (ADR-0026) and the Operator plane asking for none, exactly as the
/// binary builds them.
async fn serve(pki: &Pki, setup: Setup<'_>) -> Served {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
    let enrolment = setup
        .bootstrap
        .map(|_| Arc::new(fleet_server::enrolment::Enrolment::new(clock.clone())));
    let revocations = setup.revocations.then(|| {
        let accepted: Vec<String> = setup
            .token
            .map(|token| format!("Bearer {token}"))
            .into_iter()
            .collect();
        Arc::new(
            Revocations::open(
                Box::new(
                    fleet_server::fs::FsLedgerStore::open(dir.path().join("revocation"))
                        .expect("ledger"),
                ),
                clock.clone(),
                Arc::new(move |authorization: &str| accepted.iter().any(|a| a == authorization)),
                authorities(pki, setup.bootstrap),
            )
            .expect("revocations"),
        )
    });
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("state")
            .with_client_ca(setup.client_ca)
            .with_connection_offer(setup.offer)
            .with_enrolment(enrolment.clone())
            .with_revocations(revocations.clone())
            .with_audit(setup.audit.clone()),
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
                "bearer_tokens = [{:?}]",
                fleet_server::credentials::bearer_entry(token)
            ))
            .expect("auth config"),
        )
        .expect("auth")
    });
    let mut admission = Admission::new(auth, true)
        .with_enrolment(planes.issuers, enrolment)
        .with_revocations(revocations.clone())
        .with_audit(setup.audit.clone());
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
        revocations,
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
/// while the Operator plane on its own listener serves a browser that presents none (ADR-0023,
/// ADR-0026).
/// Verifies: ADR-0026, ADR-0023, G-17
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
/// Verifies: ADR-0026, G-17
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
/// says so (ADR-0026 clause 9).
/// Verifies: ADR-0026
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
/// Verifies: ADR-0026, ADR-0027
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
/// the endpoint answers `503` — the credential was right, so it is no failure (ADR-0026 clauses
/// 20, 21, 24).
/// Verifies: ADR-0026
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
/// the fleet's members get never reaches an enrolling host (ADR-0026 clauses 4, 20 to 23).
/// Verifies: ADR-0026
#[tokio::test]
async fn an_enrolment_request_waits_for_an_operator_and_is_issued_on_approval() {
    let pki = Pki::new();
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let secrets = tempfile::tempdir().expect("tempdir");
    let rotated = secrets.path().join("rotated");
    std::fs::write(&rotated, "rotated\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&rotated, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    let offer = toml::from_str::<fleet_server::config::ConnectionOfferConfig>(&format!(
        "bearer_token_file = {:?}",
        rotated.display().to_string()
    ))
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
/// certificate again (ADR-0026 clauses 20, 21).
/// Verifies: ADR-0026
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
/// credential is compared — the right credential included (ADR-0026 clause 24).
/// Verifies: ADR-0026
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
/// subject with it is refused when the Server builds its TLS material (ADR-0026 clause 19).
/// Verifies: ADR-0026
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
/// approval and a rejection alike (ADR-0026 clause 22).
/// Verifies: ADR-0026
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
/// is still admitted on the Agent plane (ADR-0026 clause 24).
/// Verifies: ADR-0026
#[tokio::test]
async fn the_operator_plane_counts_its_failures_in_a_table_of_its_own() {
    let pki = Pki::new();
    let limits = || fleet_server::throttle::Limits {
        max_failures: 3,
        window_secs: 60,
        backoff_secs: 300,
    };
    let rest_auth = toml::from_str::<fleet_server::config::RestAuthConfig>(&format!(
        "[basic_users]\nops = {:?}\n",
        fleet_server::credentials::hash_basic("s3cret").expect("hash")
    ))
    .expect("rest auth config");
    let operator_auth = fleet_server::api::OperatorAuth::from_config(&rest_auth)
        .expect("operator auth")
        .with_throttle(Arc::new(fleet_server::throttle::Throttle::new(
            limits(),
            Arc::new(fleet_server::clock::SystemClock),
        )));
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

// ---- Revocation and the end of a session (ADR-0031) ----

/// The issuer and serial of a certificate in PEM.
fn cert_id(pem: &str) -> CertId {
    let der = opamp::tls::certificates(pem.as_bytes()).expect("pem");
    fleet_server::ca::facts(der[0].as_ref()).expect("facts").id
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A WebSocket to the Agent plane presenting `cert`, with `token` as the credential when given.
async fn websocket(
    served: &Served,
    cert: &str,
    key: &str,
    token: Option<&str>,
) -> Result<Socket, tokio_tungstenite::tungstenite::Error> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    opamp::tls::install_ring_provider();
    let config = opamp::tls::client_builder()
        .with_root_certificates(opamp::tls::root_store(served.ca_pem.as_bytes()).expect("roots"))
        .with_client_auth_cert(
            opamp::tls::certificates(cert.as_bytes()).expect("cert"),
            opamp::tls::private_key(key.as_bytes()).expect("key"),
        )
        .expect("client auth");
    let mut request = served
        .endpoint
        .replace("https://", "wss://")
        .into_client_request()
        .expect("request");
    if let Some(token) = token {
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("header"),
        );
    }
    let port = served
        .endpoint
        .rsplit(':')
        .next()
        .and_then(|rest| rest.split('/').next())
        .expect("port")
        .to_string();
    let tcp = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
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
}

/// Sends one report and waits for its reply, so the session is known to be up.
async fn exchange(socket: &mut Socket, uid: &InstanceUid) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let framed = opamp::frame::encode_within(&report(uid), usize::MAX).expect("frame");
    socket.send(Ws::Binary(framed.into())).await.expect("send");
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("a reply in time")
        {
            Some(Ok(Ws::Binary(_))) => return,
            Some(Ok(_)) => continue,
            other => panic!("the session ended early: {other:?}"),
        }
    }
}

/// The close frame the Server ends `socket` with, within `secs`.
async fn closed_with(socket: &mut Socket, secs: u64) -> (u16, String) {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as Ws;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("closed in time");
        match message {
            Some(Ok(Ws::Close(Some(frame)))) => {
                return (frame.code.into(), frame.reason.to_string())
            }
            Some(Ok(Ws::Close(None))) | None => panic!("closed without a code"),
            Some(Ok(_)) => continue,
            Some(Err(e)) => panic!("{e}"),
        }
    }
}

/// Whether `socket` is still open after `millis`: nothing arrives that ends it.
async fn still_open(socket: &mut Socket, millis: u64) -> bool {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message as Ws;
    match tokio::time::timeout(std::time::Duration::from_millis(millis), socket.next()).await {
        Err(_) => true,
        Ok(Some(Ok(Ws::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => false,
        Ok(Some(Ok(_))) => true,
    }
}

fn revocations_setup(pki: &Pki) -> Setup<'static> {
    Setup {
        token: Some("secret"),
        client_ca: Some(client_ca_of(pki)),
        revocations: true,
        ..Setup::default()
    }
}

fn authorized(message: AgentToServer) -> (AgentToServer, &'static str) {
    (message, "Bearer secret")
}

async fn post_as(
    client: &reqwest::Client,
    endpoint: &str,
    message: AgentToServer,
) -> reqwest::Response {
    let (message, authorization) = authorized(message);
    client
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::AUTHORIZATION, authorization)
        .body(message.encode_to_vec())
        .send()
        .await
        .expect("send")
}

/// A revoked certificate is refused on plain HTTP, on the WebSocket upgrade and on the download,
/// with the challenge and without saying which proof was revoked; revoking it through the REST API
/// lists it, and lifting it admits the certificate again (ADR-0031 clauses 3, 7, 8).
/// Verifies: ADR-0031
#[tokio::test]
async fn a_revoked_certificate_is_refused_on_both_transports_and_the_download() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let uid = InstanceUid::default();
    assert!(post_as(&http, &served.endpoint, report(&uid))
        .await
        .status()
        .is_success());

    let id = cert_id(&cert);
    let operator = client(&served.ca_pem, None);
    let revocations = format!(
        "https://localhost:{}/api/v1/revocations",
        served.operator_port
    );
    let response = operator
        .post(&revocations)
        .json(&serde_json::json!({"certificate": {"authority": "client", "serial": id.serial}}))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let entry: serde_json::Value = response.json().await.expect("json");

    let refused = post_as(&http, &served.endpoint, report(&uid)).await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(refused
        .headers()
        .contains_key(reqwest::header::WWW_AUTHENTICATE));
    assert!(!refused.text().await.expect("body").contains("revoked"));
    assert!(
        websocket(&served, &cert, &key, Some("secret"))
            .await
            .is_err(),
        "a revoked certificate passed the upgrade"
    );
    let download = served.endpoint.replace(
        "/v1/opamp",
        "/api/v1/packages/otelcol/1.0.0/file?os=linux&arch=amd64",
    );
    let response = http.get(&download).send().await.expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);

    let listed: serde_json::Value = operator
        .get(&revocations)
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(listed[0]["id"], entry["id"]);
    let lifted = operator
        .delete(format!(
            "{revocations}/{}",
            entry["id"].as_str().expect("id")
        ))
        .send()
        .await
        .expect("send");
    assert_eq!(lifted.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(post_as(&http, &served.endpoint, report(&uid))
        .await
        .status()
        .is_success());
}

/// A revoked credential is refused, and ends every session it admitted; a credential `[auth]` does
/// not hold cannot be revoked (ADR-0031 clauses 5, 8, 9, 11).
/// Verifies: ADR-0031
#[tokio::test]
async fn a_revoked_credential_ends_its_session_and_is_refused() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let revocations = served.revocations.clone().expect("armed");
    let (cert, key) = pki.issue("edge-01");
    let mut socket = websocket(&served, &cert, &key, Some("secret"))
        .await
        .expect("admitted");
    exchange(&mut socket, &InstanceUid::default()).await;

    assert!(revocations.revoke_credential("Bearer typo").is_err());
    revocations
        .revoke_credential("Bearer secret")
        .expect("revoke");
    assert_eq!(
        closed_with(&mut socket, 5).await,
        (1008, "revoked".to_string())
    );
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let refused = post_as(&http, &served.endpoint, report(&InstanceUid::default())).await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// A revocation closes the sessions it concerns at once and leaves every other one running
/// (ADR-0031 clause 9).
/// Verifies: ADR-0031
#[tokio::test]
async fn a_revocation_closes_the_session_it_concerns_and_no_other() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (revoked_cert, revoked_key) = pki.issue("edge-01");
    let (kept_cert, kept_key) = pki.issue("edge-02");
    let mut revoked = websocket(&served, &revoked_cert, &revoked_key, Some("secret"))
        .await
        .expect("admitted");
    let mut kept = websocket(&served, &kept_cert, &kept_key, Some("secret"))
        .await
        .expect("admitted");
    exchange(&mut revoked, &InstanceUid::default()).await;
    let kept_uid = InstanceUid::default();
    exchange(&mut kept, &kept_uid).await;

    let id = cert_id(&revoked_cert);
    served
        .revocations
        .as_ref()
        .expect("armed")
        .revoke_certificate("client", &id.serial)
        .expect("revoke");
    assert_eq!(
        closed_with(&mut revoked, 5).await,
        (1008, "revoked".to_string())
    );
    assert!(
        still_open(&mut kept, 300).await,
        "an unrelated session was closed"
    );
    exchange(&mut kept, &kept_uid).await;
}

/// A session ends when the certificate that admitted it expires (ADR-0031 clause 10).
/// Verifies: ADR-0031
#[tokio::test]
async fn a_session_is_closed_when_its_certificate_expires() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let key = KeyPair::generate().expect("key");
    let mut params = CertificateParams::new(vec!["edge-01".to_string()]).expect("params");
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::minutes(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::seconds(3);
    let cert = params.signed_by(&key, &pki.issuer()).expect("signed").pem();
    let mut socket = websocket(&served, &cert, &key.serialize_pem(), Some("secret"))
        .await
        .expect("admitted");
    exchange(&mut socket, &InstanceUid::default()).await;
    assert_eq!(
        closed_with(&mut socket, 10).await,
        (1008, "certificate expired".to_string())
    );
}

/// A certificate renewed before its predecessor was revoked is revoked with it: the register
/// records the presented certificate as the issued one's predecessor (ADR-0031 clauses 1, 2, 4).
/// Verifies: ADR-0031
#[tokio::test]
async fn a_revocation_reaches_a_certificate_renewed_before_it() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (old_cert, old_key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&old_cert, &old_key)));
    let uid = InstanceUid::default();
    let (csr, new_key) = csr_for("edge-01");
    let mut message = report(&uid);
    message.connection_settings_request = Some(ConnectionSettingsRequest {
        opamp: Some(OpAmpConnectionSettingsRequest {
            certificate_request: Some(CertificateRequest { csr }),
        }),
    });
    let reply = decode(post_as(&http, &served.endpoint, message).await).await;
    let new_cert = String::from_utf8(
        reply
            .connection_settings
            .and_then(|s| s.opamp)
            .and_then(|o| o.certificate)
            .expect("renewed")
            .cert,
    )
    .expect("pem");

    let register: serde_json::Value = client(&served.ca_pem, None)
        .get(format!(
            "https://localhost:{}/api/v1/certificates",
            served.operator_port
        ))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    let old = cert_id(&old_cert);
    let new = cert_id(&new_cert);
    assert_eq!(register[0]["serial"], new.serial.as_str());
    assert_eq!(register[0]["predecessor"]["serial"], old.serial.as_str());
    assert_eq!(register[0]["instance_uid"], hex::encode(uid.as_bytes()));

    let renewed = client(&served.ca_pem, Some((&new_cert, &new_key.serialize_pem())));
    assert!(post_as(&renewed, &served.endpoint, report(&uid))
        .await
        .status()
        .is_success());
    served
        .revocations
        .as_ref()
        .expect("armed")
        .revoke_certificate("client", &old.serial)
        .expect("revoke");
    let refused = post_as(&renewed, &served.endpoint, report(&uid)).await;
    assert_eq!(
        refused.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a renewal escaped the revocation of its predecessor"
    );
}

/// A renewal of a certificate an operator provisioned names a host of its own; the operator
/// sees the host, what it speaks for, and can mark it as a Gateway (ADR-0026 clause 7).
/// Verifies: ADR-0026
#[tokio::test]
async fn an_operator_sees_each_host_and_can_mark_a_gateway() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let uid = InstanceUid::default();
    let (csr, _) = csr_for("edge-01");
    let reply = decode(post_as(&http, &served.endpoint, with_csr(&uid, csr)).await).await;
    assert!(reply
        .connection_settings
        .and_then(|s| s.opamp)
        .and_then(|o| o.certificate)
        .is_some());

    let operator = client(&served.ca_pem, None);
    let hosts: serde_json::Value = operator
        .get(format!(
            "https://localhost:{}/api/v1/hosts",
            served.operator_port
        ))
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(hosts.as_array().expect("a list").len(), 1, "{hosts}");
    assert_eq!(hosts[0]["certificates"], 1);
    assert_eq!(hosts[0]["gateway"], false);
    let host = hosts[0]["host"].as_str().expect("a host").to_string();

    let gateway = |host: &str| {
        operator
            .put(format!(
                "https://localhost:{}/api/v1/hosts/{host}/gateway",
                served.operator_port
            ))
            .json(&serde_json::json!({"gateway": true}))
            .send()
    };
    assert_eq!(
        gateway(&host).await.expect("send").status(),
        reqwest::StatusCode::NO_CONTENT
    );
    assert_eq!(
        gateway("nobody").await.expect("send").status(),
        reqwest::StatusCode::NOT_FOUND
    );
}

// ---- A CSR's claim to an instance_uid (ADR-0026) ----

fn csr_claiming(subject: &str) -> Vec<u8> {
    let key = KeyPair::generate().expect("client key");
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, subject);
    params
        .serialize_request(&key)
        .expect("csr")
        .pem()
        .expect("csr pem")
        .into_bytes()
}

fn with_csr(uid: &InstanceUid, csr: Vec<u8>) -> AgentToServer {
    let mut message = report(uid);
    message.connection_settings_request = Some(ConnectionSettingsRequest {
        opamp: Some(OpAmpConnectionSettingsRequest {
            certificate_request: Some(CertificateRequest { csr }),
        }),
    });
    message
}

/// Verifies: ADR-0026
#[tokio::test]
async fn a_csr_claiming_another_instance_uid_is_a_bad_request() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let sender = InstanceUid::default();
    let other = InstanceUid::default();
    let reply = decode(
        post_as(
            &http,
            &served.endpoint,
            with_csr(&sender, csr_claiming(&format!("agent {other}"))),
        )
        .await,
    )
    .await;
    let error = reply.error_response.expect("an error");
    assert_eq!(error.r#type, ServerErrorResponseType::BadRequest as i32);
    assert!(reply.connection_settings.is_none(), "nothing was signed");
    assert!(served
        .revocations
        .as_ref()
        .expect("armed")
        .issued()
        .is_empty());
}

/// Verifies: ADR-0026
#[tokio::test]
async fn a_csr_claiming_its_own_instance_uid_is_signed() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let sender = InstanceUid::default();
    let reply = decode(
        post_as(
            &http,
            &served.endpoint,
            with_csr(&sender, csr_claiming(&format!("urn:uuid:{sender}"))),
        )
        .await,
    )
    .await;
    assert!(reply.error_response.is_none(), "{:?}", reply.error_response);
    assert!(reply.connection_settings.is_some(), "signed");
}

/// The regression guard: this project's own Client claims nothing (ADR-0026 clause 31).
/// Verifies: ADR-0026
#[tokio::test]
async fn a_csr_claiming_nothing_is_signed_as_before() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let reply = decode(
        post_as(
            &http,
            &served.endpoint,
            with_csr(&InstanceUid::default(), csr_claiming("edge-01")),
        )
        .await,
    )
    .await;
    assert!(reply.error_response.is_none(), "{:?}", reply.error_response);
    assert!(reply.connection_settings.is_some(), "signed");
}

/// Verifies: ADR-0026
#[tokio::test]
async fn an_enrolment_csr_claiming_another_instance_uid_never_reaches_the_queue() {
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
    let enrolment = served.state.enrolment().expect("enrolment").clone();
    enrolment.open(60).expect("open");
    let (cert, key) = bootstrap.issue("bootstrap");
    let enrolling = client(&served.ca_pem, Some((&cert, &key)));
    let other = InstanceUid::default();
    let reply = decode(
        post(
            &enrolling,
            &served.endpoint,
            with_csr(&InstanceUid::default(), csr_claiming(&other.to_string())),
        )
        .await,
    )
    .await;
    let error = reply.error_response.expect("an error");
    assert_eq!(error.r#type, ServerErrorResponseType::BadRequest as i32);
    assert!(enrolment.pending().is_empty(), "it reached the queue");
}

/// The issuer is named by its role, so a CA whose subject has several parts — and a comma inside
/// one — is revoked as simply as any other; a role the Server does not have is refused (ADR-0031
/// clause 3).
/// Verifies: ADR-0031
#[tokio::test]
async fn a_revocation_names_its_issuer_by_role_whatever_the_issuer_is_called() {
    let pki = Pki::with_organisation();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let revocations = format!(
        "https://localhost:{}/api/v1/revocations",
        served.operator_port
    );
    let operator = client(&served.ca_pem, None);
    let unknown = operator
        .post(&revocations)
        .json(&serde_json::json!({"certificate": {"authority": "bootstrap", "serial": "01"}}))
        .send()
        .await
        .expect("send");
    assert_eq!(unknown.status(), reqwest::StatusCode::BAD_REQUEST);
    let serial = cert_id(&cert).serial;
    let colons: String = serial
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).expect("hex").to_uppercase())
        .collect::<Vec<_>>()
        .join(":");
    let response = operator
        .post(&revocations)
        .json(&serde_json::json!({"certificate": {"authority": "client", "serial": colons}}))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let refused = post_as(&http, &served.endpoint, report(&InstanceUid::default())).await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// A CSR descends from the certificate the connection presented, whichever Agent the message
/// names: a self-asserted `instance_uid` cannot lift a renewal out of its chain, and behind a
/// Gateway revoking the Gateway reaches what was renewed through it (ADR-0031 clauses 2, 11).
/// Verifies: ADR-0031
#[tokio::test]
async fn a_csr_for_another_agent_still_descends_from_the_presented_certificate() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("gateway-01");
    let mut socket = websocket(&served, &cert, &key, Some("secret"))
        .await
        .expect("admitted");
    exchange(&mut socket, &InstanceUid::default()).await;
    let downstream = InstanceUid::default();
    let framed =
        opamp::frame::encode_within(&with_csr(&downstream, csr_claiming("edge-02")), usize::MAX)
            .expect("frame");
    socket.send(Ws::Binary(framed.into())).await.expect("send");
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("a reply in time")
        {
            Some(Ok(Ws::Binary(_))) => break,
            Some(Ok(_)) => continue,
            other => panic!("the session ended early: {other:?}"),
        }
    }
    let revocations = served.revocations.as_ref().expect("armed");
    let issued = revocations.issued();
    assert_eq!(issued.len(), 1, "the CSR was signed");
    let presented = cert_id(&cert);
    assert_eq!(issued[0].predecessor.as_ref(), Some(&presented));
    assert_eq!(issued[0].instance_uid, hex::encode(downstream.as_bytes()));
    revocations
        .revoke_certificate("client", &presented.serial)
        .expect("revoke");
    assert!(
        revocations.is_certificate_revoked(&issued[0].facts.id),
        "a renewal under another instance_uid escaped its chain"
    );
}

// ---- The audit record (ADR-0030) ----

/// An audit record on the filesystem, in `dir`.
fn audit_in(dir: &std::path::Path) -> Arc<dyn fleet_server::audit::Audit> {
    Arc::new(
        fleet_server::audit_log::AuditLog::start(
            Box::new(fleet_server::fs::FsAuditStore::open(dir.to_path_buf()).expect("store")),
            fleet_server::audit_log::Limits::default(),
            Arc::new(fleet_server::clock::SystemClock),
        )
        .expect("audit"),
    )
}

/// Every entry written so far, once the writer has caught up.
async fn entries(dir: &std::path::Path) -> Vec<serde_json::Value> {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    fleet_server::fs::audit_files(dir)
        .expect("files")
        .iter()
        .flat_map(|file| {
            std::fs::read_to_string(file)
                .expect("read")
                .lines()
                .map(|line| serde_json::from_str(line).expect("json"))
                .collect::<Vec<serde_json::Value>>()
        })
        .collect()
}

fn named<'a>(entries: &'a [serde_json::Value], event: &str) -> Vec<&'a serde_json::Value> {
    entries.iter().filter(|e| e["event"] == event).collect()
}

/// A WebSocket admission and a refusal each leave exactly one entry, the refusal naming the
/// check that refused and never the credential presented.
/// Verifies: ADR-0030
#[tokio::test]
async fn an_admission_and_its_refusal_each_leave_one_entry() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit_in(dir.path())),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let mut socket = websocket(&served, &cert, &key, Some("secret"))
        .await
        .expect("admitted");
    exchange(&mut socket, &InstanceUid::default()).await;
    assert!(websocket(&served, &cert, &key, Some("wrong-guess"))
        .await
        .is_err());

    let all = entries(dir.path()).await;
    let admitted = named(&all, "admission.admitted");
    assert_eq!(admitted.len(), 1, "{all:#?}");
    assert_eq!(admitted[0]["transport"], "websocket");
    assert_eq!(admitted[0]["serial"], cert_id(&cert).serial.as_str());
    let refused = named(&all, "admission.refused");
    assert_eq!(refused.len(), 1, "{all:#?}");
    assert_eq!(refused[0]["check"], "credential");
    let text = serde_json::to_string(&all).expect("json");
    assert!(
        !text.contains("wrong-guess") && !text.contains("secret\""),
        "{text}"
    );
}

/// A renewal is recorded with the certificate it issued and the one it renewed.
/// Verifies: ADR-0030
#[tokio::test]
async fn an_issued_certificate_is_recorded_with_its_predecessor() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit_in(dir.path())),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let reply = decode(
        post_as(
            &http,
            &served.endpoint,
            with_csr(&InstanceUid::default(), csr_claiming("edge-01")),
        )
        .await,
    )
    .await;
    assert!(reply.connection_settings.is_some(), "signed");
    let all = entries(dir.path()).await;
    let signed = named(&all, "issuance.signed");
    assert_eq!(signed.len(), 1, "{all:#?}");
    assert_eq!(signed[0]["kind"], "renewal");
    assert_eq!(
        signed[0]["predecessor_serial"],
        cert_id(&cert).serial.as_str()
    );
}

/// A revocation records the session it ended.
/// Verifies: ADR-0030
#[tokio::test]
async fn a_revocation_records_the_sessions_it_ended() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit_in(dir.path())),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let mut socket = websocket(&served, &cert, &key, Some("secret"))
        .await
        .expect("admitted");
    exchange(&mut socket, &InstanceUid::default()).await;
    let serial = cert_id(&cert).serial;
    served
        .revocations
        .as_ref()
        .expect("armed")
        .revoke_certificate("client", &serial)
        .expect("revoke");
    closed_with(&mut socket, 5).await;
    let all = entries(dir.path()).await;
    let ended = named(&all, "session.ended");
    assert_eq!(ended.len(), 1, "{all:#?}");
    assert_eq!(ended[0]["reason"], "revoked");
    assert_eq!(ended[0]["serial"], serial.as_str());
}

/// An operator's act is recorded with the operator's name before it runs and with its outcome
/// after, and a refused sign-in is recorded too.
/// Verifies: ADR-0030
#[tokio::test]
async fn an_operator_act_names_the_operator() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let audit = audit_in(dir.path());
    let rest_auth = toml::from_str::<fleet_server::config::RestAuthConfig>(&format!(
        "[basic_users]\nops = {:?}\n",
        fleet_server::credentials::hash_basic("s3cret").expect("hash")
    ))
    .expect("rest auth config");
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit.clone()),
            operator_auth: Some(
                fleet_server::api::OperatorAuth::from_config(&rest_auth)
                    .expect("operator auth")
                    .with_audit(Some(audit)),
            ),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let operator = client(&served.ca_pem, None);
    let url = format!(
        "https://localhost:{}/api/v1/revocations",
        served.operator_port
    );
    let response = operator
        .post(&url)
        .basic_auth("ops", Some("s3cret"))
        .json(&serde_json::json!({"certificate": {"authority": "client", "serial": "abc123"}}))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let refused = operator
        .get(&url)
        .basic_auth("ops", Some("guess"))
        .send()
        .await
        .expect("send");
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);

    let all = entries(dir.path()).await;
    let acts = named(&all, "operator.act");
    assert_eq!(acts.len(), 2, "{all:#?}");
    assert_eq!(acts[0]["outcome"], "requested");
    assert_eq!(acts[0]["operator"], "ops");
    assert_eq!(acts[0]["path"], "/api/v1/revocations");
    assert_eq!(acts[1]["outcome"], "completed");
    assert_eq!(acts[1]["status"], 201);
    assert_eq!(named(&all, "revocation.revoked")[0]["serial"], "abc123");
    let refused = named(&all, "operator.refused");
    assert_eq!(refused.len(), 1, "{all:#?}");
    assert_eq!(refused[0]["operator"], "ops");
    assert!(!serde_json::to_string(&all).expect("json").contains("guess"));
}

/// A record that cannot be written stops admission: no Agent is admitted without its entry.
/// Verifies: ADR-0030
#[tokio::test]
async fn an_audit_that_cannot_write_refuses_admission() {
    struct Broken;
    impl fleet_server::audit_log::AuditStore for Broken {
        fn tail(&mut self) -> Result<Option<fleet_server::audit_log::Tail>, String> {
            Ok(None)
        }
        fn append(&mut self, _: u64, _: &str) -> Result<(), String> {
            Err("disk full".to_string())
        }
        fn current_bytes(&self) -> u64 {
            0
        }
        fn rotate(&mut self, _: usize) -> Result<Vec<(String, String)>, String> {
            Ok(Vec::new())
        }
    }
    let pki = Pki::new();
    let audit: Arc<dyn fleet_server::audit::Audit> = Arc::new(
        fleet_server::audit_log::AuditLog::start(
            Box::new(Broken),
            fleet_server::audit_log::Limits::default(),
            Arc::new(fleet_server::clock::SystemClock),
        )
        .expect("audit"),
    );
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    // The first admission is queued; its write fails, and from then on nothing is admitted.
    let _ = post_as(&http, &served.endpoint, report(&InstanceUid::default())).await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let other = client(
        &served.ca_pem,
        Some(pki.issue("edge-02"))
            .as_ref()
            .map(|(c, k)| (c.as_str(), k.as_str())),
    );
    let refused = post_as(&other, &served.endpoint, report(&InstanceUid::default())).await;
    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

// ---- The list a Gateway refuses by (ADR-0031 clause 12) ----

/// The certificate a CSR from `client` is answered with, and the key it was requested for.
async fn issued_through(served: &Served, client: &reqwest::Client, name: &str) -> (String, String) {
    let (csr, key) = csr_for(name);
    let reply = decode(
        post_as(
            client,
            &served.endpoint,
            with_csr(&InstanceUid::default(), csr),
        )
        .await,
    )
    .await;
    let cert = reply
        .connection_settings
        .and_then(|s| s.opamp)
        .and_then(|o| o.certificate)
        .expect("an issued certificate")
        .cert;
    (String::from_utf8(cert).expect("pem"), key.serialize_pem())
}

async fn fetch_list(
    served: &Served,
    client: &reqwest::Client,
    etag: Option<&str>,
) -> reqwest::Response {
    let mut request = client
        .get(
            served
                .endpoint
                .replace("/v1/opamp", "/v1/gateway/revocations"),
        )
        .header(reqwest::header::AUTHORIZATION, "Bearer secret");
    if let Some(etag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    request.send().await.expect("send")
}

/// Only a host marked as a Gateway is handed the list, and what it is handed names every revoked
/// certificate of the client CA with its renewals resolved; an unchanged list is answered `304`.
/// Verifies: ADR-0031
#[tokio::test]
async fn a_marked_gateway_is_handed_the_revoked_certificates_with_their_renewals() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let revocations = served.revocations.clone().expect("armed");

    // The Gateway's own certificate, issued by the Server so that it names a host.
    let (provisioned, provisioned_key) = pki.issue("gateway-01");
    let (gateway_cert, gateway_key) = issued_through(
        &served,
        &client(&served.ca_pem, Some((&provisioned, &provisioned_key))),
        "gateway-01",
    )
    .await;
    let gateway = client(&served.ca_pem, Some((&gateway_cert, &gateway_key)));
    assert_eq!(
        fetch_list(&served, &gateway, None).await.status(),
        reqwest::StatusCode::FORBIDDEN,
        "an unmarked member is handed the list"
    );
    let host = fleet_server::ca::facts(
        opamp::tls::certificates(gateway_cert.as_bytes()).expect("pem")[0].as_ref(),
    )
    .expect("facts")
    .host
    .expect("a host");
    assert!(revocations.set_gateway(&host, true).expect("mark"));

    let empty = fetch_list(&served, &gateway, None).await;
    assert_eq!(empty.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = empty.json().await.expect("json");
    assert_eq!(body["certificates"], serde_json::json!([]));

    // A member that renewed, and then the certificate it renewed from is revoked.
    let (member, member_key) = pki.issue("edge-02");
    let (renewed, _) = issued_through(
        &served,
        &client(&served.ca_pem, Some((&member, &member_key))),
        "edge-02",
    )
    .await;
    revocations
        .revoke_certificate("client", &cert_id(&member).serial)
        .expect("revoke");

    let listed = fetch_list(&served, &gateway, None).await;
    let etag = listed
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .expect("an ETag")
        .to_string();
    let body: serde_json::Value = listed.json().await.expect("json");
    let named: Vec<(String, String)> = body["certificates"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|c| {
            (
                c["issuer"].as_str().expect("issuer").to_string(),
                c["serial"].as_str().expect("serial").to_string(),
            )
        })
        .collect();
    for id in [cert_id(&member), cert_id(&renewed)] {
        assert!(
            named.contains(&(id.issuer.clone(), id.serial.clone())),
            "{id:?} not in {named:?}"
        );
    }
    assert_eq!(named.len(), 2, "{named:?}");

    assert_eq!(
        fetch_list(&served, &gateway, Some(&etag)).await.status(),
        reqwest::StatusCode::NOT_MODIFIED
    );
    let without_credential = gateway
        .get(
            served
                .endpoint
                .replace("/v1/opamp", "/v1/gateway/revocations"),
        )
        .send()
        .await
        .expect("send");
    assert_eq!(
        without_credential.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
}

/// A bootstrap certificate is admitted, while the window is open, to enrol and to nothing else:
/// the list a Gateway refuses by is not for it.
/// Verifies: ADR-0031
#[tokio::test]
async fn a_bootstrap_certificate_is_not_handed_the_list() {
    let pki = Pki::new();
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let served = serve(
        &pki,
        Setup {
            bootstrap: Some(&bootstrap),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let opened = client(&served.ca_pem, None)
        .post(operator(&served, "/api/v1/enrolment/window"))
        .json(&serde_json::json!({ "open_for_secs": 600 }))
        .send()
        .await
        .expect("open window");
    assert_eq!(opened.status(), 200);
    let (cert, key) = bootstrap.issue("bootstrap");
    let enrolling = client(&served.ca_pem, Some((&cert, &key)));
    assert_eq!(
        fetch_list(&served, &enrolling, None).await.status(),
        reqwest::StatusCode::FORBIDDEN
    );
}
