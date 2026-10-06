//! Mutual TLS, enrolment and the CSR flow, end to end over the real listener (ADR-0059).
//!
//! What these cover is the part that cannot be unit-tested: the handshake actually carrying a
//! client certificate into the OpAMP route, and the admission rule that the certificate is the
//! whole of it. The signing itself is covered where it lives, in `fleet_server::ca`.

use std::sync::Arc;

use fleet_server::ca::ClientCa;
use fleet_server::fleet::AppState;
use fleet_server::revocation::{CertId, Revocations};
use fleet_server::transport::Admission;
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

    /// A certificate and key signed by this CA naming `host` the way the Server's own signer does
    /// (ADR-0059 clause 7): `urn:opamp-fleet:host:<host>`, and no other name.
    fn issue_to_host(&self, host: &str) -> (String, String) {
        let key = KeyPair::generate().expect("key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
        params.subject_alt_names.push(rcgen::SanType::URI(
            format!("{}{host}", fleet_server::ca::HOST_URI_PREFIX)
                .try_into()
                .expect("uri"),
        ));
        let cert = params.signed_by(&key, &self.issuer()).expect("signed");
        (cert.pem(), key.serialize_pem())
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
    client_ca: Option<ClientCa>,
    /// The bootstrap CA of `[enrolment]`.
    bootstrap: Option<&'a Pki>,
    throttle: Option<fleet_server::throttle::Limits>,
    /// `[connection_offer]`.
    offer: Option<fleet_server::fleet::ConnectionOffer>,
    /// `[rest.auth]`, guarding the Operator plane.
    operator_auth: Option<fleet_server::api::OperatorAuth>,
    /// The register and the revocation list (ADR-0065).
    revocations: bool,
    /// The audit record (ADR-0063).
    audit: Option<Arc<dyn fleet_server::audit::Audit>>,
    /// `[agent_rate_limit]` (ADR-0066).
    agent_rate: Option<fleet_server::agent_rate::Limits>,
    /// `max_agents`.
    max_agents: Option<usize>,
    /// The clock the enrolment window and the rate limit run on; the system's by default.
    clock: Option<Arc<dyn fleet_server::fleet::Clock>>,
    /// Package delivery over an empty store (ADR-0043).
    packages: bool,
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
    enrolment: Option<Arc<fleet_server::enrolment::Enrolment>>,
}

/// Serves both planes over TLS on ephemeral ports (ADR-0038), the Agent plane requiring a client
/// certificate in the handshake (ADR-0059) and the Operator plane asking for none, exactly as the
/// binary builds them.
async fn serve(pki: &Pki, setup: Setup<'_>) -> Served {
    let dir = tempfile::tempdir().expect("tempdir");
    let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
    let test_clock = setup.clock.clone().unwrap_or_else(|| clock.clone());
    let enrolment = setup
        .bootstrap
        .map(|_| Arc::new(fleet_server::enrolment::Enrolment::new(test_clock.clone())));
    let revocations = setup.revocations.then(|| {
        Arc::new(
            Revocations::open(
                Box::new(
                    fleet_server::fs::FsLedgerStore::open(dir.path().join("revocation"))
                        .expect("ledger"),
                ),
                clock.clone(),
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
            .with_audit(setup.audit.clone())
            .with_packages(setup.packages.then(|| {
                fleet_server::fleet::PackageOffering::new(
                    fleet_server::packages::PackageStore::open(dir.path().join("packages"))
                        .expect("package store"),
                    String::new(),
                )
                .expect("deployments")
            }))
            .with_max_agents(
                setup
                    .max_agents
                    .unwrap_or(fleet_server::fleet::DEFAULT_MAX_AGENTS),
            )
            .with_agent_rate(setup.agent_rate.map(|limits| {
                Arc::new(fleet_server::agent_rate::AgentRate::new(
                    limits,
                    100,
                    test_clock.clone(),
                ))
            })),
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

    let mut admission = Admission::new(true)
        .with_enrolment(planes.issuers, enrolment.clone())
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
        enrolment,
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
/// ADR-0059).
/// Verifies: ADR-0059, ADR-0054, G-17
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

/// The certificate is the whole of admission: a member presenting one is admitted on both
/// transports with no `Authorization` header, as the binary builds the Agent plane — revocation,
/// throttle and all.
/// Verifies: ADR-0059, G-17
#[tokio::test]
async fn a_member_is_admitted_on_its_certificate_alone() {
    let pki = Pki::new();
    let served = serve(
        &pki,
        Setup {
            throttle: Some(fleet_server::throttle::Limits::default()),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let response = post(&http, &served.endpoint, report(&InstanceUid::default())).await;
    assert!(response.status().is_success(), "{:?}", response.status());

    let mut socket = websocket(&served, &cert, &key, None)
        .await
        .expect("admitted on the upgrade");
    exchange(&mut socket, &InstanceUid::default()).await;
}

/// An `Authorization` header is never read on the Agent plane: whatever it holds, a member is
/// admitted on its certificate, on both transports and on the download, and a refusal carries no
/// challenge — so a Client of the previous version keeps connecting.
/// Verifies: ADR-0059
#[tokio::test]
async fn an_authorization_header_is_ignored() {
    let pki = Pki::new();
    let served = serve(
        &pki,
        Setup {
            throttle: Some(fleet_server::throttle::Limits {
                max_failures: 2,
                window_secs: 60,
                backoff_secs: 300,
            }),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let download = served.endpoint.replace(
        "/v1/opamp",
        "/api/v1/packages/otelcol/1.0.0/file?os=linux&arch=amd64",
    );
    // More attempts than the throttle allows failures: none of them is one.
    for authorization in [
        "Bearer an-old-fleet-token",
        "Basic ZmxlZXQ6c2VjcmV0",
        "Bearer",
        "garbage",
    ] {
        let response = http
            .post(&served.endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
            .header(reqwest::header::AUTHORIZATION, authorization)
            .body(report(&InstanceUid::default()).encode_to_vec())
            .send()
            .await
            .expect("send");
        assert!(
            response.status().is_success(),
            "{authorization}: {:?}",
            response.status()
        );
        let mut socket = websocket(&served, &cert, &key, Some(authorization))
            .await
            .expect("admitted on the upgrade");
        exchange(&mut socket, &InstanceUid::default()).await;
        let response = http
            .get(&download)
            .header(reqwest::header::AUTHORIZATION, authorization)
            .send()
            .await
            .expect("send");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{authorization}: the download handler is reached"
        );
    }

    // A refusal behind the handshake carries no challenge: no header could answer it.
    served
        .revocations
        .as_ref()
        .expect("armed")
        .revoke_certificate("client", &cert_id(&cert).serial)
        .expect("revoke");
    let refused = http
        .post(&served.endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::AUTHORIZATION, "Bearer an-old-fleet-token")
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await
        .expect("send");
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(!refused
        .headers()
        .contains_key(reqwest::header::WWW_AUTHENTICATE));
}

/// Renewal: a member that asks over a connection it was admitted on gets a certificate back at
/// once, in an ordinary connection-settings offer, and the Server declares the capability that
/// says so (ADR-0059 clause 9).
/// Verifies: ADR-0059
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
/// Verifies: ADR-0059, ADR-0060
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
/// the endpoint answers `503` — the handshake proved the certificate, so it is no failure
/// (ADR-0059 clauses 20, 21, 24).
/// Verifies: ADR-0059
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
/// bootstrap certificate never reaches the package download, and the standing offer the fleet's
/// members get never reaches an enrolling host (ADR-0059 clauses 4, 20 to 23).
/// Verifies: ADR-0059
#[tokio::test]
async fn an_enrolment_request_waits_for_an_operator_and_is_issued_on_approval() {
    let pki = Pki::new();
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let offer = toml::from_str::<fleet_server::config::ConnectionOfferConfig>(
        "heartbeat_interval_secs = 15\n",
    )
    .expect("offer config");
    let served = serve(
        &pki,
        Setup {
            client_ca: Some(client_ca_of(&pki)),
            bootstrap: Some(&bootstrap),
            offer: Some(fleet_server::fleet::ConnectionOffer::from_config(&offer)),
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
        "an enrolling host is offered neither the standing offer nor a configuration"
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
/// certificate again (ADR-0059 clauses 20, 21).
/// Verifies: ADR-0059
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

/// A peer address that fails admission too often — a revoked certificate retried — is answered
/// `429` with `Retry-After`, a valid certificate from the same address included (ADR-0059 clause
/// 24).
/// Verifies: ADR-0059
#[tokio::test]
async fn repeated_failures_from_one_address_are_throttled() {
    let pki = Pki::new();
    let served = serve(
        &pki,
        Setup {
            throttle: Some(fleet_server::throttle::Limits {
                max_failures: 3,
                window_secs: 60,
                backoff_secs: 300,
            }),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    served
        .revocations
        .as_ref()
        .expect("armed")
        .revoke_certificate("client", &cert_id(&cert).serial)
        .expect("revoke");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    for _ in 0..3 {
        let response = post(&http, &served.endpoint, report(&InstanceUid::default())).await;
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
    let (valid_cert, valid_key) = pki.issue("edge-02");
    let valid = client(&served.ca_pem, Some((&valid_cert, &valid_key)));
    let response = post(&valid, &served.endpoint, report(&InstanceUid::default())).await;
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = response.headers()[reqwest::header::RETRY_AFTER]
        .to_str()
        .expect("text")
        .parse()
        .expect("seconds");
    assert!(wait > 0 && wait <= 300, "{wait}");
}

/// A bootstrap CA must be told apart from the client CA by its subject, so one that shares a
/// subject with it is refused when the Server builds its TLS material (ADR-0059 clause 19).
/// Verifies: ADR-0059
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
/// approval and a rejection alike (ADR-0059 clause 22).
/// Verifies: ADR-0059
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
/// is still admitted on the Agent plane (ADR-0059 clause 24).
/// Verifies: ADR-0059
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

    let response = post(&http, &served.endpoint, report(&InstanceUid::default())).await;
    assert!(
        response.status().is_success(),
        "the Agent plane's count is its own: {:?}",
        response.status()
    );
}

// ---- Revocation and the end of a session (ADR-0065) ----

/// The issuer and serial of a certificate in PEM.
fn cert_id(pem: &str) -> CertId {
    let der = opamp::tls::certificates(pem.as_bytes()).expect("pem");
    fleet_server::ca::facts(der[0].as_ref()).expect("facts").id
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A WebSocket to the Agent plane presenting `cert`, sending `authorization` as an `Authorization`
/// header when given — what a Client of the previous version still sends.
async fn websocket(
    served: &Served,
    cert: &str,
    key: &str,
    authorization: Option<&str>,
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
    if let Some(authorization) = authorization {
        request
            .headers_mut()
            .insert("authorization", authorization.parse().expect("header"));
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
        client_ca: Some(client_ca_of(pki)),
        revocations: true,
        ..Setup::default()
    }
}

/// A revoked certificate is refused on plain HTTP, on the WebSocket upgrade and on the download,
/// without a challenge and without saying it was revoked; revoking it through the REST API lists
/// it, and lifting it admits the certificate again (ADR-0065 clauses 3, 7, 8).
/// Verifies: ADR-0065
#[tokio::test]
async fn a_revoked_certificate_is_refused_on_both_transports_and_the_download() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let uid = InstanceUid::default();
    assert!(post(&http, &served.endpoint, report(&uid))
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

    let refused = post(&http, &served.endpoint, report(&uid)).await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(
        !refused
            .headers()
            .contains_key(reqwest::header::WWW_AUTHENTICATE),
        "the Agent plane sends no challenge"
    );
    assert!(!refused.text().await.expect("body").contains("revoked"));
    assert!(
        websocket(&served, &cert, &key, None).await.is_err(),
        "a revoked certificate passed the upgrade"
    );
    let download = served.endpoint.replace(
        "/v1/opamp",
        "/api/v1/packages/otelcol/1.0.0/file?os=linux&arch=amd64",
    );
    let response = http.get(&download).send().await.expect("send");
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(!response
        .headers()
        .contains_key(reqwest::header::WWW_AUTHENTICATE));

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
    assert!(post(&http, &served.endpoint, report(&uid))
        .await
        .status()
        .is_success());
}

/// There is no credential to revoke: naming one is answered `400`, naming the field, and the list
/// is left as it was (ADR-0065 clause 5).
/// Verifies: ADR-0065
#[tokio::test]
async fn a_credential_revocation_is_answered_400() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let url = operator(&served, "/api/v1/revocations");
    let operator = client(&served.ca_pem, None);
    for body in [
        serde_json::json!({"credential": "Bearer an-old-fleet-token"}),
        serde_json::json!({
            "credential": "Bearer an-old-fleet-token",
            "certificate": {"authority": "client", "serial": "ab"}
        }),
    ] {
        let response = operator.post(&url).json(&body).send().await.expect("send");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{body}"
        );
        let text = response.text().await.expect("body");
        assert!(text.contains("credential"), "names the field: {text}");
        assert!(
            text.contains("admits by client certificate alone"),
            "says why: {text}"
        );
        assert!(!text.contains("an-old-fleet-token"), "{text}");
    }
    assert!(served
        .revocations
        .as_ref()
        .expect("armed")
        .list()
        .is_empty());
    let listed: serde_json::Value = operator
        .get(&url)
        .send()
        .await
        .expect("send")
        .json()
        .await
        .expect("json");
    assert_eq!(listed, serde_json::json!([]));
}

/// A body that names no certificate, or carries a field the route does not know, is malformed and
/// answered `400`, like every other malformed body; the list is left as it was.
/// Verifies: ADR-0065
#[tokio::test]
async fn a_revocation_naming_no_certificate_is_answered_400() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let url = operator(&served, "/api/v1/revocations");
    let operator = client(&served.ca_pem, None);
    for body in [
        serde_json::json!({}),
        serde_json::json!({"certificate": {"authority": "client"}}),
        serde_json::json!({
            "certificate": {"authority": "client", "serial": "ab"},
            "reason": "an unknown field"
        }),
    ] {
        let response = operator.post(&url).json(&body).send().await.expect("send");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{body}"
        );
    }
    assert!(served
        .revocations
        .as_ref()
        .expect("armed")
        .list()
        .is_empty());
}

/// A revocation closes the sessions it concerns at once and leaves every other one running
/// (ADR-0065 clause 9).
/// Verifies: ADR-0065
#[tokio::test]
async fn a_revocation_closes_the_session_it_concerns_and_no_other() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (revoked_cert, revoked_key) = pki.issue("edge-01");
    let (kept_cert, kept_key) = pki.issue("edge-02");
    let mut revoked = websocket(&served, &revoked_cert, &revoked_key, None)
        .await
        .expect("admitted");
    let mut kept = websocket(&served, &kept_cert, &kept_key, None)
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

/// A session ends when the certificate that admitted it expires (ADR-0065 clause 10).
/// Verifies: ADR-0065
#[tokio::test]
async fn a_session_is_closed_when_its_certificate_expires() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let key = KeyPair::generate().expect("key");
    let mut params = CertificateParams::new(vec!["edge-01".to_string()]).expect("params");
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::minutes(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::seconds(3);
    let cert = params.signed_by(&key, &pki.issuer()).expect("signed").pem();
    let mut socket = websocket(&served, &cert, &key.serialize_pem(), None)
        .await
        .expect("admitted");
    exchange(&mut socket, &InstanceUid::default()).await;
    assert_eq!(
        closed_with(&mut socket, 10).await,
        (1008, "certificate expired".to_string())
    );
}

/// A certificate renewed before its predecessor was revoked is revoked with it: the register
/// records the presented certificate as the issued one's predecessor (ADR-0065 clauses 1, 2, 4).
/// Verifies: ADR-0065
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
    let reply = decode(post(&http, &served.endpoint, message).await).await;
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
    assert!(post(&renewed, &served.endpoint, report(&uid))
        .await
        .status()
        .is_success());
    served
        .revocations
        .as_ref()
        .expect("armed")
        .revoke_certificate("client", &old.serial)
        .expect("revoke");
    let refused = post(&renewed, &served.endpoint, report(&uid)).await;
    assert_eq!(
        refused.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "a renewal escaped the revocation of its predecessor"
    );
}

/// A renewal of a certificate an operator provisioned names a host of its own; the operator
/// sees the host, what it speaks for, and can mark it as a Gateway (ADR-0059 clause 7).
/// Verifies: ADR-0059
#[tokio::test]
async fn an_operator_sees_each_host_and_can_mark_a_gateway() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let uid = InstanceUid::default();
    let (csr, _) = csr_for("edge-01");
    let reply = decode(post(&http, &served.endpoint, with_csr(&uid, csr)).await).await;
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

// ---- A CSR's claim to an instance_uid (ADR-0050) ----

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

/// Verifies: ADR-0050
#[tokio::test]
async fn a_csr_claiming_another_instance_uid_is_a_bad_request() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let sender = InstanceUid::default();
    let other = InstanceUid::default();
    let reply = decode(
        post(
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

/// Verifies: ADR-0050
#[tokio::test]
async fn a_csr_claiming_its_own_instance_uid_is_signed() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let sender = InstanceUid::default();
    let reply = decode(
        post(
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

/// The regression guard: this project's own Client claims nothing (ADR-0050 clause 4).
/// Verifies: ADR-0050
#[tokio::test]
async fn a_csr_claiming_nothing_is_signed_as_before() {
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let reply = decode(
        post(
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

/// Verifies: ADR-0050
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
/// one — is revoked as simply as any other; a role the Server does not have is refused (ADR-0065
/// clause 3).
/// Verifies: ADR-0065
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
    let refused = post(&http, &served.endpoint, report(&InstanceUid::default())).await;
    assert_eq!(refused.status(), reqwest::StatusCode::UNAUTHORIZED);
}

/// A CSR descends from the certificate the connection presented, whichever Agent the message
/// names: a self-asserted `instance_uid` cannot lift a renewal out of its chain, and behind a
/// Gateway revoking the Gateway reaches what was renewed through it (ADR-0065 clauses 2, 11).
/// Verifies: ADR-0065
#[tokio::test]
async fn a_csr_for_another_agent_still_descends_from_the_presented_certificate() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let pki = Pki::new();
    let served = serve(&pki, revocations_setup(&pki)).await;
    let (cert, key) = pki.issue("gateway-01");
    let mut socket = websocket(&served, &cert, &key, None)
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

// ---- The audit record (ADR-0063) ----

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
/// check that refused — here a revoked certificate.
/// Verifies: ADR-0063
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
    let mut socket = websocket(&served, &cert, &key, None)
        .await
        .expect("admitted");
    exchange(&mut socket, &InstanceUid::default()).await;
    let (revoked_cert, revoked_key) = pki.issue("edge-02");
    served
        .revocations
        .as_ref()
        .expect("armed")
        .revoke_certificate("client", &cert_id(&revoked_cert).serial)
        .expect("revoke");
    assert!(websocket(&served, &revoked_cert, &revoked_key, None)
        .await
        .is_err());

    let all = entries(dir.path()).await;
    let admitted = named(&all, "admission.admitted");
    assert_eq!(admitted.len(), 1, "{all:#?}");
    assert_eq!(admitted[0]["transport"], "websocket");
    assert_eq!(admitted[0]["serial"], cert_id(&cert).serial.as_str());
    let refused = named(&all, "admission.refused");
    assert_eq!(refused.len(), 1, "{all:#?}");
    assert_eq!(refused[0]["check"], "revoked");
}

/// An `Authorization` header a Client of the previous version sends is never written to the
/// record, neither its value nor any hash of it: the admission entry holds the certificate alone.
/// Verifies: ADR-0063
#[tokio::test]
async fn an_authorization_an_older_client_sends_is_never_written() {
    use sha2::Digest as _;
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
    let token = "an-old-fleet-token-of-32-characters";
    let authorization = format!("Bearer {token}");
    let mut socket = websocket(&served, &cert, &key, Some(&authorization))
        .await
        .expect("admitted on its certificate");
    exchange(&mut socket, &InstanceUid::default()).await;
    let http = client(&served.ca_pem, Some((&cert, &key)));
    let response = http
        .post(&served.endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
        .header(reqwest::header::AUTHORIZATION, &authorization)
        .body(report(&InstanceUid::default()).encode_to_vec())
        .send()
        .await
        .expect("send");
    assert!(response.status().is_success(), "{:?}", response.status());

    let all = entries(dir.path()).await;
    let admitted = named(&all, "admission.admitted");
    assert_eq!(admitted.len(), 2, "{all:#?}");
    let text = std::fs::read_dir(dir.path())
        .expect("dir")
        .map(|file| std::fs::read_to_string(file.expect("entry").path()).expect("read"))
        .collect::<String>();
    for secret in [
        token.to_string(),
        hex::encode(sha2::Sha256::digest(token.as_bytes())),
        hex::encode(sha2::Sha256::digest(authorization.as_bytes())),
    ] {
        assert!(!text.contains(&secret), "{secret} in {text}");
    }
    assert!(!text.contains("Bearer"), "{text}");
}

/// A renewal is recorded with the certificate it issued and the one it renewed.
/// Verifies: ADR-0063
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
        post(
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
/// Verifies: ADR-0063
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
    let mut socket = websocket(&served, &cert, &key, None)
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
/// Verifies: ADR-0063
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
/// Verifies: ADR-0063
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
    let _ = post(&http, &served.endpoint, report(&InstanceUid::default())).await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let other = client(
        &served.ca_pem,
        Some(pki.issue("edge-02"))
            .as_ref()
            .map(|(c, k)| (c.as_str(), k.as_str())),
    );
    let refused = post(&other, &served.endpoint, report(&InstanceUid::default())).await;
    assert_eq!(refused.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
}

/// A record that cannot be written stops issuance: a member whose session was admitted while the
/// record worked is handed no certificate once it fails.
/// Verifies: ADR-0063
#[tokio::test]
async fn an_audit_that_cannot_write_issues_no_certificate() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Failing(Arc<AtomicBool>);
    impl fleet_server::audit_log::AuditStore for Failing {
        fn tail(&mut self) -> Result<Option<fleet_server::audit_log::Tail>, String> {
            Ok(None)
        }
        fn append(&mut self, _: u64, _: &str) -> Result<(), String> {
            if self.0.load(Ordering::SeqCst) {
                Err("disk full".to_string())
            } else {
                Ok(())
            }
        }
        fn current_bytes(&self) -> u64 {
            0
        }
        fn rotate(&mut self, _: usize) -> Result<Vec<(String, String)>, String> {
            Ok(Vec::new())
        }
    }
    let pki = Pki::new();
    let failing = Arc::new(AtomicBool::new(false));
    let audit: Arc<dyn fleet_server::audit::Audit> = Arc::new(
        fleet_server::audit_log::AuditLog::start(
            Box::new(Failing(failing.clone())),
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
    let mut socket = websocket(&served, &cert, &key, None)
        .await
        .expect("admitted while the record works");
    let uid = InstanceUid::default();
    exchange(&mut socket, &uid).await;

    // The record fails at its next write — another peer's admission — and stays failed.
    failing.store(true, Ordering::SeqCst);
    let (other_cert, other_key) = pki.issue("edge-02");
    let _ = post(
        &client(&served.ca_pem, Some((&other_cert, &other_key))),
        &served.endpoint,
        report(&InstanceUid::default()),
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let (csr, _) = csr_for("edge-01");
    let framed = opamp::frame::encode_within(&with_csr(&uid, csr), usize::MAX).expect("frame");
    socket.send(Ws::Binary(framed.into())).await.expect("send");
    let reply = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("a reply in time")
        {
            Some(Ok(Ws::Binary(bytes))) => {
                break opamp::frame::decode::<ServerToAgent>(&bytes, usize::MAX).expect("decode")
            }
            Some(Ok(_)) => continue,
            other => panic!("the session ended early: {other:?}"),
        }
    };
    assert!(
        reply
            .connection_settings
            .and_then(|s| s.opamp)
            .and_then(|o| o.certificate)
            .is_none(),
        "a certificate was issued without its record"
    );
    let error = reply.error_response.expect("the request is held back");
    assert_eq!(
        error.r#type,
        opamp::proto::ServerErrorResponseType::Unavailable as i32,
        "a record that cannot be written is the Server's to recover from, so the Agent retries"
    );
    assert!(
        matches!(
            error.details,
            Some(opamp::proto::server_error_response::Details::RetryInfo(ref info))
                if info.retry_after_nanoseconds > 0
        ),
        "the Agent is told when to ask again: {:?}",
        error.details
    );
    assert!(served
        .revocations
        .as_ref()
        .expect("armed")
        .issued()
        .is_empty());
}

// ---- The list a Gateway refuses by (ADR-0065 clause 12) ----

/// The certificate a CSR from `client` is answered with, and the key it was requested for.
async fn issued_through(served: &Served, client: &reqwest::Client, name: &str) -> (String, String) {
    let (csr, key) = csr_for(name);
    let reply = decode(
        post(
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
    let mut request = client.get(
        served
            .endpoint
            .replace("/v1/opamp", "/v1/gateway/revocations"),
    );
    if let Some(etag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }
    request.send().await.expect("send")
}

/// A member whose host is not marked as a Gateway and asks for the list is refused, and the refusal
/// is recorded with the host that asked.
/// Verifies: ADR-0063
#[tokio::test]
async fn an_unmarked_member_asking_for_the_list_leaves_a_refusal() {
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
    let (provisioned, provisioned_key) = pki.issue("edge-01");
    let (cert, key) = issued_through(
        &served,
        &client(&served.ca_pem, Some((&provisioned, &provisioned_key))),
        "edge-01",
    )
    .await;
    let member = client(&served.ca_pem, Some((&cert, &key)));
    assert_eq!(
        fetch_list(&served, &member, None).await.status(),
        reqwest::StatusCode::FORBIDDEN
    );
    let host = fleet_server::ca::facts(
        opamp::tls::certificates(cert.as_bytes()).expect("pem")[0].as_ref(),
    )
    .expect("facts")
    .host
    .expect("a host");

    let all = entries(dir.path()).await;
    let refused = named(&all, "gateway_list.refused");
    assert_eq!(refused.len(), 1, "{all:#?}");
    assert_eq!(refused[0]["host"], host.as_str());
    assert_eq!(refused[0]["check"], "not a gateway");
}

/// Only a host marked as a Gateway is handed the list — admitted by its certificate alone — and
/// what it is handed names every revoked certificate of the client CA with its renewals resolved;
/// an unchanged list is answered `304`.
/// Verifies: ADR-0065
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
}

/// A bootstrap certificate is admitted, while the window is open, to enrol and to nothing else:
/// the list a Gateway refuses by is not for it.
/// Verifies: ADR-0065
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

// ---- The rate limit on the Agent plane (ADR-0066) ----

/// A clock a test moves by hand, started at the real time so certificates and entries read true.
struct Manual(std::sync::atomic::AtomicU64);

impl fleet_server::fleet::Clock for Manual {
    fn now_ms(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Manual {
    fn now() -> Arc<Self> {
        use fleet_server::fleet::Clock as _;
        Arc::new(Manual(std::sync::atomic::AtomicU64::new(
            fleet_server::clock::SystemClock.now_ms(),
        )))
    }

    fn advance(&self, secs: u64) {
        self.0
            .fetch_add(secs * 1000, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A bucket of `burst` messages that refills one a second.
fn burst(burst: u32) -> fleet_server::agent_rate::Limits {
    fleet_server::agent_rate::Limits {
        messages_per_sec: 1,
        burst,
        gateway_messages_per_sec: 1,
        gateway_burst: burst,
    }
}

/// Whether `reply` is the `Unavailable` of the rate limit or any other.
fn is_unavailable(reply: &ServerToAgent) -> bool {
    reply
        .error_response
        .as_ref()
        .is_some_and(|error| error.r#type == ServerErrorResponseType::Unavailable as i32)
}

/// The host a certificate in PEM names.
fn host_of(pem: &str) -> String {
    let der = opamp::tls::certificates(pem.as_bytes()).expect("pem");
    fleet_server::ca::facts(der[0].as_ref())
        .expect("facts")
        .host
        .expect("a host")
}

/// One report from `client`, answered.
async fn reported(served: &Served, client: &reqwest::Client) -> ServerToAgent {
    decode(post(client, &served.endpoint, report(&InstanceUid::default())).await).await
}

/// Two certificates of one host draw on one bucket, whichever of them a message comes over; a
/// certificate of another host has a bucket of its own.
/// Verifies: ADR-0066
#[tokio::test]
async fn two_certificates_of_one_host_share_a_bucket_and_two_hosts_do_not() {
    let pki = Pki::new();
    let served = serve(
        &pki,
        Setup {
            agent_rate: Some(burst(3)),
            clock: Some(Manual::now()),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (provisioned, provisioned_key) = pki.issue("edge-01");
    let (first, first_key) = issued_through(
        &served,
        &client(&served.ca_pem, Some((&provisioned, &provisioned_key))),
        "edge-01",
    )
    .await;
    let first_client = client(&served.ca_pem, Some((&first, &first_key)));
    // Its renewal names the same host, and takes the host's first token.
    let (second, second_key) = issued_through(&served, &first_client, "edge-01").await;
    assert_eq!(host_of(&first), host_of(&second));
    let second_client = client(&served.ca_pem, Some((&second, &second_key)));

    assert!(!is_unavailable(&reported(&served, &first_client).await));
    assert!(!is_unavailable(&reported(&served, &second_client).await));
    assert!(is_unavailable(&reported(&served, &first_client).await));
    assert!(
        is_unavailable(&reported(&served, &second_client).await),
        "the second certificate drew on the same, empty bucket"
    );

    let (other, other_key) = pki.issue("edge-02");
    let (third, third_key) = issued_through(
        &served,
        &client(&served.ca_pem, Some((&other, &other_key))),
        "edge-02",
    )
    .await;
    assert_ne!(host_of(&third), host_of(&first));
    let third_client = client(&served.ca_pem, Some((&third, &third_key)));
    assert!(!is_unavailable(&reported(&served, &third_client).await));
}

/// One bootstrap certificate presented from two addresses is two buckets: an enrolling host is
/// counted by where it connects from, not by a certificate the whole fleet may share.
/// Verifies: ADR-0066
#[tokio::test]
async fn a_bootstrap_certificate_shared_by_two_addresses_is_two_buckets() {
    let pki = Pki::new();
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let served = serve(
        &pki,
        Setup {
            client_ca: Some(client_ca_of(&pki)),
            bootstrap: Some(&bootstrap),
            agent_rate: Some(burst(1)),
            clock: Some(Manual::now()),
            ..Setup::default()
        },
    )
    .await;
    served
        .enrolment
        .as_ref()
        .expect("enrolment")
        .open(600)
        .expect("open the window");
    let (cert, key) = bootstrap.issue("bootstrap");
    let from = |address: &str| {
        let mut pem = key.as_bytes().to_vec();
        pem.extend_from_slice(cert.as_bytes());
        reqwest::Client::builder()
            .use_rustls_tls()
            .tls_certs_only([reqwest::Certificate::from_pem(served.ca_pem.as_bytes()).expect("ca")])
            .resolve(
                "localhost",
                "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
            )
            .local_address(Some(address.parse().expect("address")))
            .identity(reqwest::Identity::from_pem(&pem).expect("identity"))
            .build()
            .expect("client")
    };
    let (one, two) = (from("127.0.0.1"), from("127.0.0.2"));
    assert!(!is_unavailable(&reported(&served, &one).await));
    assert!(is_unavailable(&reported(&served, &one).await));
    assert!(
        !is_unavailable(&reported(&served, &two).await),
        "another address, another bucket"
    );
}

/// The record's lines, in memory: no disk to wait on.
#[derive(Clone, Default)]
struct Lines(Arc<std::sync::Mutex<Vec<String>>>);

impl fleet_server::audit_log::AuditStore for Lines {
    fn tail(&mut self) -> Result<Option<fleet_server::audit_log::Tail>, String> {
        Ok(None)
    }
    fn append(&mut self, _: u64, line: &str) -> Result<(), String> {
        self.0.lock().expect("lines").push(line.to_string());
        Ok(())
    }
    fn current_bytes(&self) -> u64 {
        0
    }
    fn rotate(&mut self, _: usize) -> Result<Vec<(String, String)>, String> {
        Ok(Vec::new())
    }
}

/// Every throttled message is passed to the record as a refusal naming the host; past ten a second
/// from one address the rest are counted in `agent_rate.throttled.aggregated`. The record runs on
/// the Server's frozen clock, so every refusal falls into one second: ten are written and the
/// other fifteen are counted once the clock has moved past it.
/// Verifies: ADR-0066, ADR-0063
#[tokio::test]
async fn a_throttled_message_leaves_an_aggregated_refusal_naming_the_host() {
    const REFUSED: u64 = 25;
    let pki = Pki::new();
    let lines = Lines::default();
    // Frozen, so the bucket stays empty and every refusal is recorded in the same second.
    let clock = Manual::now();
    let audit: Arc<dyn fleet_server::audit::Audit> = Arc::new(
        fleet_server::audit_log::AuditLog::start(
            Box::new(lines.clone()),
            fleet_server::audit_log::Limits::default(),
            clock.clone(),
        )
        .expect("audit"),
    );
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit),
            agent_rate: Some(burst(1)),
            clock: Some(clock.clone()),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let (provisioned, provisioned_key) = pki.issue("edge-01");
    let (cert, key) = issued_through(
        &served,
        &client(&served.ca_pem, Some((&provisioned, &provisioned_key))),
        "edge-01",
    )
    .await;
    let host = host_of(&cert);
    let member = client(&served.ca_pem, Some((&cert, &key)));
    assert!(!is_unavailable(&reported(&served, &member).await));
    for _ in 0..REFUSED {
        assert!(is_unavailable(&reported(&served, &member).await));
    }
    clock.advance(1);

    // The writer counts a second's excess on its next tick after that second has passed.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let (written, aggregated) = loop {
        let all: Vec<serde_json::Value> = lines
            .0
            .lock()
            .expect("lines")
            .iter()
            .map(|line| serde_json::from_str(line).expect("json"))
            .collect();
        let aggregated: Vec<_> = named(&all, "agent_rate.throttled.aggregated")
            .into_iter()
            .cloned()
            .collect();
        if !aggregated.is_empty() {
            let written: Vec<_> = named(&all, "agent_rate.throttled")
                .into_iter()
                .cloned()
                .collect();
            break (written, aggregated);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no aggregate was written: {all:#?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    assert_eq!(written.len(), 10, "ten a second are written");
    assert_eq!(aggregated.len(), 1, "{aggregated:#?}");
    assert_eq!(aggregated[0]["count"].as_u64(), Some(REFUSED - 10));
    for entry in &written {
        assert_eq!(entry["outcome"], "throttled");
        assert_eq!(entry["host"], host.as_str());
        assert_eq!(entry["bucket"], "host");
        assert_eq!(entry["route"], "opamp");
        assert_eq!(entry["transport"], "http");
        assert_eq!(entry["peer"], "127.0.0.1");
        assert_eq!(entry["instance_uid"].as_str().map(str::len), Some(32));
    }
    for entry in &aggregated {
        assert_eq!(entry["peer"], "127.0.0.1");
    }
}

/// Each `Unavailable` this Server sends carries the `instance_uid` of the message it answers — the
/// field the Client and a Gateway route a reply by: the rate limit's, the Agent-record ceiling's, a
/// CSR held back for its record, the full enrolment queue and the closed enrolment window.
/// Verifies: ADR-0066, ADR-0059, ADR-0063
#[tokio::test]
async fn every_unavailable_reply_names_the_agent_it_answers() {
    let names_its_agent = |reply: &ServerToAgent, uid: &InstanceUid, case: &str| {
        assert!(is_unavailable(reply), "{case}: {reply:?}");
        assert_eq!(reply.instance_uid, uid.as_bytes(), "{case}");
    };
    let pki = Pki::new();

    // The rate limit.
    let served = serve(
        &pki,
        Setup {
            agent_rate: Some(burst(1)),
            clock: Some(Manual::now()),
            ..Setup::default()
        },
    )
    .await;
    let (cert, key) = pki.issue("edge-01");
    let member = client(&served.ca_pem, Some((&cert, &key)));
    reported(&served, &member).await;
    let uid = InstanceUid::default();
    let reply = decode(post(&member, &served.endpoint, report(&uid)).await).await;
    names_its_agent(&reply, &uid, "throttled");

    // The Agent-record ceiling.
    let served = serve(
        &pki,
        Setup {
            max_agents: Some(1),
            ..Setup::default()
        },
    )
    .await;
    let member = client(&served.ca_pem, Some((&cert, &key)));
    reported(&served, &member).await;
    let uid = InstanceUid::default();
    let reply = decode(post(&member, &served.endpoint, report(&uid)).await).await;
    names_its_agent(&reply, &uid, "ceiling");

    // A CSR held back for its audit record. A plain-HTTP member's admission is recorded once an
    // hour, so the member is still admitted after the record fails; its CSR is not signed.
    let failing = Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct Failing(Arc<std::sync::atomic::AtomicBool>);
    impl fleet_server::audit_log::AuditStore for Failing {
        fn tail(&mut self) -> Result<Option<fleet_server::audit_log::Tail>, String> {
            Ok(None)
        }
        fn append(&mut self, _: u64, _: &str) -> Result<(), String> {
            if self.0.load(std::sync::atomic::Ordering::SeqCst) {
                Err("disk full".to_string())
            } else {
                Ok(())
            }
        }
        fn current_bytes(&self) -> u64 {
            0
        }
        fn rotate(&mut self, _: usize) -> Result<Vec<(String, String)>, String> {
            Ok(Vec::new())
        }
    }
    let audit: Arc<dyn fleet_server::audit::Audit> = Arc::new(
        fleet_server::audit_log::AuditLog::start(
            Box::new(Failing(failing.clone())),
            fleet_server::audit_log::Limits::default(),
            Arc::new(fleet_server::clock::SystemClock),
        )
        .expect("audit"),
    );
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit.clone()),
            ..revocations_setup(&pki)
        },
    )
    .await;
    let member = client(&served.ca_pem, Some((&cert, &key)));
    reported(&served, &member).await;
    failing.store(true, std::sync::atomic::Ordering::SeqCst);
    // The record turns unavailable once its writer has failed to append; wait for that, not for
    // a fixed time.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while audit
        .record(fleet_server::audit::Entry::new("probe", "probe"))
        .is_ok()
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the record never turned unavailable"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let uid = InstanceUid::default();
    let (csr, _) = csr_for("edge-01");
    let reply = decode(post(&member, &served.endpoint, with_csr(&uid, csr)).await).await;
    names_its_agent(&reply, &uid, "a CSR held back");

    // The full enrolment queue, and the window that closed while a host was connected.
    let bootstrap = Pki::named("opamp-fleet-test-bootstrap-ca");
    let clock = Manual::now();
    let served = serve(
        &pki,
        Setup {
            client_ca: Some(client_ca_of(&pki)),
            bootstrap: Some(&bootstrap),
            clock: Some(clock.clone()),
            ..Setup::default()
        },
    )
    .await;
    let enrolment = served.enrolment.clone().expect("enrolment");
    enrolment.open(60).expect("open the window");
    for n in 0..fleet_server::enrolment::MAX_PENDING {
        let request = fleet_server::enrolment::Request {
            csr_pem: String::new(),
            subject: format!("CN=filler-{n}"),
            key_fingerprint: format!("{n:064x}"),
            instance_uid: vec![0; 16],
        };
        let requester = fleet_server::enrolment::Requester {
            bootstrap_subject: String::new(),
            bootstrap_fingerprint: String::new(),
            peer: None,
        };
        enrolment.submit(request, requester);
    }
    let (bootstrap_cert, bootstrap_key) = bootstrap.issue("bootstrap");
    let enrolling = client(&served.ca_pem, Some((&bootstrap_cert, &bootstrap_key)));
    let uid = InstanceUid::default();
    let (csr, _) = csr_for("edge-03");
    let reply = decode(post(&enrolling, &served.endpoint, with_csr(&uid, csr)).await).await;
    names_its_agent(&reply, &uid, "the full enrolment queue");

    enrolment.close();
    enrolment.open(60).expect("open the window");
    let mut socket = websocket(&served, &bootstrap_cert, &bootstrap_key, None)
        .await
        .expect("admitted while the window is open");
    clock.advance(61);
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as Ws;
    let uid = InstanceUid::default();
    let (csr, _) = csr_for("edge-04");
    let framed = opamp::frame::encode_within(&with_csr(&uid, csr), usize::MAX).expect("frame");
    socket.send(Ws::Binary(framed.into())).await.expect("send");
    let reply = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("a reply in time")
        {
            Some(Ok(Ws::Binary(bytes))) => {
                break opamp::frame::decode::<ServerToAgent>(&bytes, usize::MAX).expect("decode")
            }
            Some(Ok(_)) => continue,
            other => panic!("the session ended before its reply: {other:?}"),
        }
    };
    names_its_agent(&reply, &uid, "the closed enrolment window");
}

// ---- A host fetches only what is offered to its own Agents (ADR-0068) ----

/// The Agent type the delivery tests release.
const OTELCOL: &str = "otelcol";

/// A Server with package delivery, a host register and the client CA.
fn delivery_setup(pki: &Pki) -> Setup<'static> {
    Setup {
        packages: true,
        ..revocations_setup(pki)
    }
}

/// A host's client: a certificate naming `host`.
fn host_client(served: &Served, pki: &Pki, host: &str) -> reqwest::Client {
    let (cert, key) = pki.issue_to_host(host);
    client(&served.ca_pem, Some((&cert, &key)))
}

/// A report of an `otelcol` Agent on linux/amd64 that accepts packages.
fn package_report(uid: &InstanceUid) -> AgentToServer {
    let attr = opamp::attributes::string_attr;
    AgentToServer {
        agent_description: Some(opamp::proto::AgentDescription {
            identifying_attributes: vec![attr("service.name", OTELCOL)],
            non_identifying_attributes: vec![attr("os.type", "linux"), attr("host.arch", "amd64")],
        }),
        capabilities: AgentCapabilities::ReportsStatus as u64
            | AgentCapabilities::AcceptsPackages as u64,
        ..report(uid)
    }
}

fn linux() -> fleet_server::packages::Platform {
    fleet_server::packages::Platform::new("linux", "amd64").expect("platform")
}

/// Uploads `artifact` as `otelcol@<version>` for linux/amd64 and puts it, signed, into the
/// `stable` channel that claims every `otelcol` — saved, released to nobody.
fn save(served: &Served, version: &str, artifact: &[u8]) {
    let id = fleet_server::packages::PackageId::new(OTELCOL, version).expect("id");
    let store = served.state.packages().expect("package delivery");
    store.create(&id).expect("create");
    store
        .put_entry(&id, &linux(), artifact.to_vec())
        .expect("entry");
    served
        .state
        .deployment_store()
        .expect("deployments")
        .put(
            "stable",
            [("service.name".to_string(), OTELCOL.to_string())].into(),
        )
        .expect("deployment");
    served
        .state
        .put_deployment_package("stable", &id, true)
        .expect("package");
    served
        .state
        .put_deployment_signature("stable", &id, &linux(), vec![1; 64])
        .expect("signature");
}

/// `GET` of the download route for `otelcol@<version>`, with a raw query.
async fn fetch(served: &Served, client: &reqwest::Client, path: &str) -> reqwest::Response {
    client
        .get(served.endpoint.replace("/v1/opamp", path))
        .send()
        .await
        .expect("send")
}

fn artifact_path(version: &str) -> String {
    format!("/api/v1/packages/{OTELCOL}/{version}/file?os=linux&arch=amd64")
}

/// One host's Agent is reported, and `version` is saved and released to it by the press.
async fn released_to(served: &Served, host: &reqwest::Client, version: &str, artifact: &[u8]) {
    post(
        host,
        &served.endpoint,
        package_report(&InstanceUid([1; 16])),
    )
    .await;
    save(served, version, artifact);
    assert_eq!(
        served
            .state
            .rollout_deployment("stable")
            .expect("the press"),
        1
    );
}

/// A host fetches the artifact released to the Agent that reported with its certificate.
/// Verifies: ADR-0068
#[tokio::test]
async fn a_host_fetches_the_artifact_offered_to_its_own_agent() {
    let pki = Pki::new();
    let served = serve(&pki, delivery_setup(&pki)).await;
    let host = host_client(&served, &pki, "h1");
    released_to(&served, &host, "1.0.0", b"the-binary").await;
    let response = fetch(&served, &host, &artifact_path("1.0.0")).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.bytes().await.expect("bytes").as_ref(),
        b"the-binary"
    );
}

/// Another member of the fleet is answered `404` for what was released to one host's Agent alone.
/// Verifies: ADR-0068
#[tokio::test]
async fn a_host_is_answered_404_for_an_artifact_offered_only_to_another_host() {
    let pki = Pki::new();
    let served = serve(&pki, delivery_setup(&pki)).await;
    released_to(
        &served,
        &host_client(&served, &pki, "h1"),
        "1.0.0",
        b"the-binary",
    )
    .await;
    let other = host_client(&served, &pki, "h2");
    post(&other, &served.endpoint, report(&InstanceUid([2; 16]))).await;
    let response = fetch(&served, &other, &artifact_path("1.0.0")).await;
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
    assert_ne!(
        response.bytes().await.expect("bytes").as_ref(),
        b"the-binary"
    );
}

/// Status, headers and body of a response, the `Date` header aside: it tells the time of the
/// answer, never what the store holds.
async fn answer(response: reqwest::Response) -> (u16, Vec<(String, Vec<u8>)>, Vec<u8>) {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| *name != reqwest::header::DATE)
        .map(|(name, value)| (name.to_string(), value.as_bytes().to_vec()))
        .collect();
    (
        status,
        headers,
        response.bytes().await.expect("body").to_vec(),
    )
}

/// The same request is answered byte for byte alike whether the store holds nothing under it,
/// holds only a referenced entry, holds an artifact released to nobody, or holds one released to
/// another host's Agent.
/// Verifies: ADR-0068
#[tokio::test]
async fn an_artifact_not_offered_and_one_not_held_are_answered_alike() {
    let pki = Pki::new();
    let served = serve(&pki, delivery_setup(&pki)).await;
    let other = host_client(&served, &pki, "h2");
    post(&other, &served.endpoint, report(&InstanceUid([2; 16]))).await;

    let not_held = answer(fetch(&served, &other, &artifact_path("1.0.0")).await).await;
    assert_eq!(not_held.0, 404);

    let referenced = fleet_server::packages::PackageId::new(OTELCOL, "1.0.0").expect("id");
    let store = served.state.packages().expect("package delivery");
    store.create(&referenced).expect("create");
    store
        .set_entry_source(
            &referenced,
            &linux(),
            vec![0; 32],
            fleet_server::packages::Source {
                url: "https://example.com/otelcol.tar.gz".to_string(),
                headers: Default::default(),
            },
        )
        .expect("source");
    let only_referenced = answer(fetch(&served, &other, &artifact_path("1.0.0")).await).await;
    assert_eq!(only_referenced, not_held, "a referenced entry");
    store.delete_set(&referenced).expect("delete");

    let host = host_client(&served, &pki, "h1");
    post(
        &host,
        &served.endpoint,
        package_report(&InstanceUid([1; 16])),
    )
    .await;
    save(&served, "1.0.0", b"the-binary");
    let released_to_nobody = answer(fetch(&served, &other, &artifact_path("1.0.0")).await).await;
    assert_eq!(
        released_to_nobody, not_held,
        "an artifact released to nobody"
    );

    served
        .state
        .rollout_deployment("stable")
        .expect("the press");
    let released_to_another = answer(fetch(&served, &other, &artifact_path("1.0.0")).await).await;
    assert_eq!(
        released_to_another, not_held,
        "an artifact of another host's Agent"
    );
    assert_eq!(
        fetch(&served, &host, &artifact_path("1.0.0"))
            .await
            .status(),
        reqwest::StatusCode::OK
    );
}

/// A host marked as a Gateway speaks for any Agent, so it fetches what is offered to any; before
/// it is marked, it fetches nothing of another host's.
/// Verifies: ADR-0068
#[tokio::test]
async fn a_marked_gateway_fetches_what_is_offered_to_any_agent() {
    let pki = Pki::new();
    let served = serve(&pki, delivery_setup(&pki)).await;
    released_to(
        &served,
        &host_client(&served, &pki, "h1"),
        "1.0.0",
        b"the-binary",
    )
    .await;
    let gateway = host_client(&served, &pki, "gw");
    post(&gateway, &served.endpoint, report(&InstanceUid([9; 16]))).await;
    assert_eq!(
        fetch(&served, &gateway, &artifact_path("1.0.0"))
            .await
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert!(served
        .revocations
        .as_ref()
        .expect("register")
        .set_gateway("gw", true)
        .expect("mark"));
    let response = fetch(&served, &gateway, &artifact_path("1.0.0")).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response.bytes().await.expect("bytes").as_ref(),
        b"the-binary"
    );
}

/// A certificate that names no host speaks for no Agent — not even the one it reported, to which
/// the artifact was released.
/// Verifies: ADR-0068
#[tokio::test]
async fn a_certificate_naming_no_host_fetches_nothing() {
    let pki = Pki::new();
    let served = serve(&pki, delivery_setup(&pki)).await;
    let (cert, key) = pki.issue("edge-01");
    let provisioned = client(&served.ca_pem, Some((&cert, &key)));
    released_to(&served, &provisioned, "1.0.0", b"the-binary").await;
    assert_eq!(
        fetch(&served, &provisioned, &artifact_path("1.0.0"))
            .await
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
}

/// A fetch that is not offered leaves one `download.refused` entry naming the check, the host, the
/// certificate's serial and the artifact asked for; it is no failure the admission throttle counts,
/// which here backs off after one.
/// Verifies: ADR-0068, ADR-0063
#[tokio::test]
async fn a_refused_fetch_leaves_one_download_refused_entry_naming_its_check() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit_in(dir.path())),
            throttle: Some(fleet_server::throttle::Limits {
                max_failures: 1,
                window_secs: 60,
                backoff_secs: 300,
            }),
            ..delivery_setup(&pki)
        },
    )
    .await;
    released_to(
        &served,
        &host_client(&served, &pki, "h1"),
        "1.0.0",
        b"the-binary",
    )
    .await;
    let (cert, key) = pki.issue_to_host("h2");
    let other = client(&served.ca_pem, Some((&cert, &key)));
    assert_eq!(
        fetch(&served, &other, &artifact_path("1.0.0"))
            .await
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );

    let all = entries(dir.path()).await;
    let refused = named(&all, "download.refused");
    assert_eq!(refused.len(), 1, "{all:#?}");
    assert_eq!(refused[0]["outcome"], "refused");
    assert_eq!(refused[0]["check"], "not offered");
    assert_eq!(refused[0]["host"], "h2");
    assert_eq!(refused[0]["serial"], cert_id(&cert).serial.as_str());
    assert_eq!(refused[0]["agent_type"], OTELCOL);
    assert_eq!(refused[0]["version"], "1.0.0");
    assert_eq!(refused[0]["platform"], "linux-amd64");

    // Past the throttle's one failure, the same address is still admitted.
    assert_eq!(
        fetch(&served, &other, &artifact_path("1.0.0"))
            .await
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    assert!(
        post(&other, &served.endpoint, report(&InstanceUid([2; 16])))
            .await
            .status()
            .is_success()
    );
}

/// A malformed identity or Platform token is a `400`, decided before any offer is tested: no
/// `download.refused` entry follows it.
/// Verifies: ADR-0068
#[tokio::test]
async fn a_malformed_token_is_answered_400_before_the_offer_is_tested() {
    let pki = Pki::new();
    let dir = tempfile::tempdir().expect("tempdir");
    let served = serve(
        &pki,
        Setup {
            audit: Some(audit_in(dir.path())),
            ..delivery_setup(&pki)
        },
    )
    .await;
    let other = host_client(&served, &pki, "h2");
    for path in [
        "/api/v1/packages/otel@col/1.0.0/file?os=linux&arch=amd64",
        "/api/v1/packages/otelcol/1.0.0/file?os=lin%2Fux&arch=amd64",
        "/api/v1/packages/otelcol/1.0.0/file?os=linux",
    ] {
        assert_eq!(
            fetch(&served, &other, path).await.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{path}"
        );
    }
    let all = entries(dir.path()).await;
    assert!(named(&all, "download.refused").is_empty(), "{all:#?}");
}

/// A download takes a token from the bucket of the host its certificate names — the bucket its
/// Agent's messages draw on.
/// Verifies: ADR-0068, ADR-0066
#[tokio::test]
async fn a_download_costs_a_token_of_the_hosts_bucket() {
    let pki = Pki::new();
    let served = serve(
        &pki,
        Setup {
            agent_rate: Some(burst(2)),
            clock: Some(Manual::now()),
            ..delivery_setup(&pki)
        },
    )
    .await;
    let host = host_client(&served, &pki, "h1");
    released_to(&served, &host, "1.0.0", b"the-binary").await;
    assert_eq!(
        fetch(&served, &host, &artifact_path("1.0.0"))
            .await
            .status(),
        reqwest::StatusCode::OK
    );
    assert!(
        is_unavailable(
            &decode(
                post(
                    &host,
                    &served.endpoint,
                    package_report(&InstanceUid([1; 16]))
                )
                .await
            )
            .await
        ),
        "the download took the host's second token"
    );
    assert_eq!(
        fetch(&served, &host, &artifact_path("1.0.0"))
            .await
            .status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
}
