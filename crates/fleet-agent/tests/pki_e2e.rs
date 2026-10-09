//! The certificates `server pki init` makes are all a fleet needs (ADR-0029): a Server configured
//! from `server.toml.fragment` admits the real Client configured from `supervisor.toml.fragment`
//! once its enrolment is approved, with no certificate made by anything else.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleet_server::fleet::AppState;
use fleet_server::revocation::Revocations;

struct ClientUnderTest(Child);

impl Drop for ClientUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The sections of `server.toml.fragment`, read as the Server reads them.
#[derive(serde::Deserialize)]
struct Fragment {
    tls: fleet_server::config::TlsConfig,
    client_ca: fleet_server::config::ClientCaConfig,
    enrolment: fleet_server::config::EnrolmentConfig,
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

fn authority(file: &Path, role: &str) -> fleet_server::revocation::Authority {
    let pem = std::fs::read(file).expect("read the CA");
    let der = opamp::tls::certificates(&pem).expect("pem");
    let facts = fleet_server::ca::facts(der[0].as_ref()).expect("facts");
    fleet_server::revocation::Authority {
        role: role.to_string(),
        subject: facts.id.issuer,
        name: facts.issuer_name,
    }
}

// Verifies: ADR-0029
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fleet_made_by_pki_init_admits_an_enrolled_host() {
    opamp::tls::install_ring_provider();
    let dir = tempfile::tempdir().expect("tempdir");
    let server_dir = dir.path().join("server");
    let offline_dir = dir.path().join("offline");
    fleet_server::pki::init(&fleet_server::pki::InitOptions {
        server_dir: server_dir.clone(),
        offline_dir: offline_dir.clone(),
        server_path: None,
        host_path: None,
        fleet: fleet_server::pki::DEFAULT_FLEET.to_string(),
        names: vec!["localhost".to_string()],
        ca_days: fleet_server::pki::CA_DAYS,
        server_days: fleet_server::pki::SERVER_DAYS,
        bootstrap_days: fleet_server::pki::BOOTSTRAP_DAYS,
    })
    .expect("pki init");

    // The Server, built from the fragment as `main` builds it from `server.toml`.
    let fragment: Fragment = toml::from_str(
        &std::fs::read_to_string(server_dir.join("server.toml.fragment")).expect("fragment"),
    )
    .expect("the server fragment parses as server.toml sections");
    let clock: Arc<dyn fleet_server::fleet::Clock> = Arc::new(fleet_server::clock::SystemClock);
    let enrolment = Arc::new(fleet_server::enrolment::Enrolment::new(clock.clone()));
    let revocations = Arc::new(
        Revocations::open(
            Box::new(
                fleet_server::fs::FsLedgerStore::open(dir.path().join("revocation"))
                    .expect("ledger"),
            ),
            clock,
            vec![
                authority(&fragment.client_ca.cert_file, "client"),
                authority(&fragment.enrolment.bootstrap_ca_file, "bootstrap"),
            ],
        )
        .expect("revocations"),
    );
    let signer = fleet_server::ca::ClientCa::from_config(&fragment.client_ca)
        .expect("the client CA pki init made loads");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("state")
            .with_client_ca(Some(signer))
            .with_enrolment(Some(enrolment.clone()))
            .with_revocations(Some(revocations.clone())),
    );
    let planes = fleet_server::tls::server_tls(&fragment.tls, Some(&fragment.enrolment))
        .expect("the Server accepts the material pki init made");
    let admission = fleet_server::transport::Admission::new(true)
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

    // The real Client, on the supervisor fragment as written.
    let state_dir = dir.path().join("client-state");
    let config_path = dir.path().join("supervisor.toml");
    std::fs::write(
        &config_path,
        format!(
            "endpoint = \"wss://localhost:{port}/v1/opamp\"\nstate_dir = {:?}\n\
             heartbeat_interval_secs = 1\n{}",
            state_dir.display().to_string(),
            std::fs::read_to_string(offline_dir.join("supervisor.toml.fragment"))
                .expect("fragment"),
        ),
    )
    .expect("write supervisor.toml");
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
    until(
        "the host as a member of the fleet",
        Duration::from_secs(20),
        || (!state.snapshot().is_empty()).then_some(()),
    )
    .await;
    assert!(
        state_dir.join("client-cert.pem").exists(),
        "the host stores the certificate it was issued"
    );
    assert_eq!(
        revocations.issued().len(),
        1,
        "one approval, one certificate"
    );
}
