//! The `icinga2` kind as the core drives it (ADR-0068, ADR-0069): the plugin's own adapter, its
//! enrolment task, and the validation gate in front of the shared `Runner`.
//!
//! The unit tests in the module check the pieces; this drives the assembled thing through
//! `Plugin::start`, which is what the Client does — so what is asserted here is behaviour an
//! operator would see in the fleet view: a daemon that does not start while a certificate is
//! missing, an unreachable parent that waits instead of crash-looping, and a configuration Icinga
//! refuses that never reaches the running process.

use std::path::{Path, PathBuf};
use std::time::Duration;

use client::service::runtime::shutdown_channel;
use client::supervisor::icinga2::Icinga2Plugin;
use client::supervisor::ports::{
    EventSender, Plugin, ProcessCommand, ProcessEvent, SupervisorContext,
};
use client::supervisor::process::InstallTarget;
use opamp::proto::AgentRemoteConfig;
use tokio::sync::mpsc;

/// The stub that stands in for `icinga2`, built by the same `cargo test` run.
fn stub() -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(format!("stub_icinga2{}", std::env::consts::EXE_SUFFIX))
}

struct Harness {
    commands: mpsc::Sender<ProcessCommand>,
    events: mpsc::Receiver<(usize, ProcessEvent)>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    dir: tempfile::TempDir,
}

impl Harness {
    fn supervisor_dir(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn config_dir(&self) -> PathBuf {
        self.dir.path().join("config")
    }
}

/// Starts the kind exactly as the core does, with `settings` as the block's plugin-specific keys.
fn start(settings: &str) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("config")).expect("config dir");
    let (event_tx, events) = mpsc::channel(64);
    let (shutdown_tx, shutdown) = shutdown_channel();
    let settings: toml::Table = settings.parse().expect("settings");
    let ctx = SupervisorContext {
        name: "icinga2".to_string(),
        supervisor_dir: dir.path().to_path_buf(),
        config_dir: dir.path().join("config"),
        program: stub(),
        install: InstallTarget::Binary(dir.path().join("program/icinga2")),
        stop_timeout: Duration::from_secs(5),
        apply_grace: Duration::ZERO,
        retain_previous: Duration::ZERO,
        archive_key: None,
        settings,
        events: EventSender::new(0, event_tx),
        shutdown,
    };
    let commands = Icinga2Plugin.start(ctx).expect("the adapter starts");
    Harness {
        commands,
        events,
        shutdown_tx,
        dir,
    }
}

/// Waits for a health event whose status contains `needle`, so a test asserts on what the fleet
/// view would show rather than on timing.
async fn wait_for_health(harness: &mut Harness, needle: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let event = tokio::time::timeout_at(deadline, harness.events.recv())
            .await
            .unwrap_or_else(|_| panic!("no health mentioning {needle:?} in time"))
            .expect("an open channel");
        if let (_, ProcessEvent::Health(health)) = event {
            if health.status.contains(needle) {
                return health.last_error;
            }
        }
    }
}

async fn next_config_ack(harness: &mut Harness) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let event = tokio::time::timeout_at(deadline, harness.events.recv())
            .await
            .expect("an acknowledgement in time")
            .expect("an open channel");
        if let (_, ProcessEvent::ConfigApplied { result, .. }) = event {
            return result;
        }
    }
}

async fn wait_for_file(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn write_config(harness: &Harness, body: &str) {
    std::fs::write(harness.config_dir().join("icinga2-conf"), body).expect("configuration");
}

async fn apply_config(harness: &Harness) {
    harness
        .commands
        .send(ProcessCommand::ApplyConfig {
            config: AgentRemoteConfig {
                config_hash: b"hash".to_vec(),
                ..Default::default()
            },
        })
        .await
        .expect("send");
}

/// The Agent role's gate (ADR-0069): a parent that cannot be reached is a *wait* with a reason, and
/// the daemon stays unstarted. Before this, a Supervisor would have spawned a process that could
/// not do its job and hidden the cause in a restart loop.
#[tokio::test]
async fn an_unreachable_parent_waits_with_a_reason_and_starts_nothing() {
    let mut harness = start(
        r#"
        main_config = "icinga2-conf"
        parent_host = "unreachable.example"
        "#,
    );
    write_config(&harness, "include <itl>\n");

    let reason = wait_for_health(&mut harness, "awaiting the certificate").await;
    assert!(
        reason.contains("Cannot connect to host"),
        "the health carries the parent's own words: {reason}"
    );
    assert!(
        !harness
            .supervisor_dir()
            .join("icinga2-enrolment.json")
            .exists(),
        "nothing is recorded as enrolled"
    );

    harness.shutdown_tx.send(true).expect("shutdown");
}

/// The whole path in one go: enrolment succeeds, the gate opens, and the daemon runs — with the
/// state where ADR-0068 puts it, beside the tree rather than inside it.
#[tokio::test]
async fn enrolment_opens_the_gate_and_the_daemon_starts() {
    let mut harness = start(
        r#"
        main_config = "icinga2-conf"
        parent_host = "master.example"
        node_name = "edge-01"
        "#,
    );
    write_config(&harness, "include <itl>\n");

    let certs = harness.supervisor_dir().join("data/certs");
    wait_for_file(&certs.join("edge-01.crt")).await;
    wait_for_file(&certs.join("ca.crt")).await;
    wait_for_file(&harness.supervisor_dir().join("icinga2-enrolment.json")).await;
    assert!(
        harness
            .supervisor_dir()
            .join("trusted-parent.crt")
            .is_file(),
        "the parent is pinned outside config/, which every apply empties"
    );

    // The Runner reports the process as running once the gate the enrolment opened lets it spawn.
    wait_for_health(&mut harness, "running").await;

    harness.shutdown_tx.send(true).expect("shutdown");
}

/// ADR-0068's validation gate, through the assembled adapter: a configuration Icinga refuses is
/// answered `FAILED` and never reaches the running daemon — which is the only way the fleet can be
/// told the truth about an apply Icinga aborts silently.
#[tokio::test]
async fn a_configuration_icinga_refuses_is_reported_failed() {
    let mut harness = start(
        r#"
        main_config = "icinga2-conf"
        "#,
    );
    write_config(&harness, "include <itl>\n");
    apply_config(&harness).await;
    next_config_ack(&mut harness)
        .await
        .expect("a valid configuration applies");

    write_config(&harness, "object INVALID nonsense\n");
    apply_config(&harness).await;
    let refused = next_config_ack(&mut harness)
        .await
        .expect_err("a configuration Icinga refuses is not applied");
    assert!(
        refused.contains("syntax error"),
        "the refusal carries Icinga's own message: {refused}"
    );

    harness.shutdown_tx.send(true).expect("shutdown");
}

/// A standalone node — no parent — needs no certificate, so its configuration is all it waits for.
#[tokio::test]
async fn a_standalone_node_runs_without_enrolment() {
    let mut harness = start(
        r#"
        main_config = "icinga2-conf"
        "#,
    );
    write_config(&harness, "include <itl>\n");
    apply_config(&harness).await;

    wait_for_health(&mut harness, "running").await;
    assert!(
        !harness
            .supervisor_dir()
            .join("data/certs/icinga2.crt")
            .exists(),
        "no parent, no enrolment"
    );

    harness.shutdown_tx.send(true).expect("shutdown");
}
