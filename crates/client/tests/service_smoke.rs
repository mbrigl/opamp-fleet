//! The Client under the machine's **real** service manager — systemd, launchd, or the SCM
//! (ADR-0010, ADR-0020).
//!
//! Every other test in this project stands in for the service manager: the self-update end-to-end
//! test restarts the Client itself, "exactly as systemd would". That is what makes the update loop
//! observable, and it is also the one thing it cannot assert — whether the manager *would*. On
//! Windows that gap is the whole contract: `service-manager`'s `sc.exe` backend silently discards a
//! restart policy, so the Client registers the recovery actions itself and reports
//! `ServiceSpecific(10)` on exit so the SCM reads a failure rather than a clean stop. Nothing has
//! ever proved that this brings a Client back.
//!
//! **Ignored by default, and not part of any ordinary run.** It registers a system service, starts
//! it, and kills it — it needs root or an Administrator, and it changes the machine. The
//! `service-smoke` workflow runs it with `--ignored` on an ephemeral runner; locally, run it only
//! on a host you are willing to have a service installed on:
//!
//! ```console
//! sudo -E cargo test -p client --test service_smoke -- --ignored --nocapture
//! ```
//!
//! What it does **not** cover: starting at boot (a runner never reboots), long-running behaviour,
//! and hosts with SELinux or AppArmor in the way. The manual checklist in `README.md` keeps those.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use client::cli::{parse_instance_name, InstanceName};
use client::service::manager::{service_name, NativeService, RESTART_DELAY_SECS};
use client::service::{ServiceControl, ServiceLevel, ServiceState};
use server::fleet::{AgentView, AppState};

/// The instance this test installs. Its own name, so a leftover from a failed run is recognisable
/// and never collides with an operator's `default`.
const INSTANCE: &str = "ci-smoke";

/// The operator's name for this Agent — written as `name` in the `client.toml` below and reported
/// as `service.instance.name` (ADR-0033), which is what the fleet view is searched by. Deliberately
/// not `service.name`: that carries the Agent *type*, which for this Client is always
/// `opamp-fleet-client` and is the same for every instance.
const AGENT_NAME: &str = "service-smoke-client";

fn instance() -> InstanceName {
    parse_instance_name(INSTANCE).expect("a legal instance name")
}

fn service() -> NativeService {
    NativeService::new(ServiceLevel::System, instance())
}

/// Uninstalls whatever this test installed, however it ends — a leftover service would make the
/// next run fail for a reason that has nothing to do with the code under test.
struct Registered;

impl Drop for Registered {
    fn drop(&mut self) {
        let _ = service().stop();
        let _ = client(&["service", "uninstall"], &PathBuf::from("client.toml"));
    }
}

/// Runs the real CLI, the way the README's checklist tells an operator to. Driving the binary
/// rather than the library is the point here: `service install` also stages the versioned layout,
/// and an operator's mistake would be in that command line.
fn client(args: &[&str], config: &Path) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_opamp-fleet-client"))
        .arg("--config")
        .arg(config)
        .args(["--instance", INSTANCE])
        .args(args)
        .output()
        .map_err(|e| format!("cannot run the client: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(format!(
            "`client {}` failed with {}\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn wait_for<T>(what: &str, timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(value) = probe() {
            return value;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!("timed out after {timeout:?} waiting for {what}");
}

fn agent(state: &AppState) -> Option<AgentView> {
    state
        .snapshot()
        .into_iter()
        .find(|a| a.service_instance_name == AGENT_NAME)
}

/// The service's process id, asked of the platform's own manager. `None` when nothing is running
/// under that name — which is itself an answer the assertions below use.
fn service_pid() -> Option<u32> {
    // One name on every platform since ADR-0030, so this is what systemd, launchd, and the SCM
    // are each asked about.
    let qualified = service_name(&instance());
    #[cfg(windows)]
    {
        // `sc queryex` prints `PID                : 1234`.
        let out = Command::new("sc")
            .args(["queryex", &qualified])
            .output()
            .expect("run sc queryex");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        parse_after(&text, "PID")
    }
    #[cfg(target_os = "linux")]
    {
        let out = Command::new("systemctl")
            .args(["show", "-p", "MainPID", &format!("{qualified}.service")])
            .output()
            .expect("run systemctl show");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        parse_after(&text, "MainPID")
    }
    #[cfg(target_os = "macos")]
    {
        // `launchctl list <label>` prints a plist; `"PID" = 1234;` is there while it runs.
        let out = Command::new("launchctl")
            .args(["list", &qualified])
            .output()
            .expect("run launchctl list");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        parse_after(&text, "\"PID\"")
    }
}

/// The first non-zero number on the line naming `key`, whatever separator the platform's tool puts
/// between the two.
fn parse_after(text: &str, key: &str) -> Option<u32> {
    text.lines()
        .find(|line| line.trim_start().starts_with(key))?
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .find_map(|part| part.parse::<u32>().ok())
        .filter(|pid| *pid != 0)
}

fn kill(pid: u32) {
    #[cfg(windows)]
    let status = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status();
    #[cfg(unix)]
    let status = Command::new("kill").args(["-9", &pid.to_string()]).status();
    assert!(
        status.expect("run the kill").success(),
        "could not kill the service's process {pid}"
    );
}

/// A Server the installed service can reach. Bound on the loopback interface, so it is reachable
/// from a service running as `LocalSystem` or `root` — a different account, the same machine.
fn spawn_server() -> (
    std::net::SocketAddr,
    Arc<AppState>,
    std::thread::JoinHandle<()>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(AppState::new(dir.path().join("fleet-configs")).expect("configs"));
    std::mem::forget(dir);
    let (tx, rx) = std::sync::mpsc::channel();
    let served = state.clone();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            tx.send(listener.local_addr().expect("addr")).expect("send");
            axum::serve(
                listener,
                server::app(served, server::transport::Admission::open()),
            )
            .await
            .expect("serve");
        });
    });
    let addr = rx.recv().expect("the server's address");
    (addr, state, handle)
}

/// Install → start → the Agent is in the fleet → kill it → the manager brings it back → stop →
/// it stays down → uninstall.
///
/// The kill is the assertion the stand-in service manager cannot make, and the reason this test
/// exists: `RestartPolicy::OnFailure` is what a self-update relies on to come back at all, and on
/// Windows it is a set of recovery actions this Client registers itself.
#[test]
#[ignore = "installs a real system service; run with --ignored in the service-smoke job"]
fn the_installed_service_starts_comes_back_from_a_crash_and_stays_down_after_a_stop() {
    assert_eq!(
        service().state().expect("query the service"),
        ServiceState::NotInstalled,
        "a leftover {INSTANCE} service from an earlier run — remove it before running this"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("install");
    let (addr, state, _server) = spawn_server();
    let config = dir.path().join("client.toml");
    std::fs::write(
        &config,
        format!(
            "endpoint = \"ws://{addr}/v1/opamp\"\nname = \"{AGENT_NAME}\"\nheartbeat_interval_secs = 1\n"
        ),
    )
    .expect("write client.toml");

    client(
        &["service", "install", "--root", &root.to_string_lossy()],
        &config,
    )
    .expect("install the service");
    let _registered = Registered;

    // launchd does not auto-start after an install (a known ADR-0010 gap), so every platform is
    // started explicitly — which is also what the README's checklist tells an operator to do.
    client(&["service", "start"], &config).expect("start the service");
    wait_for(
        "the service to report running",
        Duration::from_secs(60),
        || (service().state().ok()? == ServiceState::Running).then_some(()),
    );

    // The proof that the unit, plist or SCM entry starts a *working* Client: it reaches the Server
    // and appears in the fleet. A malformed program path or working directory dies here.
    wait_for(
        "the Agent to appear in the fleet",
        Duration::from_secs(60),
        || agent(&state).filter(|a| a.connected).map(|_| ()),
    );

    let first = wait_for(
        "the service's process id",
        Duration::from_secs(30),
        service_pid,
    );
    kill(first);
    wait_for(
        "the Agent to drop off the fleet",
        Duration::from_secs(30),
        || agent(&state).filter(|a| !a.connected).map(|_| ()),
    );

    // The restart policy, asserted against the platform that implements it. The delay is the
    // Client's own, so waiting a generous multiple of it is the difference between "not yet" and
    // "never".
    let second = wait_for(
        "the service manager to restart the killed service",
        Duration::from_secs(u64::from(RESTART_DELAY_SECS) * 12),
        || service_pid().filter(|pid| *pid != first),
    );
    assert_ne!(first, second, "a new process, not the one that was killed");
    wait_for(
        "the restarted Agent to reconnect",
        Duration::from_secs(60),
        || agent(&state).filter(|a| a.connected).map(|_| ()),
    );

    // An explicit stop is not a failure: the recovery actions must not fire.
    client(&["service", "stop"], &config).expect("stop the service");
    wait_for(
        "the service to report stopped",
        Duration::from_secs(60),
        || (service().state().ok()? == ServiceState::Stopped).then_some(()),
    );
    std::thread::sleep(Duration::from_secs(u64::from(RESTART_DELAY_SECS) * 3));
    assert_eq!(
        service().state().expect("query the service"),
        ServiceState::Stopped,
        "an explicitly stopped service must stay down (ADR-0010)"
    );

    client(&["service", "uninstall"], &config).expect("uninstall the service");
    wait_for(
        "the service to be deregistered",
        Duration::from_secs(30),
        || (service().state().ok()? == ServiceState::NotInstalled).then_some(()),
    );
}
