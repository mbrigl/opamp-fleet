//! The Client updating itself, end to end (ADR-0020, ADR-0010) — the one path in this project
//! whose failure takes a host out of the fleet's reach for good, and the only major feature that
//! had no test across the process boundary.
//!
//! Everything real except the service manager: the real Server armed with a package store, the
//! real Client binary running from a real ADR-0010 install layout, installing a real artifact over
//! itself. The restart the Client asks for by exiting is performed *here* — that is precisely the
//! step no CI can delegate to systemd, launchd, or the SCM, and standing in for it is what makes
//! the rest observable.
//!
//! The contract under test is the fleet's, not the mechanism's: whatever number of restarts it
//! takes, the Server must end up being told the new version is `Installed`. Asserting the step
//! count instead would freeze an implementation detail that ADR-0020 deliberately leaves open.
//!
//! Runs on all three platforms, because the pointer is the part of ADR-0010 that differs between
//! them — a symlink on Unix, a junction on Windows — and it is precisely what a self-update moves
//! while the Client is running through it. A test that only ever moved the symlink would leave the
//! platform whose mechanism is the unusual one entirely unasserted.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use server::fleet::{AgentView, AppState, PackageOffering};
use server::packages::{PackageStore, Platform};

/// The Platform this test's Client will report about itself (ADR-0031) — the Server offers only
/// the artifact that fits the machine, so a self-update test has to store one for this one.
fn this_host() -> Platform {
    Platform::new(std::env::consts::OS, std::env::consts::ARCH).expect("this host has a platform")
}

// What the Client exits with to ask its service manager for a restart, and its file name inside a
// version directory. Imported rather than restated since ADR-0024: both were copied here with a
// comment saying `client` is a binary crate and a test cannot link it, and a copied constant is a
// correctness risk that no comment can remove.
use client::selfupdate::EXIT_RESTART_FOR_UPDATE;
use client::service::layout::BINARY_FILENAME as CLIENT_BINARY;

/// The version directory laid out before the update. Joined one component at a time, never as
/// `versions/<name>`: this test builds the Windows pointer with `mklink`, a `cmd` builtin that
/// reads an embedded `/` as the start of a switch.
const PREVIOUS_VERSION_DIR: &str = "opamp-fleet-client-0.0.0-previous";

async fn wait_until<T>(what: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if let Some(value) = probe() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn spawn_server(store: PackageStore) -> (std::net::SocketAddr, Arc<AppState>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(
        AppState::new(dir.path().join("fleet-configs"))
            .expect("configs")
            .with_packages(Some(PackageOffering::new(store, String::new()))),
    );
    // The configuration directory only has to outlive the Server, which outlives the test.
    std::mem::forget(dir);
    let app = server::app(state.clone(), server::transport::Admission::open());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (addr, state)
}

/// The version this Client binary reports for itself (ADR-0009, baked in at build time). The
/// package must be offered under exactly this version, because the staged binary is this same
/// binary and the self-check compares the two.
fn version_of(binary: &Path) -> String {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .expect("run --version");
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .last()
        .expect("a version in the output")
        .to_string()
}

/// Lays out `<root>/versions/<dir>/<client>` + `<root>/current` the way `service install` would,
/// and returns the path the "service" runs — through the pointer, which is how the service manager
/// is registered and therefore the only way this is worth starting.
///
/// The version directory is deliberately **not** named after the version this binary reports:
/// installing a package resolves its target directory from the offered version, and a Client
/// already running from that directory is told the version it was offered is the one it runs. A
/// host that installed by hand and one that arrived by package differ exactly here, and the
/// interesting case is the one where the two names differ.
fn install_layout(root: &Path, client: &Path) -> PathBuf {
    let version_dir = root.join("versions").join(PREVIOUS_VERSION_DIR);
    std::fs::create_dir_all(&version_dir).expect("create the version directory");
    let binary = version_dir.join(CLIENT_BINARY);
    std::fs::copy(client, &binary).expect("copy the client");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    point_current_at(root, &version_dir);
    root.join("current").join(CLIENT_BINARY)
}

/// Creates `<root>/current` pointing at `version_dir`, by the mechanism `service install` uses on
/// this platform (`layout::set_current`): a symbolic link on Unix, a directory junction on Windows.
///
/// Restated here rather than called, for the reason the exit code above is: `client` is a binary
/// crate. Restating it is also what makes the Windows run worth having — a junction is created by
/// `mklink /J` and needs no privilege, and if that stopped being true this test would say so.
fn point_current_at(root: &Path, version_dir: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(version_dir, root.join("current")).expect("point current");
    #[cfg(windows)]
    {
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("current"))
            .arg(version_dir)
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J failed with {status}");
    }
}

/// A Client under a stand-in service manager: it is restarted whenever it exits, exactly as
/// systemd would, and every exit code it produced is kept for the assertions.
struct Supervised {
    program: PathBuf,
    config: PathBuf,
    child: Child,
    exits: Vec<i32>,
}

impl Supervised {
    fn start(program: &Path, config: &Path) -> Self {
        Supervised {
            program: program.to_path_buf(),
            config: config.to_path_buf(),
            child: Self::spawn(program, config),
            exits: Vec::new(),
        }
    }

    fn spawn(program: &Path, config: &Path) -> Child {
        // The Client logs to stdout; keeping it beside the configuration makes a failing run
        // readable instead of silent.
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.with_extension("log"))
            .expect("open the client log");
        Command::new(program)
            .arg("--config")
            .arg(config)
            .stdout(Stdio::from(log))
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the client")
    }

    /// Restarts the process if it has exited. Call it from the polling loop — this is the service
    /// manager's whole job in this test.
    fn tend(&mut self) {
        if let Some(status) = self.child.try_wait().expect("wait on the client") {
            self.exits.push(status.code().unwrap_or(-1));
            assert!(
                self.exits.len() <= 8,
                "the Client keeps restarting without settling: exits {:?}",
                self.exits
            );
            self.child = Self::spawn(&self.program, &self.config);
        }
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Finds an Agent by the operator's name for it — `service.instance.name` (ADR-0033). The Client's
/// configured `name` is its *instance* name; its `service.name` is the constant type
/// `opamp-fleet-client`, the same on every host in the fleet, which is what a Selector aiming the
/// Client's own package matches on.
fn view<'a>(agents: &'a [AgentView], name: &str) -> Option<&'a AgentView> {
    agents.iter().find(|a| a.service_instance_name == name)
}

fn config_toml(addr: std::net::SocketAddr, state_dir: &Path, package: &str) -> String {
    format!(
        concat!(
            "endpoint = \"ws://{addr}/v1/opamp\"\n",
            "name = \"self-updating-client\"\n",
            "state_dir = {state:?}\n",
            "heartbeat_interval_secs = 1\n\n",
            "[self_update]\n",
            "package = \"{package}\"\n",
        ),
        addr = addr,
        state = state_dir.to_string_lossy(),
        package = package,
    )
}

/// The whole loop: the Server offers the Client a version of itself, the Client stages it beside
/// the running one, proves it with `self-check`, moves `current`, and asks to be restarted — and
/// whatever comes up afterwards owes the Server a terminal status, which must be `Installed`.
#[tokio::test]
async fn the_client_installs_a_version_of_itself_and_reports_it_installed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = PathBuf::from(env!("CARGO_BIN_EXE_opamp-fleet-client"));
    // Offered the way an operator uploads a release: the number on the archive, without the commit
    // the build carries (ADR-0029). The staged binary reports the full string and must still be
    // recognised as this release — the failure that ADR exists for.
    let full = version_of(&client);
    // Whether the two actually differ depends on how this build was versioned, so the guarantee
    // itself is pinned by `selfupdate`'s own `the_probe_ignores_the_commit_a_build_came_from`;
    // what this test adds is that the whole loop runs on the operator's spelling.
    let version = opamp::version::identity(&full)
        .unwrap_or_else(|| panic!("{full:?} is not a version"))
        .to_string();

    // The artifact is this very binary: the only thing that will pass the staged binary's own
    // self-check, which requires it to *be* an OpAMP Fleet Client at the offered version.
    let artifact = std::fs::read(&client).expect("read the client binary");
    let store_dir = tempfile::tempdir().expect("store dir");
    let store = PackageStore::open(store_dir.path().to_path_buf()).expect("store");
    // The Client's own Agent reports the constant type `opamp-fleet-client` (ADR-0033), and a Set
    // reaches only Agents of its type — the type is part of its identity (ADR-0052).
    let set = server::packages::SetId::new("opamp-fleet-client", "opamp-fleet-client", &version)
        .expect("set id");
    store
        .create_or_update(&set, Default::default(), false)
        .expect("create set");
    store
        .put_entry(&set, &this_host(), None, artifact)
        .expect("put entry");
    // And released, or it is a draft the fleet never sees (ADR-0043).
    store.set_published(&set, true).expect("publish");

    let (addr, state) = spawn_server(store).await;
    let root = dir.path().join("install");
    let program = install_layout(&root, &client);
    let state_dir = dir.path().join("client-state");
    let config = dir.path().join("client.toml");
    std::fs::write(&config, config_toml(addr, &state_dir, "opamp-fleet-client"))
        .expect("write config");

    let mut service = Supervised::start(&program, &config);

    // The contract: however many restarts it takes, the Server is told the new version is in.
    wait_until("the self-update to be reported Installed", || {
        service.tend();
        let snapshot = state.snapshot();
        let agent = view(&snapshot, "self-updating-client")?;
        let package = agent
            .packages
            .iter()
            .find(|p| p.name == "opamp-fleet-client")?;
        (package.status == "Installed" && package.version == version).then_some(())
    })
    .await;

    // The configured `name` names this instance; the type is the shipped binary's name and the
    // same for every Client in the fleet, so one Selector aims the Client's package at all of them
    // without naming a host (ADR-0028, ADR-0033).
    assert_eq!(
        view(&state.snapshot(), "self-updating-client")
            .expect("the client's own agent")
            .service_name,
        "opamp-fleet-client"
    );

    assert!(
        service.exits.contains(&EXIT_RESTART_FOR_UPDATE),
        "the Client asked its service manager for the restart rather than restarting itself: \
         exits {:?}",
        service.exits
    );

    // `current` moved to a directory named after the offered version, and the previous one is
    // still there — a rollback needs somewhere to go back to.
    let current = std::fs::canonicalize(root.join("current")).expect("current resolves");
    assert_ne!(
        current,
        std::fs::canonicalize(root.join("versions").join(PREVIOUS_VERSION_DIR))
            .expect("the previous version"),
        "current still points at the version that was running before the update"
    );
    assert!(
        current.join(CLIENT_BINARY).is_file(),
        "the new version is staged"
    );
    assert!(
        root.join("versions")
            .join(PREVIOUS_VERSION_DIR)
            .join(CLIENT_BINARY)
            .is_file(),
        "the version it replaced is kept, so a rollback has a target"
    );
    assert!(
        !state_dir.join("self-update.json").exists(),
        "the in-flight marker is cleared once the new version has settled"
    );
}

/// The name in `[self_update]` is the whole of the protection (ADR-0020): a package with an empty
/// Selector reaches every consenting Agent, and one written over the Client would take the host
/// out of reach. Anything not called what that section says is refused and reported — never
/// applied, and never a reason to restart.
#[tokio::test]
async fn a_package_under_another_name_is_refused_and_the_client_keeps_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let client = PathBuf::from(env!("CARGO_BIN_EXE_opamp-fleet-client"));
    let version = version_of(&client);

    // A perfectly good Client artifact — under a name this Client did not consent to.
    let artifact = std::fs::read(&client).expect("read the client binary");
    let store_dir = tempfile::tempdir().expect("store dir");
    let store = PackageStore::open(store_dir.path().to_path_buf()).expect("store");
    // Typed so that it *does* reach the Client, which is the only way this test can still test what
    // it is named for. ADR-0034 makes the Server refuse to send a package of another type, but the
    // two guards are independent by design — so the case exercised here is the one where the
    // Server's guard does not fire: an operator uploads a Collector artifact and mistypes its
    // agent type as the Client's. The Client's name check (ADR-0020) is then all that is left.
    let set =
        server::packages::SetId::new("otelcol", "opamp-fleet-client", &version).expect("set id");
    store
        .create_or_update(&set, Default::default(), false)
        .expect("create set");
    store
        .put_entry(&set, &this_host(), None, artifact)
        .expect("put entry");
    store.set_published(&set, true).expect("publish");

    let (addr, state) = spawn_server(store).await;
    let root = dir.path().join("install");
    let program = install_layout(&root, &client);
    let previous = std::fs::canonicalize(root.join("current")).expect("current resolves");
    let state_dir = dir.path().join("client-state");
    let config = dir.path().join("client.toml");
    std::fs::write(&config, config_toml(addr, &state_dir, "opamp-fleet-client"))
        .expect("write config");

    let mut service = Supervised::start(&program, &config);

    // An offer refused outright has no package status to hang the reason on — the report carries
    // it, and the fleet view shows it as `package_error`.
    let error = wait_until("the offer to be refused", || {
        service.tend();
        let snapshot = state.snapshot();
        let agent = view(&snapshot, "self-updating-client")?;
        (!agent.package_error.is_empty()).then(|| agent.package_error.clone())
    })
    .await;

    assert!(
        error.contains("opamp-fleet-client") && error.contains("otelcol"),
        "the refusal names both what it takes and what it was offered: {error:?}"
    );
    assert!(
        state.snapshot().iter().all(|a| a.packages.is_empty()),
        "nothing was installed"
    );
    assert!(
        service.exits.is_empty(),
        "a refused package is not a reason to restart: exits {:?}",
        service.exits
    );
    assert_eq!(
        std::fs::canonicalize(root.join("current")).expect("current resolves"),
        previous,
        "nothing was pointed anywhere else"
    );
}
