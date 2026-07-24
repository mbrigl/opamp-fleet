use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ring::signature::{Ed25519KeyPair, KeyPair};
use server::fleet::{AgentView, AppState, PackageOffering};
use server::packages::{PackageStore, Platform};

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

fn this_host() -> Platform {
    Platform::new(std::env::consts::OS, std::env::consts::ARCH).expect("this host has a platform")
}
fn spawn_client(config_path: &Path) -> ClientUnderTest {
    ClientUnderTest(
            .arg("--config")
            .arg(config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    let app = server::agent_app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
    let state_dir: PathBuf = dir.path().join("client-state");
            "[[supervisor]]\n",
            "type = \"collector\"\n",
            "name = \"otelcol\"\n",
            "args = [\"--touch\", {marker:?}]\n",
        ),
        addr = addr,
        state = state_dir.to_string_lossy(),
    state
        .save_configuration(
            "fleet",
            server::configs::Revision {
                selector: Default::default(),
                body: "receivers: {}\n".to_string(),
                role: String::new(),
                service_name: String::new(),
            },
        )
    let artifact = std::fs::read(env!("CARGO_BIN_EXE_stub_agent")).expect("read stub");
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).expect("keygen");
    let keypair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("keypair");
    let public_key_hex = hex::encode(keypair.public_key().as_ref());
    let signature = keypair.sign(&artifact).as_ref().to_vec();

    let store_dir = tempfile::tempdir().expect("store dir");
    let store = PackageStore::open(store_dir.path().to_path_buf()).expect("store");
    let app = server::agent_app(state.clone(), server::transport::Admission::open());
    let state_dir = dir.path().join("client-state");
    std::fs::copy(env!("CARGO_BIN_EXE_stub_agent"), &managed).expect("copy stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&managed, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let marker = dir.path().join("marker");
    let toml = format!(
        concat!(
            "heartbeat_interval_secs = 1\n\n",
            "[packages]\n",
            "verification_key = \"{key}\"\n\n",
            "[[supervisor]]\n",
            "type = \"command\"\n",
            "name = \"myagent\"\n",
            "apply_grace_secs = 1\n",
            "args = [\"--touch\", {marker:?}]\n",
        ),
        addr = addr,
        state = state_dir.to_string_lossy(),
        key = public_key_hex,
        marker = marker.to_string_lossy(),
    );
    wait_until("the package to be reported Installed", || {
        let snapshot = state.snapshot();
        let agent = view(&snapshot, "myagent")?;
        (package.status == "Installed" && package.version == "2.0.0").then_some(())
    })
    .await;
