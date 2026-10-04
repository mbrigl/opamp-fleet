//! Enrolment and the renewals after it, end to end (ADR-0039 clauses 9, 11, 21 and 22): the real
//! Client binary enrols with a bootstrap certificate over mutual TLS, an operator approves its
//! request, and it renews what it was issued — each step costing the register exactly one
//! certificate.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleet_server::fleet::AppState;
use fleet_server::revocation::Revocations;
use rcgen::{CertificateParams, IsCa, Issuer, KeyPair};

struct ClientUnderTest(Child);

impl Drop for ClientUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A throwaway CA, written into `dir` as `<name>.pem` and `<name>-key.pem`.
struct Ca {
    pem: String,
    key_pem: String,
}

impl Ca {
    fn new(dir: &Path, name: &str) -> Self {
        let key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(Vec::<String>::new()).expect("ca params");
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, name);
        params.is_ca = IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let cert = params.self_signed(&key).expect("ca");
        let ca = Ca {
            pem: cert.pem(),
            key_pem: key.serialize_pem(),
        };
        std::fs::write(dir.join(format!("{name}.pem")), &ca.pem).expect("write");
        std::fs::write(dir.join(format!("{name}-key.pem")), &ca.key_pem).expect("write");
        ca
    }

    /// A leaf certificate and its key, written into `dir` as `<name>.pem` and `<name>-key.pem`.
    fn issue(&self, dir: &Path, name: &str) {
        let issuer =
            Issuer::from_ca_cert_pem(&self.pem, KeyPair::from_pem(&self.key_pem).expect("ca key"))
                .expect("issuer");
        let key = KeyPair::generate().expect("key");
        let cert = CertificateParams::new(vec![name.to_string()])
            .expect("params")
            .signed_by(&key, &issuer)
            .expect("signed");
        std::fs::write(dir.join(format!("{name}.pem")), cert.pem()).expect("write");
        std::fs::write(dir.join(format!("{name}-key.pem")), key.serialize_pem()).expect("write");
    }

    fn authority(&self, role: &str) -> fleet_server::revocation::Authority {
        let der = opamp::tls::certificates(self.pem.as_bytes()).expect("pem");
        let facts = fleet_server::ca::facts(der[0].as_ref()).expect("facts");
        fleet_server::revocation::Authority {
            role: role.to_string(),
            subject: facts.id.issuer,
            name: facts.issuer_name,
        }
    }
}

/// The serial of the certificate the Client holds, if it holds one.
fn held(state_dir: &Path) -> Option<Vec<u8>> {
    let pem = std::fs::read(state_dir.join("client-cert.pem")).ok()?;
    let (_, block) = x509_parser::pem::parse_x509_pem(&pem).ok()?;
    let cert = block.parse_x509().ok()?;
    Some(cert.raw_serial().to_vec())
}

async fn until<T>(what: &str, within: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(found) = probe() {
            return found;
        }
        assert!(Instant::now() < deadline, "{what} did not happen in time");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The newest message the Server has taken from the Agent, once it is an Agent of the fleet.
fn last_message(state: &AppState) -> Option<u64> {
    state.snapshot().first().map(|agent| agent.sequence_num)
}

/// Waits until the Server has taken a message after `after`: one connection's messages are taken
/// in order, so everything `after` caused — a certificate signed on it included — is done.
async fn past(state: &AppState, after: u64) {
    until("a later message", Duration::from_secs(10), || {
        last_message(state).filter(|seq| *seq > after)
    })
    .await;
}

/// A host enrols with a bootstrap certificate and an operator's approval, and then renews, over the
/// WebSocket transport: the approval issues one certificate and the renewal one more.
/// Verifies: ADR-0039, ADR-0049
#[tokio::test]
async fn an_enrolment_and_a_renewal_each_issue_one_certificate_over_websocket() {
    enrol_and_renew("wss").await;
}

/// The same over plain HTTP polling.
/// Verifies: ADR-0039, ADR-0049
#[tokio::test]
async fn an_enrolment_and_a_renewal_each_issue_one_certificate_over_http() {
    enrol_and_renew("https").await;
}

/// Enrols the real Client and lets it renew once, checking after each step that the register holds
/// one certificate per step, and that the one an approval issued is registered under the key
/// fingerprint the request was listed and approved by.
async fn enrol_and_renew(scheme: &str) {
    const LIFE: i64 = 20;
    let dir = tempfile::tempdir().expect("tempdir");
    let pki = dir.path();
    let client_ca = Ca::new(pki, "client-ca");
    let bootstrap_ca = Ca::new(pki, "bootstrap-ca");
    client_ca.issue(pki, "localhost");
    bootstrap_ca.issue(pki, "bootstrap");

    let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
    let enrolment = Arc::new(fleet_server::enrolment::Enrolment::new(clock.clone()));
    let revocations = Arc::new(
        Revocations::open(
            Box::new(
                fleet_server::fs::FsLedgerStore::open(pki.join("revocation")).expect("ledger"),
            ),
            clock,
            Arc::new(|_: &str| false),
            vec![
                client_ca.authority("client"),
                bootstrap_ca.authority("bootstrap"),
            ],
        )
        .expect("revocations"),
    );
    let signer = fleet_server::ca::ClientCa::from_config(
        &toml::from_str::<fleet_server::config::ClientCaConfig>(&format!(
            "cert_file = {:?}\nkey_file = {:?}\n",
            pki.join("client-ca.pem").display().to_string(),
            pki.join("client-ca-key.pem").display().to_string(),
        ))
        .expect("client_ca config"),
    )
    .expect("client ca")
    .with_validity(time::Duration::seconds(LIFE));
    let state = Arc::new(
        AppState::new(pki.join("fleet-configs"))
            .expect("state")
            .with_client_ca(Some(signer))
            .with_enrolment(Some(enrolment.clone()))
            .with_revocations(Some(revocations.clone())),
    );

    let tls = toml::from_str::<fleet_server::config::TlsConfig>(&format!(
        "cert_file = {:?}\nkey_file = {:?}\nclient_ca_file = {:?}\n",
        pki.join("localhost.pem").display().to_string(),
        pki.join("localhost-key.pem").display().to_string(),
        pki.join("client-ca.pem").display().to_string(),
    ))
    .expect("tls config");
    let planes = fleet_server::tls::server_tls(
        &tls,
        Some(&fleet_server::config::EnrolmentConfig {
            bootstrap_ca_file: pki.join("bootstrap-ca.pem"),
        }),
    )
    .expect("server material");
    let auth = fleet_server::transport::OpampAuth::from_config(
        &toml::from_str::<fleet_server::config::AuthConfig>(&format!(
            "bearer_tokens = [{:?}]",
            fleet_server::credentials::bearer_entry("test-fleet-token")
        ))
        .expect("auth config"),
    )
    .expect("auth");
    let admission = fleet_server::transport::Admission::new(Some(auth), true)
        .with_enrolment(planes.issuers, Some(enrolment.clone()))
        .with_revocations(Some(revocations.clone()));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(
        fleet_server::listen::plane(
            listener,
            Some(planes.agent.rustls_config().expect("agent plane config")),
            64,
            opamp::server::listen::Handle::new(),
        )
        .serve(fleet_server::agent_app(state.clone(), admission)),
    );
    enrolment.open(600).expect("open the window");

    let state_dir = pki.join("client-state");
    let config_path = pki.join("supervisor.toml");
    std::fs::write(
        &config_path,
        format!(
            "endpoint = \"{scheme}://localhost:{port}/v1/opamp\"\nstate_dir = {:?}\n\
             heartbeat_interval_secs = 1\npoll_interval_secs = 1\n\n[auth]\nbearer_token = \"test-fleet-token\"\n\n\
             [tls]\nca_file = {:?}\ncert_file = {:?}\nkey_file = {:?}\n",
            state_dir.display().to_string(),
            pki.join("client-ca.pem").display().to_string(),
            pki.join("bootstrap.pem").display().to_string(),
            pki.join("bootstrap-key.pem").display().to_string(),
        ),
    )
    .expect("write");
    let _client = ClientUnderTest(
        Command::new(env!("CARGO_BIN_EXE_supervisor"))
            .arg("--config")
            .arg(&config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the client"),
    );

    let id = until("an enrolment request", Duration::from_secs(20), || {
        enrolment
            .pending()
            .first()
            .map(|pending| pending.id.clone())
    })
    .await;
    state.approve_enrolment(&id).expect("approve");
    let first = until("the issued certificate", Duration::from_secs(10), || {
        held(&state_dir)
    })
    .await;
    // An enrolling host is no Agent of the fleet, so the first message the Server keeps is the
    // first on a member connection: the one a request answered already would ride.
    let first_member = until("a member connection", Duration::from_secs(10), || {
        last_message(&state)
    })
    .await;
    past(&state, first_member).await;
    let issued = revocations.issued();
    assert_eq!(issued.len(), 1, "an approval issued {}", issued.len());
    assert_eq!(
        issued[0].facts.key_fingerprint, id,
        "the register knows the key by another fingerprint than the approved request"
    );

    let second = until("a renewal", Duration::from_secs(LIFE as u64), || {
        held(&state_dir).filter(|serial| *serial != first)
    })
    .await;
    // The renewed certificate is stored before the Client reconnects with it, so the message after
    // this one is the first on the new connection, and the one after that proves it was taken.
    let stored = last_message(&state).expect("an Agent");
    past(&state, stored + 1).await;
    let issued = revocations.issued();
    assert_eq!(
        issued.len(),
        2,
        "enrolment and one renewal issued {}",
        issued.len()
    );
    assert_eq!(
        held(&state_dir),
        Some(second),
        "the Client stays on its renewal"
    );
}
