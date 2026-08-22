//! The two kinds whose whole block is `type` and `name` (ADR-0091), driven through
//! `Plugin::start` the way the core drives them: [`glpi`](ADR-0093) and
//! [`telegraf`](ADR-0094).
//!
//! The unit tests in each module check what the kind *builds* — the program's name per platform,
//! the arguments, the refusals. What no unit test can show is that a two-line block is actually
//! enough: that the derived program path is where the install puts the program, that the version
//! probe reaches it, that a delivered Configuration lands where the kind points its agent, and
//! that the directories the agent writes into exist by the time it runs. That is what an operator
//! finds out on the first host, and it is what this file asserts.

use std::path::{Path, PathBuf};
use std::time::Duration;

use client::service::runtime::shutdown_channel;
use client::supervisor::ports::{
    EventSender, Plugin, ProcessCommand, ProcessEvent, SupervisorContext,
};
use client::supervisor::process::InstallTarget;
use opamp::proto::AgentRemoteConfig;
use tokio::sync::mpsc;

/// The stub that stands in for the agent, built by the same `cargo test` run. It prints a version
/// the way a real agent does and then sleeps until it is stopped.
fn stub() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(format!("stub_agent{}", std::env::consts::EXE_SUFFIX))
}

struct Harness {
    commands: mpsc::Sender<ProcessCommand>,
    events: mpsc::Receiver<(usize, ProcessEvent)>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    dir: tempfile::TempDir,
}

impl Harness {
    fn config_dir(&self) -> PathBuf {
        self.dir.path().join("config")
    }
}

/// Starts a wrapped kind exactly as the core does — **with no settings at all**, which is the
/// claim under test.
///
/// `program_path` is what the kind's `defaults()` says, and the stub is put exactly there: this is
/// the install's side of the same agreement, so a kind whose path disagreed with the artifact
/// would spawn nothing here.
fn start(plugin: &dyn Plugin, name: &str, program_path: Option<&str>) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("config")).expect("config dir");
    let program_dir = dir.path().join("program");
    let (program, install) = match program_path {
        // A tree package: the program sits inside the unpacked tree (ADR-0023).
        Some(path) => (
            program_dir.join("tree").join(path),
            InstallTarget::Tree {
                root: program_dir.clone(),
                program_path: PathBuf::from(path),
            },
        ),
        // A single file, in this Supervisor's own program/ directory (ADR-0021).
        None => {
            let program = program_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            (program.clone(), InstallTarget::Binary(program))
        }
    };
    std::fs::create_dir_all(program.parent().expect("a parent")).expect("program dir");
    std::fs::copy(stub(), &program).expect("put the stub where the install would");

    let (event_tx, events) = mpsc::channel(64);
    let (shutdown_tx, shutdown) = shutdown_channel();
    let ctx = SupervisorContext {
        name: name.to_string(),
        supervisor_dir: dir.path().to_path_buf(),
        config_dir: dir.path().join("config"),
        program,
        install,
        stop_timeout: Duration::from_secs(5),
        apply_grace: Duration::ZERO,
        retain_previous: Duration::ZERO,
        archive_key: None,
        // The whole point: the block is `type` and `name`, so the settings are empty.
        settings: toml::Table::new(),
        events: EventSender::new(0, event_tx),
        shutdown,
    };
    let commands = plugin
        .start(ctx)
        .expect("the adapter starts on an empty block");
    Harness {
        commands,
        events,
        shutdown_tx,
        dir,
    }
}

/// Waits until the agent is **running** and reports its version — the two halves of "it started"
/// that a fleet view shows separately: a pid means the Managed Process is up, and the version means
/// the kind asked it for one without the block naming how.
async fn wait_for_running_version(harness: &mut Harness) -> String {
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
    assert!(
        pid.expect("a pid") > 0,
        "the Managed Process is not running"
    );
    version.expect("a version")
}

async fn apply_config(harness: &mut Harness) -> Result<(), String> {
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
    loop {
        let (_, event) = tokio::time::timeout_at(deadline, harness.events.recv())
            .await
            .expect("an acknowledgement in time")
            .expect("an open channel");
        if let ProcessEvent::ConfigApplied { result, .. } = event {
            return result;
        }
    }
}

async fn stop(mut harness: Harness) {
    harness.shutdown_tx.send(true).expect("shutdown");
    // Drain, so the adapter's task ends before the temp directory is removed under it.
    while harness.events.recv().await.is_some() {}
}

/// Telegraf's whole block: `type` and `name`. The program is a single file found by its own name,
/// the version comes from the kind's own `--version`, and the Configuration the fleet delivers is
/// the one the kind points `--config` at.
#[tokio::test]
async fn a_two_line_telegraf_block_runs_reports_and_applies() {
    let mut harness = start(
        &client::supervisor::telegraf::TelegrafPlugin,
        "telegraf",
        None,
    );
    assert_eq!(
        wait_for_running_version(&mut harness).await,
        "9.9.9",
        "the kind asked the program for its version without the block naming how"
    );

    // The entry the fleet delivers, under the name `opamp-package-fetch` uploads — which is the
    // file name `--config` was built with.
    std::fs::write(harness.config_dir().join("telegraf-conf"), "[agent]\n").expect("the entry");
    assert_eq!(apply_config(&mut harness).await, Ok(()));
    stop(harness).await;
}

/// The GLPI Agent's whole block, likewise — and on both platforms, with the program found at the
/// place inside the tree that this platform's constant names.
#[tokio::test]
async fn a_two_line_glpi_block_runs_reports_and_applies() {
    let program_path = client::supervisor::glpi::GlpiPlugin
        .defaults()
        .program_path
        .expect("the GLPI Agent is a tree package on both platforms");
    let mut harness = start(
        &client::supervisor::glpi::GlpiPlugin,
        "glpi",
        Some(program_path),
    );
    assert_eq!(wait_for_running_version(&mut harness).await, "9.9.9");

    // What the agent writes into, which it does not create itself and exits without: the block
    // says nothing about it, and the host was not prepared for it.
    let state = harness.dir.path().join("agent-state");
    wait_for(&state, "the agent's own state directory").await;
    assert!(state.is_dir());

    std::fs::write(
        harness.config_dir().join("glpi-agent-conf"),
        "tag = fleet\n",
    )
    .expect("the entry");
    assert_eq!(apply_config(&mut harness).await, Ok(()));
    stop(harness).await;
}

async fn wait_for(path: &Path, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what} at {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
