//! The `icinga2` kind as the core drives it (ADR-draft-icinga-2): the plugin's own adapter, its
//! enrolment task, and the validation gate in front of the shared `Runner`.
//!
//! The unit tests in the module check the pieces; this drives the assembled thing through
//! `Plugin::start`, which is what the Client does — so what is asserted here is behaviour an
//! operator would see in the fleet view: a daemon that does not start while a certificate is
//! missing, an unreachable parent that waits instead of crash-looping, and a configuration Icinga
//! refuses that never reaches the running process.
//!
//! Not on Windows: the kind refuses to start there until a repacked tree has run on it
//! (ADR-draft-icinga-2 clause 23), which
//! `the_kind_refuses_to_start_on_windows_until_it_is_proven_there` holds.
#![cfg(all(feature = "icinga2", not(windows)))]

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleet_agent::shutdown::shutdown_channel;
use fleet_agent::supervisor::icinga2::Icinga2Plugin;
use fleet_agent::supervisor::ports::InstallTarget;
use fleet_agent::supervisor::ports::{
    EventSender, Plugin, ProcessCommand, ProcessEvent, SupervisorContext,
};
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
        // The core hands every kind a token; the `icinga2` kind declares that its daemon gets none.
        endpoint_token: zeroize::Zeroizing::new("the-token".to_string()),
        name: "icinga2".to_string(),
        supervisor_dir: dir.path().to_path_buf(),
        config_dir: dir.path().join("config"),
        program: stub(),
        install: InstallTarget::binary(dir.path().join("program/icinga2")),
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
    let commands = Icinga2Plugin::default()
        .start(ctx)
        .expect("the adapter starts");
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

/// Delivers Icinga's root configuration the way the fleet does (ADR-draft-icinga-2): the entry, and
/// the role that says it is the root. The block does not name it — the marking is the fleet's.
fn write_config(harness: &Harness, body: &str) {
    std::fs::write(harness.config_dir().join("icinga2-conf"), body).expect("configuration");
    std::fs::write(
        harness
            .config_dir()
            .join(fleet_agent::storage::SUPPLEMENTARY_FILE),
        "icinga2-conf main\n",
    )
    .expect("the role that marks it");
}

async fn apply_config(harness: &Harness) {
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
}

/// The Agent role's gate (ADR-draft-icinga-2): a parent that cannot be reached is a *wait* with a
/// reason, and the daemon stays unstarted. Before this, a Supervisor would have spawned a process
/// that could not do its job and hidden the cause in a restart loop.
/// Verifies: ADR-draft-icinga-2
#[tokio::test]
async fn an_unreachable_parent_waits_with_a_reason_and_starts_nothing() {
    let mut harness = start(
        r#"
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
/// state where ADR-draft-icinga-2 puts it, beside the tree rather than inside it.
/// Verifies: ADR-draft-icinga-2
#[tokio::test]
async fn enrolment_opens_the_gate_and_the_daemon_starts() {
    let mut harness = start(
        r#"
        parent_host = "master.example"
        node_name = "edge-01"
        "#,
    );
    write_config(&harness, "include <itl>\n");

    // No ticket: the parent queues the request, and the Agent waits saying so (ADR-draft-icinga-2
    // clause 12). The stub's `<cert>.approved` stands in for `icinga2 ca sign` on the parent.
    let certs = harness.supervisor_dir().join("data/certs");
    let reason = wait_for_health(&mut harness, "awaiting the certificate").await;
    assert!(reason.contains("icinga2 ca sign"), "{reason}");
    assert!(
        !certs.join("ca.crt").exists(),
        "no CA, so no daemon, while the request waits"
    );
    std::fs::copy(
        certs.join("edge-01.crt.pending"),
        certs.join("edge-01.crt.approved"),
    )
    .expect("the parent signs");

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

/// ADR-draft-icinga-2's validation gate, through the assembled adapter: a configuration Icinga
/// refuses is answered `FAILED` and never reaches the running daemon — which is the only way the
/// fleet can be told the truth about an apply Icinga aborts silently.
/// Verifies: G-4, G-7, ADR-draft-icinga-2
#[tokio::test]
async fn a_configuration_icinga_refuses_is_reported_failed() {
    let mut harness = start(
        r#"
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

/// The refused configuration stays on disk where the fleet wrote it; a restart does not start it,
/// and the health says what the daemon waits for rather than only that it waits (ADR-draft-icinga-2
/// clauses 8 and 9).
/// Verifies: ADR-draft-icinga-2
#[tokio::test]
async fn a_refused_configuration_is_not_started_again_and_the_health_says_why() {
    let mut harness = start(
        r#"
        "#,
    );
    write_config(&harness, "object INVALID nonsense\n");
    apply_config(&harness).await;
    next_config_ack(&mut harness)
        .await
        .expect_err("a configuration Icinga refuses is not applied");

    harness
        .commands
        .send(ProcessCommand::Restart)
        .await
        .expect("send");
    // A wait from before the configuration arrived may come first; it names that instead.
    let reason = loop {
        let reason = wait_for_health(&mut harness, "awaiting configuration").await;
        assert!(!reason.is_empty(), "a wait always says what it waits for");
        if reason.contains("refused") {
            break reason;
        }
    };
    assert!(
        reason.contains("syntax error"),
        "the wait names Icinga's reason: {reason}"
    );

    harness.shutdown_tx.send(true).expect("shutdown");
}

/// A standalone node — no parent — needs no certificate, so its configuration is all it waits for.
/// Verifies: ADR-draft-icinga-2
#[tokio::test]
async fn a_standalone_node_runs_without_enrolment() {
    let mut harness = start(
        r#"
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

/// The daemon is handed no Supervisor Endpoint token: it never connects to the Endpoint, and a
/// token in its environment would reach every check plugin it runs
/// (ADR-draft-client-modes-and-the-gateway clause 28, ADR-draft-icinga-2 clause 1).
/// Verifies: ADR-draft-icinga-2
#[tokio::test]
async fn the_daemon_is_handed_no_endpoint_token() {
    let mut harness = start(
        r#"
        "#,
    );
    write_config(&harness, "include <itl>\n");
    apply_config(&harness).await;
    wait_for_health(&mut harness, "running").await;
    let seen = harness.supervisor_dir().join("data/endpoint-token");
    wait_for_file(&seen).await;
    assert_eq!(
        std::fs::read_to_string(&seen).expect("written"),
        "unset",
        "the variable is not set at all, not even to an empty value"
    );

    harness.shutdown_tx.send(true).expect("shutdown");
}

/// The version probe runs the delivered program with the tree's environment, as the daemon runs:
/// the stub, like the real binary on a host without its bundled Boost, prints nothing without it
/// (ADR-draft-icinga-2 clauses 5 and 16). What the fleet sees is Icinga's version without its
/// packaging revision.
/// Verifies: ADR-draft-icinga-2
#[tokio::test]
async fn the_version_is_probed_with_the_trees_environment() {
    let mut harness = start(
        r#"
        "#,
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let version = loop {
        let event = tokio::time::timeout_at(deadline, harness.events.recv())
            .await
            .expect("a probed version in time")
            .expect("an open channel");
        if let (_, ProcessEvent::Description(description)) = event {
            break description
                .identifying_attributes
                .into_iter()
                .find(|attribute| attribute.key == "service.version")
                .expect("a version attribute");
        }
    };
    assert_eq!(
        version,
        opamp::attributes::string_attr("service.version", "2.14.6")
    );

    harness.shutdown_tx.send(true).expect("shutdown");
}

/// The daemon is stopped as a process group (ADR-draft-icinga-2 clause 10): Icinga's umbrella runs
/// a worker of its own, and when the polite stop is ignored and the Runner escalates to `SIGKILL`,
/// the worker goes with the umbrella instead of being left holding the data directory and the
/// port. The stub stands in for both, and ignores `SIGTERM` as `NodeName=stubborn`.
/// Verifies: ADR-draft-icinga-2
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_stop_ends_the_worker_with_the_daemon() {
    let mut harness = start(
        r#"
        node_name = "stubborn"
        "#,
    );
    write_config(&harness, "include <itl>\n");
    apply_config(&harness).await;
    wait_for_health(&mut harness, "running").await;
    let data = harness.supervisor_dir().join("data");
    wait_for_file(&data.join("worker.pid")).await;
    wait_for_file(&data.join("umbrella.pid")).await;
    let pid = |name: &str| {
        std::fs::read_to_string(data.join(name))
            .expect("pid")
            .trim()
            .parse::<u32>()
            .expect("a pid")
    };
    let (umbrella, worker) = (pid("umbrella.pid"), pid("worker.pid"));
    assert!(
        alive(umbrella) && alive(worker),
        "both run before the stop: umbrella {umbrella} {}, worker {worker} {}",
        alive(umbrella),
        alive(worker)
    );

    harness.shutdown_tx.send(true).expect("shutdown");
    // The harness stops with a 5 s bound, after which the Runner kills the group.
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while alive(umbrella) || alive(worker) {
        assert!(
            std::time::Instant::now() < deadline,
            "umbrella alive: {}, worker alive: {}",
            alive(umbrella),
            alive(worker)
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether `pid` names a process that still runs: one that is gone, or a zombie nobody has reaped
/// yet, does not.
#[cfg(target_os = "linux")]
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        // The state follows the command name, which is parenthesised and may hold spaces.
        stat.rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X")
    })
}
