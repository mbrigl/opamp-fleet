//! End to end (ADR-0011): the real Server in-process, the real Client binary with two
//! Supervisors — a Collector-type on the stub and a command-type Foreign Agent — over one
//! WebSocket connection. A configuration change reaches both Agents, restarts their processes
//! on the written files, and comes back `APPLIED` and in sync.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use server::fleet::{AgentView, AppState};

/// Kills the client on drop so a failing assertion never leaks the process.
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

async fn spawn_server() -> (std::net::SocketAddr, Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs")).expect("open the configuration store"),
    );
    let app = server::app(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the server");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (addr, state, dir)
}

fn spawn_client(config_path: &Path) -> ClientUnderTest {
    ClientUnderTest(
        Command::new(env!("CARGO_BIN_EXE_client"))
            .arg("--config")
            .arg(config_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the client"),
    )
}

fn stub_pid(marker: &Path) -> Option<u32> {
    std::fs::read_to_string(marker)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("pid=").and_then(|p| p.parse().ok()))
}

fn view<'a>(agents: &'a [AgentView], name: &str) -> Option<&'a AgentView> {
    agents.iter().find(|a| a.service_name == name)
}

#[tokio::test]
async fn a_config_change_reaches_both_supervised_agents_over_one_connection() {
    let (addr, state, dir) = spawn_server().await;
    let state_dir: PathBuf = dir.path().join("client-state");
    let stub_marker = dir.path().join("stub-marker");
    let otelcol_marker = dir.path().join("otelcol-marker");

    let toml = format!(
        concat!(
            "endpoint = \"ws://{addr}/v1/opamp\"\n",
            "state_dir = {state:?}\n",
            "heartbeat_interval_secs = 1\n\n",
            "[attributes]\n",
            "env = \"prod\"\n\n",
            "[[supervisor]]\n",
            "type = \"collector\"\n",
            "name = \"otelcol\"\n",
            "binary = {stub:?}\n",
            "args = [\"--touch\", {otelcol_marker:?}]\n\n",
            "[[supervisor]]\n",
            "type = \"command\"\n",
            "name = \"stub\"\n",
            "command = {stub:?}\n",
            "args = [\"--touch\", {stub_marker:?}]\n",
            "version_args = [\"--version\"]\n",
            "[supervisor.attributes]\n",
            "role = \"edge\"\n",
        ),
        addr = addr,
        state = state_dir.to_string_lossy(),
        stub = env!("CARGO_BIN_EXE_stub_agent"),
        otelcol_marker = otelcol_marker.to_string_lossy(),
        stub_marker = stub_marker.to_string_lossy(),
    );
    let config_path = dir.path().join("client.toml");
    std::fs::write(&config_path, toml).expect("write client.toml");

    let _client = spawn_client(&config_path);

    // Both Supervisors appear as their own connected Agents — over the one WebSocket
    // connection this Client maintains (ADR-0003: routed by instance_uid alone).
    let agents = wait_until("both agents connected", || {
        let snapshot = state.snapshot();
        (snapshot.len() == 2 && snapshot.iter().all(|a| a.connected)).then_some(snapshot)
    })
    .await;
    assert!(view(&agents, "otelcol").is_some());
    assert!(view(&agents, "stub").is_some());
    assert_ne!(agents[0].instance_uid, agents[1].instance_uid);

    // The Foreign Agent runs from the start; the Collector awaits its first configuration.
    let first_stub_pid = wait_until("the stub to run", || stub_pid(&stub_marker)).await;
    assert!(!otelcol_marker.exists(), "no config, no collector");
    let otelcol = view(&agents, "otelcol").expect("otelcol view");
    assert!(!otelcol.healthy);
    assert_eq!(otelcol.health_status, "awaiting configuration");

    // The operator distributes a fleet-wide Configuration; the Server pushes it over the socket.
    state
        .put_configuration(server::configs::Configuration {
            name: "fleet".to_string(),
            selector: Default::default(),
            body: "receivers: {}\n".to_string(),
        })
        .expect("distribute the fleet configuration");

    // Both Agents acknowledge APPLIED and are in sync; the processes restarted on the files.
    wait_until("both agents in sync", || {
        let snapshot = state.snapshot();
        (snapshot.len() == 2
            && snapshot
                .iter()
                .all(|a| a.in_sync && a.remote_config_status == "APPLIED"))
        .then_some(())
    })
    .await;
    let collector_pid = wait_until("the collector to start on the new config", || {
        stub_pid(&otelcol_marker)
    })
    .await;
    assert!(collector_pid > 0);
    let restarted_stub_pid = wait_until("the stub to restart", || {
        stub_pid(&stub_marker).filter(|pid| *pid != first_stub_pid)
    })
    .await;
    assert_ne!(restarted_stub_pid, first_stub_pid);

    // The written entry files carry the Configuration's name (ADR-0012) and are what the
    // processes were pointed at.
    let collector_argv = std::fs::read_to_string(&otelcol_marker).expect("collector marker");
    assert!(collector_argv.contains("--config"));
    let stub_config = state_dir.join("supervisors/stub/config/fleet");
    assert_eq!(
        std::fs::read_to_string(stub_config).expect("the stub's written config"),
        "receivers: {}\n"
    );

    // Both Agents report healthy now.
    wait_until("both agents healthy", || {
        let snapshot = state.snapshot();
        snapshot.iter().all(|a| a.healthy).then_some(())
    })
    .await;

    // The probed process version arrived for both: the collector plugin probes `--version` by
    // itself, the command plugin because the block sets `version_args`. The stub prints its
    // SemVer inside free text ("stub_agent version 9.9.9 (test build)").
    wait_until("both agents report the probed version", || {
        let snapshot = state.snapshot();
        snapshot
            .iter()
            .all(|a| a.service_version == "9.9.9")
            .then_some(())
    })
    .await;

    // The operator-defined attributes arrived and Selectors act on them: a Configuration
    // targeting `role = edge` matches only the stub Supervisor (ADR-0012).
    let agents = state.snapshot();
    let stub = view(&agents, "stub").expect("stub view");
    assert_eq!(
        stub.non_identifying_attributes
            .get("env")
            .map(String::as_str),
        Some("prod")
    );
    assert_eq!(
        stub.non_identifying_attributes
            .get("role")
            .map(String::as_str),
        Some("edge")
    );
    let otelcol = view(&agents, "otelcol").expect("otelcol view");
    assert_eq!(
        otelcol
            .non_identifying_attributes
            .get("env")
            .map(String::as_str),
        Some("prod")
    );
    assert!(!otelcol.non_identifying_attributes.contains_key("role"));

    state
        .put_configuration(server::configs::Configuration {
            name: "edge-extra".to_string(),
            selector: [("role".to_string(), "edge".to_string())].into(),
            body: "processors: {}\n".to_string(),
        })
        .expect("distribute the targeted configuration");
    wait_until("the stub to apply both entries", || {
        let snapshot = state.snapshot();
        let stub = view(&snapshot, "stub")?;
        (stub.in_sync
            && stub.matched_configurations == ["edge-extra", "fleet"]
            && stub.remote_config_status == "APPLIED")
            .then_some(())
    })
    .await;
    wait_until("the collector to stay on the fleet configuration", || {
        let snapshot = state.snapshot();
        let otelcol = view(&snapshot, "otelcol")?;
        (otelcol.in_sync && otelcol.matched_configurations == ["fleet"]).then_some(())
    })
    .await;
    let stub_extra = state_dir.join("supervisors/stub/config/edge-extra");
    assert_eq!(
        std::fs::read_to_string(stub_extra).expect("the stub's second entry file"),
        "processors: {}\n"
    );

    // Heartbeats (ReportsHeartbeat, 1 s in this test): with nothing left to change, every
    // Agent's sequence number keeps advancing and the description survives — routine reports,
    // not ReportFullState churn.
    let quiesced: Vec<(String, u64)> = state
        .snapshot()
        .iter()
        .map(|a| (a.instance_uid.clone(), a.sequence_num))
        .collect();
    assert!(state
        .snapshot()
        .iter()
        .all(|a| a.capabilities.iter().any(|c| c == "ReportsHeartbeat")));
    wait_until(
        "heartbeats to advance every agent's sequence number",
        || {
            let snapshot = state.snapshot();
            quiesced
                .iter()
                .all(|(uid, seq)| {
                    snapshot.iter().any(|a| {
                        &a.instance_uid == uid
                            && a.sequence_num > *seq
                            && !a.service_name.is_empty()
                    })
                })
                .then_some(())
        },
    )
    .await;
}
