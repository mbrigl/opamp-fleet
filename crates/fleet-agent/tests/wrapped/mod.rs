//! Drives a Supervisor kind through `Plugin::start` the way the core drives it (ADR-0017), with
//! the stub standing in for its agent — the harness each kind's integration tests share.
//!
//! The unit tests in each module check what a kind *builds* — the program's name per platform, the
//! arguments, the refusals. What no unit test can show is that a two-line block is actually enough:
//! that the derived program path is where the install puts the program, that the version probe
//! reaches it, that a delivered Configuration lands where the kind points its agent, and that the
//! kind applies it the way its platform allows. A kind's test file declares `mod wrapped;` and
//! asserts that with the functions here.
#![allow(
    dead_code,
    reason = "each integration-test binary uses a different subset of this harness"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleet_agent::shutdown::shutdown_channel;
use fleet_agent::supervisor::ports::{
    EventSender, InstallTarget, Plugin, ProcessCommand, ProcessEvent, SupervisorContext,
};
use opamp::proto::AgentRemoteConfig;
use tokio::sync::mpsc;

/// The stub that stands in for the agent, built by the same `cargo test` run. It prints a version
/// the way a real agent does and then sleeps until it is stopped.
pub fn stub() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(format!("stub_agent{}", std::env::consts::EXE_SUFFIX))
}

/// A started kind: its commands, its events, and the Supervisor's directory it runs in.
pub struct Harness {
    pub commands: mpsc::Sender<ProcessCommand>,
    pub events: mpsc::Receiver<(usize, ProcessEvent)>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    pub dir: tempfile::TempDir,
}

impl Harness {
    /// The directory the delivered Configurations land in.
    pub fn config_dir(&self) -> PathBuf {
        self.dir.path().join("config")
    }
}

/// Starts a kind exactly as the core does — **with no settings at all**, which is the claim a
/// wrapped kind's test makes: its block is `type` and `name`.
pub fn start(plugin: &dyn Plugin, name: &str, stub_args: &[String]) -> Harness {
    start_with(plugin, name, toml::Table::new(), stub_args)
}

/// Starts a kind as [`start`] does, with the block's kind-specific `settings`.
///
/// The program is put where the kind's `defaults()` say the install puts it: inside the unpacked
/// tree for a kind that names a `program_path`, else as a single file named `name` in this
/// Supervisor's own `program/` directory (ADR-0017), found in its archive at the place the kind
/// names (ADR-draft-packages-signatures-and-delivery clause 55). This is the install's side of the
/// same agreement, so a kind whose path disagreed with the install would spawn nothing here.
///
/// `stub_args` are flags for the stub that the kind's own arguments cannot carry; the stub reads
/// them, one per line, from `<program>.args` beside itself.
pub fn start_with(
    plugin: &dyn Plugin,
    name: &str,
    settings: toml::Table,
    stub_args: &[String],
) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("config")).expect("config dir");
    let program_dir = dir.path().join("program");
    let defaults = plugin.defaults();
    let (program, install) = match defaults.program_path {
        // A tree package: the program sits inside the unpacked tree (ADR-0027).
        Some(path) => (
            program_dir.join("tree").join(path),
            InstallTarget::Tree {
                root: program_dir.clone(),
                program_path: PathBuf::from(path),
            },
        ),
        None => {
            let program = program_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            (
                program.clone(),
                InstallTarget::Binary {
                    path: program,
                    member: defaults.member,
                },
            )
        }
    };
    std::fs::create_dir_all(program.parent().expect("a parent")).expect("program dir");
    std::fs::copy(stub(), &program).expect("put the stub where the install would");
    if !stub_args.is_empty() {
        std::fs::write(program.with_extension("args"), stub_args.join("\n")).expect("stub args");
    }

    let (event_tx, events) = mpsc::channel(64);
    let (shutdown_tx, shutdown) = shutdown_channel();
    let ctx = SupervisorContext {
        endpoint_token: zeroize::Zeroizing::new(String::new()),
        name: name.to_string(),
        supervisor_dir: dir.path().to_path_buf(),
        config_dir: dir.path().join("config"),
        program,
        install,
        stop_timeout: Duration::from_secs(5),
        apply_grace: Duration::ZERO,
        retain_previous: Duration::ZERO,
        archive_key: None,
        // No process account: the Client starts the program itself.
        process_account_group: None,
        settings,
        events: EventSender::new(0, event_tx),
        shutdown,
    };
    let commands = plugin.start(ctx).expect("the adapter starts on its block");
    Harness {
        commands,
        events,
        shutdown_tx,
        dir,
    }
}

/// Waits until the agent is **running** and reports its version — the two halves of "it started"
/// that a fleet view shows separately: a pid means the Managed Process is up, and the version means
/// the kind asked it for one. Returns both.
pub async fn wait_for_running_version(harness: &mut Harness) -> (String, u32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let (mut version, mut pid) = (None, None);
    while version.is_none() || pid.is_none() {
        let (_, event) = tokio::time::timeout_at(deadline, harness.events.recv())
            .await
            .expect("the agent to start and report a version in time")
            .expect("an open channel");
        match event {
            ProcessEvent::Pid(Some(reported)) => pid = Some(reported),
            ProcessEvent::Description(description) => {
                // What a kind reports of its process is the process's own `service.*` alone;
                // where it runs is the Supervisor's to state (ADR-0016).
                for attribute in description
                    .identifying_attributes
                    .iter()
                    .chain(&description.non_identifying_attributes)
                {
                    assert!(
                        attribute.key.starts_with("service."),
                        "the kind reports {:?} of its process",
                        attribute.key
                    );
                }
                if let Some(reported) = opamp::attributes::string_value(
                    &description.identifying_attributes,
                    opamp::attributes::SERVICE_VERSION,
                ) {
                    version = Some(reported.to_string());
                }
            }
            _ => {}
        }
    }
    let pid = pid.expect("a pid");
    assert!(pid > 0, "the Managed Process is not running");
    (version.expect("a version"), pid)
}

/// Delivers a Configuration and waits for its acknowledgement, with every process spawned on the
/// way to it: none is an apply in place, one is a restart.
pub async fn apply_config(harness: &mut Harness) -> (Result<(), String>, Vec<u32>) {
    harness
        .commands
        .send(ProcessCommand::ApplyConfig {
            config: AgentRemoteConfig {
                config_hash: b"hash".to_vec(),
                ..Default::default()
            },
            span: tracing::Span::current(),
        })
        .await
        .expect("send");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut spawned = Vec::new();
    loop {
        let (_, event) = tokio::time::timeout_at(deadline, harness.events.recv())
            .await
            .expect("an acknowledgement in time")
            .expect("an open channel");
        match event {
            ProcessEvent::Pid(Some(pid)) => spawned.push(pid),
            ProcessEvent::ConfigApplied { result, .. } => return (result, spawned),
            _ => {}
        }
    }
}

/// Waits until `path` exists — a directory or a file the agent or its kind makes.
pub async fn wait_for(path: &Path, what: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what} at {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until `path` holds `expected` — a file the stub writes once it has got somewhere.
pub async fn wait_for_file(path: &Path, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if std::fs::read_to_string(path).is_ok_and(|text| text.contains(expected)) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} never held {expected:?}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Shuts the kind down and drains its events, so its task ends before the temporary directory is
/// removed under it.
pub async fn stop(mut harness: Harness) {
    harness.shutdown_tx.send(true).expect("shutdown");
    while harness.events.recv().await.is_some() {}
}
