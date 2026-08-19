//! End to end (ADR-0011): the real Server in-process, the real Client binary with two
//! Supervisors — a Collector-type on the stub and a command-type Foreign Agent — over one
//! WebSocket connection. A configuration change reaches both Agents, restarts their processes
//! on the written files, and comes back `APPLIED` and in sync. A Configuration typed for the
//! Client itself then changes its Supervisor set at runtime (ADR-0056): an added block starts
//! and appears as a new Agent, unchanged ones ride through untouched, a removed one stops,
//! says goodbye, and its directory is purged (ADR-0059) — and `supervisor.toml` is rewritten around
//! the operator's globals each time.

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
        Command::new(env!("CARGO_BIN_EXE_supervisor"))
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

/// Finds an Agent by the operator's name for it — `service.instance.name`, the `[[supervisor]]`
/// block's `name` (ADR-0033). Deliberately not `service.name`: that is the Agent *type*, and both
/// Supervisors below run the same stub program, so it does not tell them apart.
fn view<'a>(agents: &'a [AgentView], name: &str) -> Option<&'a AgentView> {
    agents.iter().find(|a| a.service_instance_name == name)
}

/// What this Client presents: its two Supervisors, plus itself (ADR-0020).
const AGENTS: usize = 3;

/// The stub binary's own file name — what a **bare** program name resolves to inside a Supervisor's
/// owned `program/` directory. Blocks below name their program bare (not by absolute path), because
/// a Server-delivered Supervisor set may run only a program this Client owns (ADR-0057), and the
/// operator-local blocks use the same shape so the delivered set can restate them verbatim.
fn stub_program_name() -> String {
    Path::new(env!("CARGO_BIN_EXE_stub_agent"))
        .file_name()
        .expect("the stub binary has a file name")
        .to_string_lossy()
        .into_owned()
}

/// Places the stub binary where a bare program name resolves — `<state_dir>/supervisors/<name>/
/// program/<program>` (ADR-0021) — standing in for the package install that would normally put it
/// there. A Supervisor whose owned program is present starts it; one whose program is absent waits
/// for a package, which is not what this test exercises.
fn stage_owned_program(state_dir: &Path, supervisor: &str, program: &str) {
    let program_dir = state_dir
        .join("supervisors")
        .join(supervisor)
        .join("program");
    std::fs::create_dir_all(&program_dir).expect("create the owned program directory");
    let dest = program_dir.join(program);
    std::fs::copy(env!("CARGO_BIN_EXE_stub_agent"), &dest).expect("stage the stub binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .expect("make the staged program executable");
    }
}

#[tokio::test]
async fn a_config_change_reaches_both_supervised_agents_over_one_connection() {
    let (addr, state, dir) = spawn_server().await;
    let state_dir: PathBuf = dir.path().join("client-state");
    let stub_marker = dir.path().join("stub-marker");
    let otelcol_marker = dir.path().join("otelcol-marker");

    let program = stub_program_name();
    let otelcol_block = format!(
        concat!(
            "[[supervisor]]\n",
            "type = \"collector\"\n",
            "name = \"otelcol\"\n",
            "binary = {program:?}\n",
            "args = [\"--touch\", {otelcol_marker:?}]\n",
        ),
        program = program,
        otelcol_marker = otelcol_marker.to_string_lossy(),
    );
    let stub_block = format!(
        concat!(
            "[[supervisor]]\n",
            "type = \"command\"\n",
            "name = \"stub\"\n",
            "command = {program:?}\n",
            "args = [\"--touch\", {stub_marker:?}]\n",
            "version_args = [\"--version\"]\n",
            "[supervisor.attributes]\n",
            "role = \"edge\"\n",
        ),
        program = program,
        stub_marker = stub_marker.to_string_lossy(),
    );
    let toml = format!(
        concat!(
            "endpoint = \"ws://{addr}/v1/opamp\"\n",
            "state_dir = {state:?}\n",
            "heartbeat_interval_secs = 1\n\n",
            "[attributes]\n",
            "env = \"prod\"\n\n",
            "{otelcol_block}\n",
            "{stub_block}",
        ),
        addr = addr,
        state = state_dir.to_string_lossy(),
        otelcol_block = otelcol_block,
        stub_block = stub_block,
    );
    let config_path = dir.path().join("supervisor.toml");
    std::fs::write(&config_path, toml).expect("write supervisor.toml");

    // Both owned Supervisors have their program staged before the Client starts, so they run at
    // once rather than waiting for a package (ADR-0057 makes the delivery path owned-only).
    stage_owned_program(&state_dir, "otelcol", &program);
    stage_owned_program(&state_dir, "stub", &program);

    let _client = spawn_client(&config_path);

    // Both Supervisors appear as their own connected Agents — over the one WebSocket
    // connection this Client maintains (ADR-0003: routed by instance_uid alone) — and so does the
    // Client itself, which since ADR-0020 is an Agent whether or not it supervises anything.
    let agents = wait_until("every agent connected", || {
        let snapshot = state.snapshot();
        (snapshot.len() == AGENTS && snapshot.iter().all(|a| a.connected)).then_some(snapshot)
    })
    .await;
    assert!(view(&agents, "otelcol").is_some());
    assert!(view(&agents, "stub").is_some());
    assert!(
        view(&agents, "Supervisor Agent").is_some(),
        "the Client is its own Agent (ADR-0020)"
    );
    // The two Supervisors run the *same* stub program, so they report the same Agent type — which
    // is what a type is for, and exactly why it cannot double as the name (ADR-0033). They stay
    // apart because the operator's name is its own attribute, out of reach of the fold.
    let otelcol_type = &view(&agents, "otelcol").expect("otelcol view").service_name;
    let stub_type = &view(&agents, "stub").expect("stub view").service_name;
    assert_eq!(
        otelcol_type, stub_type,
        "one program, one type — both blocks name the same stub binary"
    );
    assert!(
        !otelcol_type.is_empty(),
        "a type is reported even though neither block sets `service_name`: \
         the program's file name is the fallback"
    );
    let uids: std::collections::HashSet<_> = agents.iter().map(|a| &a.instance_uid).collect();
    assert_eq!(uids.len(), AGENTS, "each Agent has its own identity");

    // The Foreign Agent runs from the start; the Collector awaits its first configuration.
    let first_stub_pid = wait_until("the stub to run", || stub_pid(&stub_marker)).await;
    assert!(!otelcol_marker.exists(), "no config, no collector");
    let otelcol = view(&agents, "otelcol").expect("otelcol view");
    assert!(!otelcol.healthy);
    assert_eq!(otelcol.health_status, "awaiting configuration");

    // The operator distributes a fleet-wide Configuration — saved, then rolled out, because
    // saving alone distributes nothing (ADR-0061); the act assigns every currently matching
    // Agent and the Server pushes the release over the socket.
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
        .expect("save the fleet configuration");
    state
        .rollout_configuration("fleet")
        .expect("roll out the fleet configuration");

    // Both Supervisors acknowledge APPLIED and are in sync; the processes restarted on the
    // files. The fleet-wide Configuration has an empty Selector, so it reaches the Client's own
    // Agent too — whose configuration is its Supervisor set (ADR-0056), and a YAML body is not
    // one: the Client refuses it loudly rather than pretend it took effect.
    wait_until("the supervised agents in sync, the client refusing", || {
        let snapshot = state.snapshot();
        let supervised = ["otelcol", "stub"].iter().all(|name| {
            view(&snapshot, name).is_some_and(|a| a.in_sync && a.remote_config_status == "APPLIED")
        });
        let refused =
            view(&snapshot, "Supervisor Agent").is_some_and(|a| a.remote_config_status == "FAILED");
        (supervised && refused).then_some(())
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

    // The probed process version arrived for both *supervised* Agents: the collector plugin
    // probes `--version` by itself, the command plugin because the block sets `version_args`. The
    // stub prints its SemVer inside free text ("stub_agent version 9.9.9 (test build)").
    wait_until("both supervised agents report the probed version", || {
        let snapshot = state.snapshot();
        ["otelcol", "stub"]
            .iter()
            .all(|name| view(&snapshot, name).is_some_and(|a| a.service_version == "9.9.9"))
            .then_some(())
    })
    .await;

    // The Client's own Agent reports the Client's version instead — never a Managed Process's,
    // because it has none (ADR-0020 makes it visible; ADR-0009 supplies the version).
    let snapshot = state.snapshot();
    let client_agent = view(&snapshot, "Supervisor Agent").expect("the client's own agent");
    assert_ne!(client_agent.service_version, "9.9.9");
    assert!(
        !client_agent.service_version.is_empty(),
        "the Client reports its own baked version"
    );

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
        .save_configuration(
            "edge-extra",
            server::configs::Revision {
                selector: [("role".to_string(), "edge".to_string())].into(),
                body: "processors: {}\n".to_string(),
                role: String::new(),
                service_name: String::new(),
            },
        )
        .expect("save the targeted configuration");
    state
        .rollout_configuration("edge-extra")
        .expect("roll out the targeted configuration");
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

    // ——— The Server manages the Client's own Supervisor set (ADR-0056) ———

    // The untyped fleet Configuration keeps poisoning the Client's composed map (its body is
    // YAML). Since ADR-0061 a narrower aim no longer withdraws what was already rolled out —
    // the Client keeps its pinned assignment however the type changes — so the recovery is to
    // delete the Configuration, which removes it from every assigned Agent, and roll it out
    // again stated for the type both Supervisors report (ADR-0054).
    let snapshot = state.snapshot();
    let supervised_type = view(&snapshot, "otelcol")
        .expect("otelcol view")
        .service_name
        .clone();
    state
        .delete_configuration("fleet")
        .expect("delete the poisoned configuration");
    state
        .save_configuration(
            "fleet",
            server::configs::Revision {
                selector: Default::default(),
                body: "receivers: {}\n".to_string(),
                role: String::new(),
                service_name: supervised_type,
            },
        )
        .expect("retype the fleet configuration");
    state
        .rollout_configuration("fleet")
        .expect("roll out the retyped configuration");
    // The two Supervisors settle on the retyped map before pids are compared below: the delete
    // and the re-rollout each moved their hash, which restarts their processes.
    wait_until("the supervised agents to settle on the retyped map", || {
        let snapshot = state.snapshot();
        ["otelcol", "stub"]
            .iter()
            .all(|name| {
                view(&snapshot, name).is_some_and(|a| {
                    a.in_sync
                        && a.remote_config_status == "APPLIED"
                        && a.assigned_configurations.iter().any(|c| c == "fleet")
                })
            })
            .then_some(())
    })
    .await;

    // A Configuration typed for the Client itself carries `[[supervisor]]` blocks: the running
    // two, verbatim, plus a third. Unchanged blocks ride through — the stub must keep its pid.
    let added_marker = dir.path().join("added-marker");
    let added_block = format!(
        concat!(
            "[[supervisor]]\n",
            "type = \"command\"\n",
            "name = \"added\"\n",
            "command = {program:?}\n",
            "args = [\"--touch\", {added_marker:?}]\n",
        ),
        program = program,
        added_marker = added_marker.to_string_lossy(),
    );
    // The added Supervisor is owned too (ADR-0057): stage its program before the set is delivered,
    // so the block the Server pushes starts a process instead of waiting for a package.
    stage_owned_program(&state_dir, "added", &program);
    let stub_pid_before = stub_pid(&stub_marker).expect("the stub runs");
    state
        .save_configuration(
            "client-supervisors",
            server::configs::Revision {
                selector: Default::default(),
                body: format!("{otelcol_block}\n{stub_block}\n{added_block}"),
                role: String::new(),
                service_name: "supervisor".to_string(),
            },
        )
        .expect("save the supervisor set");
    state
        .rollout_configuration("client-supervisors")
        .expect("roll out the supervisor set");

    wait_until("the added supervisor to connect, the set applied", || {
        let snapshot = state.snapshot();
        let added = view(&snapshot, "added").is_some_and(|a| a.connected);
        let applied = view(&snapshot, "Supervisor Agent")
            .is_some_and(|a| a.in_sync && a.remote_config_status == "APPLIED");
        (added && applied).then_some(())
    })
    .await;
    let _ = wait_until("the added stub to run", || stub_pid(&added_marker)).await;
    let rewritten = std::fs::read_to_string(&config_path).expect("read back supervisor.toml");
    assert!(rewritten.contains("name = \"added\""), "{rewritten}");
    assert!(
        rewritten.contains("env = \"prod\"") && rewritten.contains("heartbeat_interval_secs = 1"),
        "the operator's globals survive the rewrite: {rewritten}"
    );
    assert_eq!(
        stub_pid(&stub_marker),
        Some(stub_pid_before),
        "an unchanged supervisor rides through the apply untouched"
    );

    // Removing the block stops its Supervisor and retires its Agent: the goodbye arrives, the
    // file no longer names it — and the unchanged neighbours still ride through.
    state
        .save_configuration(
            "client-supervisors",
            server::configs::Revision {
                selector: Default::default(),
                body: format!("{otelcol_block}\n{stub_block}"),
                role: String::new(),
                service_name: "supervisor".to_string(),
            },
        )
        .expect("shrink the supervisor set");
    state
        .rollout_configuration("client-supervisors")
        .expect("roll out the shrunken set");

    wait_until("the added supervisor to say goodbye", || {
        let snapshot = state.snapshot();
        let gone = view(&snapshot, "added").is_some_and(|a| !a.connected);
        let applied = view(&snapshot, "Supervisor Agent")
            .is_some_and(|a| a.in_sync && a.remote_config_status == "APPLIED");
        (gone && applied).then_some(())
    })
    .await;
    let rewritten = std::fs::read_to_string(&config_path).expect("read back supervisor.toml");
    assert!(!rewritten.contains("\"added\""), "{rewritten}");
    assert_eq!(
        stub_pid(&stub_marker),
        Some(stub_pid_before),
        "the unchanged supervisors ride through the removal too"
    );

    // A removed Supervisor is purged (ADR-0059): its whole directory — identity, program, written
    // configuration — goes with it, while the supervisors that stay keep theirs.
    wait_until("the removed supervisor's directory to be purged", || {
        (!state_dir.join("supervisors/added").exists()).then_some(())
    })
    .await;
    assert!(
        state_dir.join("supervisors/stub/instance-uid").is_file(),
        "a supervisor that stays keeps its directory and identity"
    );
}
