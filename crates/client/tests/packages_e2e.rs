//! Package delivery end to end (ADR-0015, ADR-0017): the real Server armed with a package store,
//! the real Client binary running a `command` Supervisor that consents to package updates — which
//! artifact it gets is the Server's choice, not this configuration's. The Client downloads the
//! offered artifact, verifies its content hash and Ed25519 signature, swaps it over the managed
//! binary, health-gates the restart, and reports `Installed` — visible in the fleet view.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ring::signature::{Ed25519KeyPair, KeyPair};
use server::fleet::{AgentView, AppState, PackageOffering};
use server::packages::{PackageStore, Platform};

/// The Platform this test's Client will report about itself (ADR-0031) — an artifact stored for
/// any other one would not fit it, and would rightly never be offered. `std::env::consts` is the
/// same source the Client reports from, and the store canonicalises both the same way.
fn this_host() -> Platform {
    Platform::new(std::env::consts::OS, std::env::consts::ARCH).expect("this host has a platform")
}

struct ClientUnderTest(Child);

impl Drop for ClientUnderTest {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(value) = probe() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn spawn_server(
    store: PackageStore,
) -> (std::net::SocketAddr, Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("configs")
            .with_packages(Some(PackageOffering::new(store, String::new()))),
    );
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

fn spawn_client(config_path: &Path) -> ClientUnderTest {
    ClientUnderTest(
        Command::new(env!("CARGO_BIN_EXE_opamp-fleet-client"))
            .arg("--config")
            .arg(config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the client"),
    )
}

/// Finds an Agent by the operator's name for it — `service.instance.name` (ADR-0033), which is the
/// `[[supervisor]]` block's `name`. The block below is deliberately named something other than its
/// program, so looking up by `service.name` would find nothing: that attribute is the Agent type,
/// and with no `service_name` set it falls back to the program's file name.
fn view<'a>(agents: &'a [AgentView], name: &str) -> Option<&'a AgentView> {
    agents.iter().find(|a| a.service_instance_name == name)
}

#[tokio::test]
async fn a_signed_package_is_downloaded_verified_swapped_and_reported_installed() {
    // The artifact is the stub binary itself — a real executable that stays up when swapped in.
    let artifact = std::fs::read(env!("CARGO_BIN_EXE_stub_agent")).expect("read stub");
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
    let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair");
    let public_key_hex = hex::encode(keypair.public_key().as_ref());
    let signature = keypair.sign(&artifact).as_ref().to_vec();

    let store_dir = tempfile::tempdir().expect("store dir");
    let store = PackageStore::open(store_dir.path().to_path_buf()).expect("store");
    // The Set's identity states the Agent type it is built for (ADR-0052, ADR-0034): the
    // Supervisor below names its program `managed-agent` and sets no `service_name`, so that file
    // name is the type it reports.
    let set = server::packages::SetId::new("myagent", "managed-agent", "2.0.0").expect("set id");
    store
        .create_or_update(&set, Default::default(), false)
        .expect("create set");
    store
        .put_entry(&set, &this_host(), Some(signature), artifact.clone())
        .expect("put entry");
    // And released (ADR-0043): saving stages the Set, so without this it is a draft and reaches
    // nobody — which is the decision, not an accident of the test.
    store.set_published(&set, true).expect("publish");

    let (addr, state, dir) = spawn_server(store).await;

    // The managed binary starts as a copy of the stub, in the Supervisor's own `program/`
    // directory — which is what a bare `command` names, and what consents to the update
    // (ADR-0021). The package swap replaces it there.
    let state_dir = dir.path().join("client-state");
    let program_dir = state_dir.join("supervisors/myagent/program");
    std::fs::create_dir_all(&program_dir).expect("create the program dir");
    let managed = program_dir.join("managed-agent");
    std::fs::copy(env!("CARGO_BIN_EXE_stub_agent"), &managed).expect("copy stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&managed, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let marker = dir.path().join("marker");
    let toml = format!(
        concat!(
            "endpoint = \"ws://{addr}/v1/opamp\"\n",
            "state_dir = {state:?}\n",
            "heartbeat_interval_secs = 1\n\n",
            "[packages]\n",
            "verification_key = \"{key}\"\n\n",
            "[[supervisor]]\n",
            "type = \"command\"\n",
            "name = \"myagent\"\n",
            "apply_grace_secs = 1\n",
            "command = \"managed-agent\"\n",
            "args = [\"--touch\", {marker:?}]\n",
        ),
        addr = addr,
        state = state_dir.to_string_lossy(),
        key = public_key_hex,
        marker = marker.to_string_lossy(),
    );
    let config_path = dir.path().join("client.toml");
    std::fs::write(&config_path, toml).expect("write client.toml");

    let _client = spawn_client(&config_path);

    // The Agent connects, is offered the package, downloads and verifies it, swaps the binary,
    // and reports Installed at the offered version.
    wait_until("the package to be reported Installed", || {
        let snapshot = state.snapshot();
        let agent = view(&snapshot, "myagent")?;
        let package = agent.packages.iter().find(|p| p.name == "myagent")?;
        (package.status == "Installed" && package.version == "2.0.0").then_some(())
    })
    .await;

    // Name and type are the two things this block states separately, and they differ here: the
    // operator called the Supervisor `myagent`, its program is `managed-agent`, and with no
    // `service_name` set the program's file name is what the Agent reports as its type (ADR-0033).
    let agent = view(&state.snapshot(), "myagent")
        .expect("the agent is found by the operator's name for it")
        .service_name
        .clone();
    assert_eq!(agent, "managed-agent");

    // The managed process ran the swapped-in binary (the marker exists) and the persisted record
    // survives — a restart is not re-offered the same package.
    assert!(marker.exists(), "the swapped binary ran");
    assert!(state_dir
        .join("supervisors/myagent/installed-package.json")
        .exists());
}
