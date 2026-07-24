//! End to end (ADR-0010): the real Server in-process, the real Client binary with two
//! Supervisors — a Collector-type on the stub and a command-type Foreign Agent — over one
//! WebSocket connection. A configuration change reaches both Agents, restarts their processes

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
    let app = server::agent_app(state.clone(), server::transport::Admission::open());
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
}

#[tokio::test]
async fn a_config_change_reaches_both_supervised_agents_over_one_connection() {
    let (addr, state, dir) = spawn_server().await;
    let state_dir: PathBuf = dir.path().join("client-state");
    let stub_marker = dir.path().join("stub-marker");
    let otelcol_marker = dir.path().join("otelcol-marker");

        concat!(
            "[[supervisor]]\n",
            "type = \"collector\"\n",
            "name = \"otelcol\"\n",
        otelcol_marker = otelcol_marker.to_string_lossy(),
            "[[supervisor]]\n",
            "type = \"command\"\n",
            "name = \"stub\"\n",
            "args = [\"--touch\", {stub_marker:?}]\n",
        ),
        stub_marker = stub_marker.to_string_lossy(),
    );
    let toml = format!(
        concat!(
            "endpoint = \"ws://{addr}/v1/opamp\"\n",
            "state_dir = {state:?}\n",
            "heartbeat_interval_secs = 1\n\n",
        ),
        addr = addr,
        state = state_dir.to_string_lossy(),

    let _client = spawn_client(&config_path);

    // Both Supervisors appear as their own connected Agents — over the one WebSocket
        let snapshot = state.snapshot();
    })
    .await;
    assert!(view(&agents, "otelcol").is_some());
    assert!(view(&agents, "stub").is_some());

    // The Foreign Agent runs from the start; the Collector awaits its first configuration.
    let first_stub_pid = wait_until("the stub to run", || stub_pid(&stub_marker)).await;
    assert!(!otelcol_marker.exists(), "no config, no collector");
    let otelcol = view(&agents, "otelcol").expect("otelcol view");
    assert!(!otelcol.healthy);
    assert_eq!(otelcol.health_status, "awaiting configuration");

    state

        let snapshot = state.snapshot();
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

    let collector_argv = std::fs::read_to_string(&otelcol_marker).expect("collector marker");
    assert!(collector_argv.contains("--config"));
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
        let snapshot = state.snapshot();
    // The Client-wide attributes arrived — they describe the *host*, so both Agents carry them
    // (ADR-0011).
    let otelcol = view(&agents, "otelcol").expect("otelcol view");
    for agent in [stub, otelcol] {
        assert_eq!(
            agent
                .non_identifying_attributes
                .get("env")
                .map(String::as_str),
            Some("prod"),
            "the host's own tag reaches every Agent on it"
        );
        assert!(
            !agent.non_identifying_attributes.contains_key("role"),
            "no block tags one Agent any more (ADR-0010)"
        );
    }

    // Tagging *one* Agent among several is the fleet's job now: one label, keyed by the Agent's
    // uid, matched by the same Selectors — and it takes effect without touching the host's file
    // (ADR-0013, ADR-0010).
    let uid = opamp::uid::InstanceUid::parse(&stub.instance_uid).expect("the uid the Server holds");
    assert!(state
        .set_labels(&uid, [("role".to_string(), "edge".to_string())].into())
        .is_ok());
    let agents = state.snapshot();
        view(&agents, "stub")
            .expect("stub view")
            .labels
    assert!(view(&agents, "otelcol")
        .expect("otelcol view")
        .labels
        .is_empty());

    // …and a Selector aimed at it reaches that Agent and no other — which is the whole claim the
    // retired block table used to carry.
        let snapshot = state.snapshot();
        let snapshot = state.snapshot();

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
    state
        let snapshot = state.snapshot();
            "[[supervisor]]\n",
            "type = \"command\"\n",
    state
        let snapshot = state.snapshot();
    state
        let snapshot = state.snapshot();
}
