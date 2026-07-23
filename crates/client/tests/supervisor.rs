//! Integration: a `command` Supervisor brings a Foreign Agent (the stub) under management —
//! the process is spawned from the configured command line, and a Client shutdown stops it
//! first (ADR-0011).

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

fn spawn_client(config_path: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_client"))
        .arg("--config")
        .arg(config_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the client")
}

fn write_config(dir: &Path, marker: &Path) -> std::path::PathBuf {
    // An unreachable endpoint: supervision must not depend on the Server being there.
    let config = format!
        (
        "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\n[[supervisor]]\ntype = \"command\"\nname = \"stub\"\ncommand = {command:?}\nargs = [\"--touch\", {marker:?}]\n",
        state = dir.join("state").to_string_lossy(),
        command = env!("CARGO_BIN_EXE_stub_agent"),
        marker = marker.to_string_lossy(),
    );
    let path = dir.join("client.toml");
    std::fs::write(&path, config).expect("write client.toml");
    path
}

#[test]
fn a_command_supervisor_spawns_the_configured_process() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker");
    let config = write_config(dir.path(), &marker);

    let mut client = spawn_client(&config);
    wait_for("the stub's marker file", Duration::from_secs(20), || {
        marker.exists()
    });
    let content = std::fs::read_to_string(&marker).expect("read the marker");
    assert!(
        content.contains("--touch"),
        "marker carries the argv: {content}"
    );

    client.kill().expect("kill the client");
    let _ = client.wait();
}

#[test]
fn a_collector_supervisor_passes_each_config_entry_as_a_config_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker");

    // A configuration that survived a previous run: the collector starts on it right away.
    let config_dir = dir.path().join("state/supervisors/otelcol/config");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    std::fs::write(config_dir.join("collector.yaml"), "receivers: {}\n").expect("seed config");

    let toml = format!(
        "endpoint = \"ws://127.0.0.1:1/v1/opamp\"\nstate_dir = {state:?}\n\n[[supervisor]]\ntype = \"collector\"\nname = \"otelcol\"\nbinary = {binary:?}\nargs = [\"--touch\", {marker:?}]\n",
        state = dir.path().join("state").to_string_lossy(),
        binary = env!("CARGO_BIN_EXE_stub_agent"),
        marker = marker.to_string_lossy(),
    );
    let config_path = dir.path().join("client.toml");
    std::fs::write(&config_path, toml).expect("write client.toml");

    let mut client = spawn_client(&config_path);
    wait_for(
        "the stub collector's marker",
        Duration::from_secs(20),
        || marker.exists(),
    );
    let content = std::fs::read_to_string(&marker).expect("read the marker");
    assert!(
        content.contains("--config"),
        "argv carries --config: {content}"
    );
    assert!(
        content.contains("collector.yaml"),
        "argv names the entry file: {content}"
    );

    client.kill().expect("kill the client");
    let _ = client.wait();
}

#[cfg(unix)]
#[test]
fn sigterm_stops_the_managed_process_and_the_client_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("marker");
    let config = write_config(dir.path(), &marker);

    let mut client = spawn_client(&config);
    wait_for("the stub's marker file", Duration::from_secs(20), || {
        marker.exists()
    });
    let stub_pid: u32 = std::fs::read_to_string(&marker)
        .expect("read the marker")
        .lines()
        .find_map(|l| l.strip_prefix("pid=").and_then(|p| p.parse().ok()))
        .expect("the marker names the stub's pid");

    let term = Command::new("kill")
        .args(["-TERM", &client.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(term.success());

    wait_for("the client to exit", Duration::from_secs(20), || {
        matches!(client.try_wait(), Ok(Some(_)))
    });
    let status = client.wait().expect("client exit status");
    assert!(status.success(), "clean shutdown, got {status}");

    // The Managed Process went down with it (kill -0 probes for existence).
    wait_for("the stub to be gone", Duration::from_secs(10), || {
        !Command::new("kill")
            .args(["-0", &stub_pid.to_string()])
            .status()
            .expect("probe the stub")
            .success()
    });
}
