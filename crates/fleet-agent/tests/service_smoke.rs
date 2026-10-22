//! The Client under the machine's **real** service manager — systemd, launchd, or the SCM
//! (ADR-0019, ADR-0018).
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
//! sudo -E cargo test -p fleet-agent --test service_smoke -- --ignored --nocapture
//! ```
//!
//! What it does **not** cover: starting at boot (a runner never reboots), long-running behaviour,
//! and hosts with SELinux or AppArmor in the way. The manual checklist in `README.md` keeps those.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleet_agent::service::manager::{service_name, NativeService, RESTART_DELAY_SECS};
use fleet_agent::service::{ServiceControl, ServiceLevel, ServiceState};
use fleet_server::fleet::{AgentView, AppState};

/// The operator's name for this Agent — written as `name` in the `supervisor.toml` below and
/// reported as `service.instance.name` (ADR-0016), which is what the fleet view is searched by.
/// Deliberately not `service.name`: that carries the Agent *type*, which for this Client is always
/// `supervisor` (ADR-0019).
///
/// There is no `--instance` to isolate this run under (ADR-0019 clause 6), and the service it
/// registers carries the product's name like any other install. This test therefore takes over the
/// host's one service for its duration, which is what `Registered` exists to undo.
const AGENT_NAME: &str = "service-smoke-client";

fn service() -> NativeService {
    NativeService::new(ServiceLevel::System)
}

/// Uninstalls whatever this test installed, however it ends — a leftover service would make the
/// next run fail for a reason that has nothing to do with the code under test.
struct Registered;

impl Drop for Registered {
    fn drop(&mut self) {
        let _ = service().stop();
        let _ = client(&["service", "uninstall"], &PathBuf::from("supervisor.toml"));
    }
}

/// Runs the real CLI, the way the README's checklist tells an operator to. Driving the binary
/// rather than the library is the point here: `service install` also stages the versioned layout,
/// and an operator's mistake would be in that command line.
fn client(args: &[&str], config: &Path) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_supervisor"))
        .arg("--config")
        .arg(config)
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
    // One name on every platform (ADR-0019 clause 3), so this is what systemd, launchd, and the SCM
    // are each asked about.
    let qualified = service_name();
    #[cfg(windows)]
    {
        // `sc queryex` prints `PID                : 1234`.
        let out = Command::new("sc")
            .args(["queryex", qualified])
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
            .args(["list", qualified])
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
                fleet_server::agent_app(served, fleet_server::transport::Admission::open()),
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
/// Verifies: ADR-0018, ADR-0019, G-11
#[test]
#[ignore = "installs a real system service; run with --ignored in the service-smoke job"]
fn the_installed_service_starts_comes_back_from_a_crash_and_stays_down_after_a_stop() {
    assert_eq!(
        service().state().expect("query the service"),
        ServiceState::NotInstalled,
        "a leftover {} service from an earlier run — remove it before running this",
        service_name()
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("install");
    let (addr, state, _server) = spawn_server();
    let config = dir.path().join("supervisor.toml");
    std::fs::write(
        &config,
        format!(
            "endpoint = \"ws://{addr}/v1/opamp\"\nname = \"{AGENT_NAME}\"\nheartbeat_interval_secs = 1\n{}",
            common::client_identity(dir.path())
        ),
    )
    .expect("write supervisor.toml");

    client(
        &["service", "install", "--root", &root.to_string_lossy()],
        &config,
    )
    .expect("install the service");
    let _registered = Registered;

    // launchd does not auto-start after an install (a known ADR-0019 gap), so every platform is
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
        "an explicitly stopped service must stay down (ADR-0019)"
    );

    client(&["service", "uninstall"], &config).expect("uninstall the service");
    wait_for(
        "the service to be deregistered",
        Duration::from_secs(30),
        || (service().state().ok()? == ServiceState::NotInstalled).then_some(()),
    );
}

/// A temporary system account for one test, removed however the test ends.
#[cfg(target_os = "linux")]
struct TemporaryAccount(&'static str);

#[cfg(target_os = "linux")]
impl TemporaryAccount {
    fn create(name: &'static str) -> Self {
        let status = Command::new("useradd")
            .args([
                "--system",
                "--user-group",
                "--no-create-home",
                "--shell",
                "/usr/sbin/nologin",
                name,
            ])
            .status()
            .expect("run useradd");
        assert!(status.success(), "cannot create the account {name}");
        Self(name)
    }
}

#[cfg(target_os = "linux")]
impl Drop for TemporaryAccount {
    fn drop(&mut self) {
        let _ = Command::new("userdel").arg(self.0).status();
    }
}

/// A temporary role account with a group of its own, made with the lines the refusal of a
/// missing process account shows on macOS: `dseditgroup` creates the group, and `sysadminctl
/// -roleAccount` the account, with that group as its primary one. Removed however the test ends.
#[cfg(target_os = "macos")]
struct TemporaryAccount(&'static str);

#[cfg(target_os = "macos")]
impl TemporaryAccount {
    fn create(name: &'static str) -> Self {
        // A role account's uid lies in a range `sysadminctl` names, which macOS releases have
        // moved: the first free id of each range is tried, and the account looked up after.
        for range in [450..500, 300..400] {
            let Some(id) = range.into_iter().find(|id| id_is_free(*id)) else {
                continue;
            };
            let id = id.to_string();
            let group = Command::new("dseditgroup")
                .args(["-o", "create", "-i", &id, name])
                .status()
                .expect("run dseditgroup");
            assert!(group.success(), "cannot create the group {name}");
            let account = Self(name);
            let _ = Command::new("sysadminctl")
                .args(["-addUser", name, "-UID", &id, "-GID", &id, "-roleAccount"])
                .status()
                .expect("run sysadminctl");
            // `sysadminctl`'s exit status is not taken on faith: the account is looked up.
            let found = fleet_agent::service::run_as::account_of(id.parse().expect("an id"))
                .expect("the password database");
            if found
                .as_ref()
                .is_some_and(|found| found.name == name && found.gid.to_string() == id)
            {
                return account;
            }
            drop(account);
        }
        panic!("sysadminctl created no role account {name} with a group of its own");
    }
}

/// Whether neither an account nor a group holds `id`.
#[cfg(target_os = "macos")]
fn id_is_free(id: u32) -> bool {
    // SAFETY: a lookup by id; the entry it may return is not read.
    let group = unsafe { libc::getgrgid(id) };
    group.is_null()
        && fleet_agent::service::run_as::account_of(id)
            .expect("the password database")
            .is_none()
}

#[cfg(target_os = "macos")]
impl Drop for TemporaryAccount {
    fn drop(&mut self) {
        let _ = Command::new("dscl")
            .args([".", "-delete", &format!("/Users/{}", self.0)])
            .status();
        let _ = Command::new("dseditgroup")
            .args(["-o", "delete", self.0])
            .status();
    }
}

/// The accounts the process account's smoke test creates: on macOS a role account's name begins
/// with `_`.
#[cfg(target_os = "linux")]
const SMOKE_ACCOUNTS: (&str, &str) = ("opamp-fleet-smoke", "opamp-fleet-smoke-agents");
#[cfg(target_os = "macos")]
const SMOKE_ACCOUNTS: (&str, &str) = ("_opamp-fleet-smoke", "_opamp-fleet-smoke-agents");

/// A temporary directory both accounts reach: under `/tmp` on Linux, and under `/private/tmp` on
/// macOS, where the default one lies in the invoking user's own `0700` directory and `/tmp` is a
/// link.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn shared_tempdir() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    return tempfile::tempdir_in("/private/tmp").expect("tempdir");
    #[cfg(target_os = "linux")]
    tempfile::tempdir().expect("tempdir")
}

/// Install under an account, then re-install without `--run-as`, as every package upgrade does:
/// the service still runs as the account, and the unit the Client wrote leaves it the privileges
/// a Managed Process needs for `sudo` and setuid helpers — no `no_new_privs`, and `/` not mounted
/// `nosuid`. `systemd-analyze verify` accepts the unit.
/// Verifies: ADR-0042
#[cfg(target_os = "linux")]
#[test]
#[ignore = "installs a real system service; run with --ignored in the service-smoke job"]
fn a_reregistration_keeps_the_account_and_the_service_keeps_its_privileges() {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        service().state().expect("query the service"),
        ServiceState::NotInstalled,
        "a leftover {} service from an earlier run — remove it before running this",
        service_name()
    );
    let account = TemporaryAccount::create("opamp-fleet-smoke");

    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("install");
    let (addr, state, _server) = spawn_server();
    let config = dir.path().join("supervisor.toml");
    std::fs::write(
        &config,
        format!(
            "endpoint = \"ws://{addr}/v1/opamp\"\nname = \"{AGENT_NAME}\"\nheartbeat_interval_secs = 1\n{}",
            common::client_identity(dir.path())
        ),
    )
    .expect("write supervisor.toml");
    // The account must reach its program and read the identity beside the configuration, which
    // lie outside the roots the install hands over.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod the temporary directory");
    for file in ["test-client.pem", "test-client-key.pem"] {
        let status = Command::new("chown")
            .arg(account.0)
            .arg(dir.path().join(file))
            .status()
            .expect("run chown");
        assert!(status.success(), "cannot give {file} to the account");
    }

    let root_arg = root.to_string_lossy().into_owned();
    client(
        &[
            "service", "install", "--root", &root_arg, "--run-as", account.0,
        ],
        &config,
    )
    .expect("install the service under the account");
    let _registered = Registered;
    let again = client(&["service", "install", "--root", &root_arg], &config)
        .expect("re-install without --run-as");
    assert!(
        again.contains(&format!("runs as:   {} (kept from", account.0)),
        "{again}"
    );

    client(&["service", "start"], &config).expect("start the service");
    wait_for(
        "the Agent to appear in the fleet",
        Duration::from_secs(60),
        || agent(&state).filter(|a| a.connected).map(|_| ()),
    );
    let pid = wait_for(
        "the service's process id",
        Duration::from_secs(30),
        service_pid,
    );

    let user = Command::new("systemctl")
        .args(["show", "-p", "User", &format!("{}.service", service_name())])
        .output()
        .expect("run systemctl show");
    assert_eq!(
        String::from_utf8_lossy(&user.stdout).trim(),
        format!("User={}", account.0)
    );
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("status");
    let no_new_privs = status
        .lines()
        .find(|l| l.starts_with("NoNewPrivs:"))
        .expect("NoNewPrivs line");
    assert_eq!(
        no_new_privs.split_whitespace().nth(1),
        Some("0"),
        "{status}"
    );
    let mounts = std::fs::read_to_string(format!("/proc/{pid}/mountinfo")).expect("mountinfo");
    let slash = mounts
        .lines()
        .rev()
        .find(|l| l.split_whitespace().nth(4) == Some("/"))
        .expect("the mount of /");
    let options = slash.split_whitespace().nth(5).expect("mount options");
    assert!(!options.split(',').any(|o| o == "nosuid"), "{slash}");
    let verify = Command::new("systemd-analyze")
        .args([
            "verify",
            &format!("/etc/systemd/system/{}.service", service_name()),
        ])
        .output()
        .expect("run systemd-analyze verify");
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );

    client(&["service", "stop"], &config).expect("stop the service");
    client(&["service", "uninstall"], &config).expect("uninstall the service");
}

/// Adds the account `member` to the group `group`, as the operator does before naming a process
/// account (ADR-0059 clause 6).
#[cfg(target_os = "linux")]
fn add_to_group(member: &str, group: &str) {
    let status = Command::new("usermod")
        .args(["--append", "--groups", group, member])
        .status()
        .expect("run usermod");
    assert!(status.success(), "cannot add {member} to {group}");
}

#[cfg(target_os = "macos")]
fn add_to_group(member: &str, group: &str) {
    let status = Command::new("dseditgroup")
        .args(["-o", "edit", "-a", member, "-t", "user", group])
        .status()
        .expect("run dseditgroup");
    assert!(status.success(), "cannot add {member} to {group}");
}

/// What the Managed Process below found out about itself, once it has written it: its pid, its
/// account, and what it could and could not reach.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn reported(result: &Path, other_than: Option<u32>) -> std::collections::BTreeMap<String, String> {
    wait_for(
        "the Managed Process to report",
        Duration::from_secs(90),
        || {
            let text = std::fs::read_to_string(result).ok()?;
            let report: std::collections::BTreeMap<String, String> = text
                .lines()
                .filter_map(|line| line.split_once('='))
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            let pid = report.get("pid")?.parse::<u32>().ok()?;
            (Some(pid) != other_than).then_some(report)
        },
    )
}

/// The account a running process runs as: its uid asked of `ps`, its name of the password
/// database, which no column width shortens.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn user_of(pid: &str) -> String {
    let out = Command::new("ps")
        .args(["-o", "uid=", "-p", pid])
        .output()
        .expect("run ps");
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let Ok(uid) = uid.parse() else {
        return format!("no process {pid}");
    };
    fleet_agent::service::run_as::account_of(uid)
        .expect("the password database")
        .map_or_else(|| format!("uid {uid}"), |account| account.name)
}

/// The uid and primary gid of `account`.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ids_of(account: &str) -> (u32, u32) {
    let id = |flag: &str| -> u32 {
        let out = Command::new("id")
            .args([flag, account])
            .output()
            .expect("run id");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("an id")
    };
    (id("-u"), id("-g"))
}

/// The owner, group and mode of `path`, set-ID bits included.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn owner_and_mode(path: &Path) -> (u32, u32, u32) {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::symlink_metadata(path).expect("metadata");
    (meta.uid(), meta.gid(), meta.mode() & 0o7777)
}

/// A Managed Process under a process account (ADR-0059): installed with `--run-as` and
/// `--process-account` naming two real accounts, the Client starts its program through the second
/// service, and the program runs as the process account. It reads its `config/`, writes into its
/// data directory, and can neither read the Client's `client-key.pem` or its log nor write its own
/// `program/`. After a restart of the second service the Client reaches it again and the program
/// runs again. A re-registration without either flag, as every package upgrade makes, keeps both
/// accounts and leaves the program its data: what it wrote is still its own, and it writes there
/// again. `--no-process-account` returns the program to the Client's account. The in-process tests stand in for the second
/// service under the test's own uid; this is the one place the shipped binary starts a program
/// over the socket under another account. On Linux under systemd, and on macOS under launchd with
/// role accounts made as the refusal of a missing account says.
/// Verifies: ADR-0059
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
#[ignore = "installs a real system service; run with --ignored in the service-smoke job"]
fn a_managed_process_runs_under_the_process_account_and_reaches_only_what_it_runs() {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(
        service().state().expect("query the service"),
        ServiceState::NotInstalled,
        "a leftover {} service from an earlier run — remove it before running this",
        service_name()
    );
    let client_account = TemporaryAccount::create(SMOKE_ACCOUNTS.0);
    let process_account = TemporaryAccount::create(SMOKE_ACCOUNTS.1);
    add_to_group(client_account.0, process_account.0);

    let dir = shared_tempdir();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))
        .expect("chmod the temporary directory");
    let root = dir.path().join("install");
    let state = root.join("state");
    let supervisor = state.join("supervisors").join("probe");
    let program = supervisor.join("program");
    let config_dir = supervisor.join("config");
    let data = supervisor.join("data");
    std::fs::create_dir_all(&program).expect("program/");
    std::fs::create_dir_all(&config_dir).expect("config/");
    // A data directory as clause 6 gives it: the process account's group, `2770`. The command
    // kind names none, so the test makes it as a kind would.
    std::fs::create_dir_all(&data).expect("data/");
    let chgrp = Command::new("chgrp")
        .arg(process_account.0)
        .arg(&data)
        .status()
        .expect("run chgrp");
    assert!(
        chgrp.success(),
        "cannot give data/ to the process account's group"
    );
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o2770)).expect("chmod data/");
    // Where the Managed Process says what it found: writable by anyone, outside every root.
    let out = dir.path().join("out");
    std::fs::create_dir(&out).expect("out/");
    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o777)).expect("chmod out/");
    let result = out.join("result");

    // The Client's key where ADR-0059 protects it: in the state directory, owner-only.
    common::client_identity(dir.path());
    let key = state.join("client-key.pem");
    let cert = state.join("client.pem");
    std::fs::rename(dir.path().join("test-client-key.pem"), &key).expect("place the key");
    std::fs::rename(dir.path().join("test-client.pem"), &cert).expect("place the certificate");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod key");

    std::fs::write(config_dir.join("settings"), "from the config\n").expect("config entry");
    let script = program.join("probe.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             tmp={out}/result.tmp\n\
             {{\n\
             echo \"pid=$$\"\n\
             echo \"user=$(id -un)\"\n\
             if cat {key} >/dev/null 2>&1; then echo key=read; else echo key=denied; fi\n\
             if ls {logs} >/dev/null 2>&1; then echo logs=read; else echo logs=denied; fi\n\
             if (: > {program}/written) 2>/dev/null; then echo program=written; else echo program=denied; fi\n\
             if (echo $$ >> {data}/written) 2>/dev/null; then echo data=written; else echo data=denied; fi\n\
             echo \"config=$(cat {config}/settings 2>/dev/null)\"\n\
             }} > \"$tmp\"\n\
             mv \"$tmp\" {out}/result\n\
             exec sleep 3600\n",
            out = out.display(),
            key = key.display(),
            logs = state.join("logs").display(),
            program = program.display(),
            data = data.display(),
            config = config_dir.display(),
        ),
    )
    .expect("write the program");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
        .expect("chmod the program");

    let (addr, state_view, _server) = spawn_server();
    let config = dir.path().join("supervisor.toml");
    std::fs::write(
        &config,
        format!(
            "endpoint = \"ws://{addr}/v1/opamp\"\nname = \"{AGENT_NAME}\"\nheartbeat_interval_secs = 1\n\
             [tls]\ncert_file = {cert:?}\nkey_file = {key:?}\n\n\
             [[supervisor]]\ntype = \"command\"\nname = \"probe\"\ncommand = \"probe.sh\"\n",
            cert = cert.display().to_string(),
            key = key.display().to_string(),
        ),
    )
    .expect("write supervisor.toml");

    let root_arg = root.to_string_lossy().into_owned();
    client(
        &[
            "service",
            "install",
            "--root",
            &root_arg,
            "--run-as",
            client_account.0,
            "--process-account",
            process_account.0,
        ],
        &config,
    )
    .expect("install the service with a process account");
    let _registered = Registered;
    client(&["service", "start"], &config).expect("start the service");
    wait_for(
        "the Agent to appear in the fleet",
        Duration::from_secs(60),
        || agent(&state_view).filter(|a| a.connected).map(|_| ()),
    );

    let first = reported(&result, None);
    assert_eq!(first["user"], process_account.0, "{first:?}");
    assert_eq!(user_of(&first["pid"]), process_account.0, "{first:?}");
    assert_eq!(first["key"], "denied", "{first:?}");
    assert_eq!(first["logs"], "denied", "{first:?}");
    assert_eq!(first["program"], "denied", "{first:?}");
    assert_eq!(first["config"], "from the config", "{first:?}");
    assert_eq!(first["data"], "written", "{first:?}");

    // The second service restarts, and its programs with it: the Client reaches it again and
    // starts the program again, under the process account.
    let first_pid: u32 = first["pid"].parse().expect("a pid");
    let agents = fleet_agent::service::manager::agents_service_name();
    #[cfg(target_os = "linux")]
    let restarted = Command::new("systemctl")
        .args(["restart", &format!("{agents}.service")])
        .status()
        .expect("run systemctl restart");
    #[cfg(target_os = "macos")]
    let restarted = Command::new("launchctl")
        .args(["kickstart", "-k", &format!("system/{agents}")])
        .status()
        .expect("run launchctl kickstart");
    assert!(restarted.success(), "cannot restart the second service");
    let again = reported(&result, Some(first_pid));
    assert_eq!(user_of(&again["pid"]), process_account.0, "{again:?}");

    // Re-registered without either flag, as every package upgrade does: both accounts are kept,
    // and the hand-over leaves the process account its data (ADR-0059 clause 10).
    let again_pid: u32 = again["pid"].parse().expect("a pid");
    client(&["service", "stop"], &config).expect("stop the service");
    let reinstalled = client(&["service", "install", "--root", &root_arg], &config)
        .expect("re-install without --run-as and --process-account");
    assert!(
        reinstalled.contains(&format!("processes: {} (kept from", process_account.0)),
        "{reinstalled}"
    );
    let (client_uid, _) = ids_of(client_account.0);
    let (process_uid, process_gid) = ids_of(process_account.0);
    assert_eq!(
        (owner_and_mode(&data.join("written")), owner_and_mode(&data)),
        (
            (process_uid, process_gid, 0o644),
            (client_uid, process_gid, 0o2770)
        ),
        "the hand-over took the data from the process account"
    );
    client(&["service", "start"], &config).expect("start the service again");
    let upgraded = reported(&result, Some(again_pid));
    assert_eq!(user_of(&upgraded["pid"]), process_account.0, "{upgraded:?}");
    assert_eq!(upgraded["data"], "written", "{upgraded:?}");
    assert_eq!(upgraded["config"], "from the config", "{upgraded:?}");
    assert_eq!(upgraded["key"], "denied", "{upgraded:?}");

    // Dropped: the Client starts its program itself again, under its own account.
    let again_pid: u32 = upgraded["pid"].parse().expect("a pid");
    client(&["service", "stop"], &config).expect("stop the service");
    client(
        &[
            "service",
            "install",
            "--root",
            &root_arg,
            "--no-process-account",
        ],
        &config,
    )
    .expect("drop the process account");
    client(&["service", "start"], &config).expect("start the service again");
    let dropped = reported(&result, Some(again_pid));
    assert_eq!(dropped["user"], client_account.0, "{dropped:?}");

    client(&["service", "stop"], &config).expect("stop the service");
    client(&["service", "uninstall"], &config).expect("uninstall the service");
}
