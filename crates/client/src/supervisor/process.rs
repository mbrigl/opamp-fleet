//! The shared child runner both plugins drive: spawn, watch, restart with backoff, apply a new
//! configuration by respawning, stop gracefully within the budget — plus the one-shot version
//! probe both plugins use to learn a Managed Process's own version.
//!
//! Mirrors the reference `opampsupervisor` (ADR-0011): SIGTERM → bounded wait → kill on Unix,
//! `Child::kill` on Windows (which has no SIGTERM equivalent), and exponential backoff for a
//! process that keeps exiting.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opamp::proto::{AgentDescription, ComponentHealth};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::service::runtime::Shutdown;
use crate::supervisor::ports::{EventSender, ProcessCommand, ProcessEvent};
use crate::transport::Backoff;

/// How a plugin wants its Managed Process invoked, rebuilt whenever the configuration changed.
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub working_dir: Option<PathBuf>,
}

/// The adapter task driving one Managed Process. The plugin supplies `build`: the current
/// [`ProcessSpec`], or `None` while the process should not run (a Collector before any
/// configuration arrived).
pub struct Runner {
    pub name: String,
    pub stop_timeout: Duration,
    /// How long a freshly (re)started process must survive before `ApplyConfig` is acknowledged
    /// (ADR-0011's health-gated acknowledgement); zero acknowledges on start.
    pub apply_grace: Duration,
    /// The Managed Process's binary — what an `ApplyPackage` swap replaces (ADR-0015). `None` for
    /// a plugin with no single swappable binary, which then reports a package `InstallFailed`.
    pub binary: Option<PathBuf>,
    pub events: EventSender,
    pub commands: mpsc::Receiver<ProcessCommand>,
    pub build: Box<dyn Fn() -> Option<ProcessSpec> + Send + Sync>,
}

impl Runner {
    pub async fn run(mut self, mut shutdown: Shutdown) {
        let mut backoff = Backoff::new();
        let mut child = self.spawn_if_due().await;

        loop {
            let exited = async {
                match child.as_mut() {
                    Some(c) => c.wait().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(ProcessCommand::ApplyConfig { config }) => {
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        backoff.reset();
                        child = self.spawn_if_due().await;
                        // Applying means running on the new files — and surviving the apply
                        // grace (ADR-0011's health-gated acknowledgement): a process that exits
                        // right away has rejected its configuration the only way a process can.
                        let mut exited_in_grace = false;
                        let result = match (child.take(), (self.build)().is_some()) {
                            (Some(mut started), _) if !self.apply_grace.is_zero() => {
                                tokio::select! {
                                    status = started.wait() => {
                                        let describe = status
                                            .map(|s| s.to_string())
                                            .unwrap_or_else(|e| format!("wait failed: {e}"));
                                        warn!(supervisor = %self.name, status = %describe, "process exited during the apply grace");
                                        self.events
                                            .send(ProcessEvent::Health(unhealthy(
                                                format!("exited during the apply grace ({describe})"),
                                                describe.clone(),
                                            )))
                                            .await;
                                        exited_in_grace = true;
                                        Err(format!("the process exited during the apply grace ({describe})"))
                                    }
                                    _ = tokio::time::sleep(self.apply_grace) => {
                                        child = Some(started);
                                        Ok(())
                                    }
                                    // Shutting down mid-grace: no acknowledgement — the goodbyes
                                    // carry no status anyway — just stop gracefully on the way out.
                                    _ = shutdown.requested() => {
                                        child = Some(started);
                                        break;
                                    }
                                }
                            }
                            (started @ Some(_), _) => {
                                child = started;
                                Ok(())
                            }
                            (None, false) => Ok(()), // nothing should run; that is the config
                            (None, true) => Err("the process did not start".to_string()),
                        };
                        self.events
                            .send(ProcessEvent::ConfigApplied { hash: config.config_hash, result })
                            .await;
                        if exited_in_grace {
                            // Stay supervised: a flaky-but-valid configuration is retried with
                            // backoff, exactly like any unexpected exit.
                            let delay = backoff.advance();
                            tokio::select! {
                                _ = tokio::time::sleep(delay) => child = self.spawn_if_due().await,
                                _ = shutdown.requested() => break,
                            }
                        }
                    }
                    Some(ProcessCommand::ApplyPackage { staged, version, hash }) => {
                        // Swap the binary, restart, and health-gate on the apply grace — a binary
                        // that will not stay up is rolled back to the bytes it replaced (ADR-0015).
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        backoff.reset();
                        let result = self.swap_and_gate(staged, &version, &mut child, &mut shutdown).await;
                        if child.is_none() && !matches!(result, GraceOutcome::ShuttingDown) {
                            child = self.spawn_if_due().await;
                        }
                        match result {
                            GraceOutcome::Ok => {
                                self.events
                                    .send(ProcessEvent::PackageApplied { hash, result: Ok(version) })
                                    .await;
                            }
                            GraceOutcome::Failed(error) => {
                                self.events
                                    .send(ProcessEvent::PackageApplied { hash, result: Err(error) })
                                    .await;
                                // Stay supervised, exactly as a failed ApplyConfig does.
                                let delay = backoff.advance();
                                tokio::select! {
                                    _ = tokio::time::sleep(delay) => child = self.spawn_if_due().await,
                                    _ = shutdown.requested() => break,
                                }
                            }
                            GraceOutcome::ShuttingDown => break,
                        }
                    }
                    Some(ProcessCommand::Restart) => {
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        backoff.reset();
                        child = self.spawn_if_due().await;
                    }
                    Some(ProcessCommand::Shutdown) | None => break,
                },
                status = exited => {
                    let describe = status
                        .map(|s| s.to_string())
                        .unwrap_or_else(|e| format!("wait failed: {e}"));
                    warn!(supervisor = %self.name, status = %describe, "process exited unexpectedly");
                    child = None;
                    self.events
                        .send(ProcessEvent::Health(unhealthy(
                            format!("exited unexpectedly ({describe})"),
                            describe,
                        )))
                        .await;
                    // Come back with backoff — but stay responsive to commands and shutdown.
                    let delay = backoff.advance();
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => child = self.spawn_if_due().await,
                        _ = shutdown.requested() => break,
                    }
                }
                _ = shutdown.requested() => break,
            }
        }
        stop(&mut child, self.stop_timeout, &self.name).await;
    }

    /// Swaps the staged artifact over the binary, respawns, and health-gates on the apply grace,
    /// rolling the binary back on failure (ADR-0015). The process is already stopped. On success
    /// `child` holds the running process; on failure the previous binary is restored (and the
    /// caller respawns it).
    ///
    /// Everything here moves *files*: the old binary is renamed aside rather than read into
    /// memory, and the artifact is copied in a stream. A program can weigh hundreds of megabytes,
    /// and holding two copies of one in RAM to update it is not a trade this makes.
    async fn swap_and_gate(
        &self,
        staged: PathBuf,
        version: &str,
        child: &mut Option<Child>,
        shutdown: &mut Shutdown,
    ) -> GraceOutcome {
        let Some(binary) = self.binary.clone() else {
            let _ = std::fs::remove_file(&staged);
            return GraceOutcome::Failed(
                "this supervisor manages no single binary to replace".to_string(),
            );
        };
        // Move the binary we are replacing aside — same directory, so the rename is atomic and
        // costs nothing — and it is what a failed package is rolled back from.
        let backup = binary.with_extension("rollback");
        let has_backup = match std::fs::rename(&binary, &backup) {
            Ok(()) => true,
            // No existing binary (first install) — nothing to roll back to.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                let _ = std::fs::remove_file(&staged);
                return GraceOutcome::Failed(format!(
                    "cannot set the current binary {} aside: {e}",
                    binary.display()
                ));
            }
        };
        if let Err(e) = install_executable(&staged, &binary) {
            // Put the old binary back before reporting: the process must not be left with none.
            if has_backup {
                let _ = std::fs::rename(&backup, &binary);
            }
            let _ = std::fs::remove_file(&staged);
            return GraceOutcome::Failed(e);
        }
        let _ = std::fs::remove_file(&staged);
        info!(supervisor = %self.name, version = %version, binary = %binary.display(), "package binary staged; restarting");

        let started = self.spawn_if_due().await;
        let outcome = self.gate(started, child, shutdown).await;
        match (&outcome, has_backup) {
            // Roll the binary back to what ran before, so the next respawn is the old, known one.
            (GraceOutcome::Failed(_), true) => {
                if let Err(e) = std::fs::rename(&backup, &binary) {
                    warn!(supervisor = %self.name, error = %e, "cannot roll the binary back");
                } else {
                    warn!(supervisor = %self.name, "rolled the binary back after a failed package");
                }
            }
            (GraceOutcome::Failed(_), false) => {
                let _ = std::fs::remove_file(&binary);
            }
            // Applied: the previous binary is no longer needed.
            (_, true) => {
                let _ = std::fs::remove_file(&backup);
            }
            (_, false) => {}
        }
        outcome
    }

    /// The apply-grace health gate shared by a package swap: a freshly started process must
    /// survive `apply_grace` to count as applied; exiting within it fails. `child` is left holding
    /// the running process on success.
    async fn gate(
        &self,
        started: Option<Child>,
        child: &mut Option<Child>,
        shutdown: &mut Shutdown,
    ) -> GraceOutcome {
        match started {
            None => GraceOutcome::Failed("the process did not start".to_string()),
            Some(mut proc) if !self.apply_grace.is_zero() => {
                tokio::select! {
                    status = proc.wait() => {
                        let describe = status
                            .map(|s| s.to_string())
                            .unwrap_or_else(|e| format!("wait failed: {e}"));
                        warn!(supervisor = %self.name, status = %describe, "process exited during the apply grace");
                        self.events
                            .send(ProcessEvent::Health(unhealthy(
                                format!("exited during the apply grace ({describe})"),
                                describe.clone(),
                            )))
                            .await;
                        GraceOutcome::Failed(format!("the process exited during the apply grace ({describe})"))
                    }
                    _ = tokio::time::sleep(self.apply_grace) => {
                        *child = Some(proc);
                        GraceOutcome::Ok
                    }
                    _ = shutdown.requested() => {
                        *child = Some(proc);
                        GraceOutcome::ShuttingDown
                    }
                }
            }
            Some(proc) => {
                *child = Some(proc);
                GraceOutcome::Ok
            }
        }
    }

    /// Spawns when the plugin says something should run, reporting health either way.
    async fn spawn_if_due(&self) -> Option<Child> {
        let Some(spec) = (self.build)() else {
            self.events
                .send(ProcessEvent::Health(unhealthy(
                    "awaiting configuration".to_string(),
                    String::new(),
                )))
                .await;
            return None;
        };
        let mut command = Command::new(&spec.program);
        command.args(&spec.args).envs(spec.env.iter().cloned());
        if let Some(dir) = &spec.working_dir {
            command.current_dir(dir);
        }
        // If the runner is dropped without a graceful stop, take the process along.
        command.kill_on_drop(true);
        match command.spawn() {
            Ok(child) => {
                info!(supervisor = %self.name, program = %spec.program.display(), "process started");
                self.events
                    .send(ProcessEvent::Health(ComponentHealth {
                        healthy: true,
                        status: "running".to_string(),
                        start_time_unix_nano: now_ns(),
                        status_time_unix_nano: now_ns(),
                        ..Default::default()
                    }))
                    .await;
                Some(child)
            }
            Err(e) => {
                warn!(supervisor = %self.name, program = %spec.program.display(), error = %e, "cannot spawn");
                self.events
                    .send(ProcessEvent::Health(unhealthy(
                        "spawn failed".to_string(),
                        format!("cannot spawn {}: {e}", spec.program.display()),
                    )))
                    .await;
                None
            }
        }
    }
}

/// How long a version probe may take before it is abandoned — it must never stall startup.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Runs `<program> <args>` once and reports the Managed Process's version as a Description
/// event, if the output contains one. Best effort by design: a missing binary, a hang, or
/// versionless output is logged and otherwise ignored — probing must never break supervision.
/// A later self-report through the Supervisor Endpoint replaces the probed value.
pub async fn probe_version(program: PathBuf, args: Vec<String>, events: EventSender) {
    let mut command = Command::new(&program);
    command.args(&args).kill_on_drop(true);
    let output = match tokio::time::timeout(PROBE_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            warn!(program = %program.display(), error = %e, "version probe cannot run");
            return;
        }
        Err(_) => {
            warn!(program = %program.display(), "version probe timed out");
            return;
        }
    };
    // Some tools print their version to stderr; accept either stream.
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match find_semver(&text) {
        Some(version) => {
            info!(program = %program.display(), version = %version, "version probed");
            events
                .send(ProcessEvent::Description(AgentDescription {
                    identifying_attributes: vec![opamp::proto::KeyValue {
                        key: "service.version".to_string(),
                        value: Some(opamp::proto::AnyValue {
                            value: Some(opamp::proto::any_value::Value::StringValue(version)),
                        }),
                    }],
                    non_identifying_attributes: Vec::new(),
                }))
                .await;
        }
        None => {
            warn!(program = %program.display(), "version probe output contains no semantic version")
        }
    }
}

/// The first Semantic Versioning 2.0.0 version found in free-form text (e.g. the `1.2.3` in
/// "otelcol-contrib version 1.2.3"). A leading `v` and trailing punctuation around a token are
/// tolerated; the extracted version itself is strictly SemVer.
fn find_semver(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|token| {
        let token = token.strip_prefix(['v', 'V']).unwrap_or(token);
        let token = token.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        is_semver(token).then(|| token.to_string())
    })
}

/// Strict SemVer 2.0.0: `MAJOR.MINOR.PATCH`, optional `-prerelease`, optional `+build`.
fn is_semver(s: &str) -> bool {
    let (rest, build) = match s.split_once('+') {
        Some((rest, build)) => (rest, Some(build)),
        None => (s, None),
    };
    let (core, prerelease) = match rest.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (rest, None),
    };
    let numeric = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let identifier = |part: &str| {
        !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    };
    let core_parts: Vec<&str> = core.split('.').collect();
    if core_parts.len() != 3 || !core_parts.into_iter().all(numeric) {
        return false;
    }
    if let Some(prerelease) = prerelease {
        // A numeric prerelease identifier must not have leading zeros (SemVer 2.0.0 §9).
        let valid = |part: &str| {
            identifier(part) && (!part.bytes().all(|b| b.is_ascii_digit()) || numeric(part))
        };
        if !prerelease.split('.').all(valid) {
            return false;
        }
    }
    match build {
        Some(build) => build.split('.').all(identifier),
        None => true,
    }
}

/// Graceful stop: SIGTERM and a bounded wait on Unix, then (or on Windows, directly) kill.
async fn stop(child: &mut Option<Child>, timeout: Duration, name: &str) {
    let Some(mut c) = child.take() else {
        return;
    };
    #[cfg(unix)]
    if let Some(pid) = c.id() {
        // SAFETY: plain kill(2) on the child's pid; no memory is touched.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        if tokio::time::timeout(timeout, c.wait()).await.is_ok() {
            info!(supervisor = %name, "process stopped");
            return;
        }
        warn!(supervisor = %name, "process ignored SIGTERM; killing it");
    }
    #[cfg(not(unix))]
    let _ = timeout; // Windows has no SIGTERM equivalent: kill is the stop.
    let _ = c.kill().await;
    info!(supervisor = %name, "process stopped");
}

/// The result of health-gating a freshly (re)started process.
enum GraceOutcome {
    /// The process survived the grace (or the grace is zero) — applied.
    Ok,
    /// The process exited within the grace, or would not start — with the reason.
    Failed(String),
    /// A shutdown was requested mid-grace; the caller stops without an acknowledgement.
    ShuttingDown,
}

/// Writes `bytes` to `path` atomically (temp + rename) and marks it executable on Unix — the
/// binary swap a package apply performs (ADR-0015). A rename is atomic within a directory, so a
/// crash mid-write never leaves a half-written binary in place.
/// Installs a downloaded artifact as `path`: copied in a stream (the download lives in the state
/// directory, which may be on another filesystem, so this cannot be a rename), made executable,
/// then renamed into place — the rename being what makes the swap atomic.
fn install_executable(artifact: &std::path::Path, path: &std::path::Path) -> Result<(), String> {
    let temp = path.with_extension("staged");
    let mut source = std::fs::File::open(artifact)
        .map_err(|e| format!("cannot read {}: {e}", artifact.display()))?;
    let mut target = std::fs::File::create(&temp)
        .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
    std::io::copy(&mut source, &mut target)
        .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
    drop(target);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", temp.display()))?;
    }
    std::fs::rename(&temp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

fn unhealthy(status: String, last_error: String) -> ComponentHealth {
    ComponentHealth {
        healthy: false,
        status,
        last_error,
        status_time_unix_nano: now_ns(),
        ..Default::default()
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::service::runtime::shutdown_channel;
    use crate::supervisor::ports::EventSender;
    use opamp::proto::AgentRemoteConfig;
    use std::os::unix::fs::PermissionsExt;

    fn sh(script: &str) -> ProcessSpec {
        ProcessSpec {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".to_string(), script.to_string()],
            env: Vec::new(),
            working_dir: None,
        }
    }

    struct Harness {
        commands: mpsc::Sender<ProcessCommand>,
        events: mpsc::Receiver<(usize, ProcessEvent)>,
        shutdown_tx: tokio::sync::watch::Sender<bool>,
        task: tokio::task::JoinHandle<()>,
    }

    fn start(build: impl Fn() -> Option<ProcessSpec> + Send + Sync + 'static) -> Harness {
        // Zero grace: the pre-grace instant acknowledgement most tests exercise.
        start_with_grace(Duration::ZERO, build)
    }

    fn start_with_grace(
        apply_grace: Duration,
        build: impl Fn() -> Option<ProcessSpec> + Send + Sync + 'static,
    ) -> Harness {
        let (event_tx, events) = mpsc::channel(64);
        let (commands, command_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown) = shutdown_channel();
        let runner = Runner {
            name: "test".to_string(),
            stop_timeout: Duration::from_secs(5),
            apply_grace,
            binary: None,
            events: EventSender::new(0, event_tx),
            commands: command_rx,
            build: Box::new(build),
        };
        let task = tokio::spawn(runner.run(shutdown));
        Harness {
            commands,
            events,
            shutdown_tx,
            task,
        }
    }

    async fn next_health(events: &mut mpsc::Receiver<(usize, ProcessEvent)>) -> ComponentHealth {
        loop {
            let (_, event) = tokio::time::timeout(Duration::from_secs(10), events.recv())
                .await
                .expect("an event in time")
                .expect("an open channel");
            if let ProcessEvent::Health(health) = event {
                return health;
            }
        }
    }

    #[test]
    fn find_semver_extracts_the_first_strict_version_from_free_text() {
        // The shapes real tools print.
        assert_eq!(
            find_semver("otelcol-contrib version 0.114.0").as_deref(),
            Some("0.114.0")
        );
        assert_eq!(find_semver("thing v1.2.3,").as_deref(), Some("1.2.3"));
        assert_eq!(
            find_semver("agent 2.0.0-rc.1+build.5 (linux/amd64)").as_deref(),
            Some("2.0.0-rc.1+build.5")
        );
        // The first version wins.
        assert_eq!(
            find_semver("v1.0.0 (protocol 3.4.5)").as_deref(),
            Some("1.0.0")
        );
        // A dangling separator counts as trailing punctuation around a valid core.
        assert_eq!(find_semver("version 1.2.3-").as_deref(), Some("1.2.3"));
        // Not SemVer 2: too few parts, leading zeros, invalid prerelease identifiers.
        assert_eq!(find_semver("version 1.2"), None);
        assert_eq!(find_semver("version 01.2.3"), None);
        assert_eq!(find_semver("version 1.2.3-rc.01"), None);
        assert_eq!(find_semver("no version at all"), None);
    }

    #[tokio::test]
    async fn the_probe_reports_a_version_description() {
        let (event_tx, mut events) = mpsc::channel(4);
        probe_version(
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), "echo tool version 3.2.1".to_string()],
            EventSender::new(0, event_tx),
        )
        .await;
        let (_, event) = events.recv().await.expect("a probed description");
        let ProcessEvent::Description(description) = event else {
            panic!("expected a Description event, got {event:?}");
        };
        assert_eq!(description.identifying_attributes[0].key, "service.version");
    }

    #[tokio::test]
    async fn a_failing_or_versionless_probe_stays_silent() {
        let (event_tx, mut events) = mpsc::channel(4);
        probe_version(
            PathBuf::from("/nonexistent/definitely-not-here"),
            vec![],
            EventSender::new(0, event_tx.clone()),
        )
        .await;
        probe_version(
            PathBuf::from("/bin/sh"),
            vec!["-c".to_string(), "echo no version here".to_string()],
            EventSender::new(0, event_tx),
        )
        .await;
        assert!(
            events.try_recv().is_err(),
            "neither probe may emit an event"
        );
    }

    #[tokio::test]
    async fn a_long_running_process_reports_healthy_and_stops_on_shutdown() {
        let mut harness = start(|| Some(sh("sleep 600")));
        let health = next_health(&mut harness.events).await;
        assert!(health.healthy);
        harness.shutdown_tx.send(true).expect("signal shutdown");
        tokio::time::timeout(Duration::from_secs(10), harness.task)
            .await
            .expect("the runner exits in time")
            .expect("no panic");
    }

    #[tokio::test]
    async fn an_exiting_process_turns_unhealthy_and_is_restarted() {
        let mut harness = start(|| Some(sh("exit 3")));
        let first = next_health(&mut harness.events).await;
        assert!(first.healthy, "the spawn itself succeeds");
        let exited = next_health(&mut harness.events).await;
        assert!(!exited.healthy);
        assert!(exited.status.contains("exited unexpectedly"));
        // The watchdog respawns (backoff starts at one second).
        let respawned = next_health(&mut harness.events).await;
        assert!(respawned.healthy);
        harness.shutdown_tx.send(true).expect("signal shutdown");
        let _ = harness.task.await;
    }

    #[tokio::test]
    async fn a_spawn_failure_is_reported_not_fatal() {
        let mut harness = start(|| {
            Some(ProcessSpec {
                program: PathBuf::from("/nonexistent/definitely-not-here"),
                args: Vec::new(),
                env: Vec::new(),
                working_dir: None,
            })
        });
        let health = next_health(&mut harness.events).await;
        assert!(!health.healthy);
        assert_eq!(health.status, "spawn failed");
        harness.shutdown_tx.send(true).expect("signal shutdown");
        let _ = harness.task.await;
    }

    #[tokio::test]
    async fn nothing_to_run_reports_awaiting_configuration() {
        let mut harness = start(|| None);
        let health = next_health(&mut harness.events).await;
        assert!(!health.healthy);
        assert_eq!(health.status, "awaiting configuration");
        harness.shutdown_tx.send(true).expect("signal shutdown");
        let _ = harness.task.await;
    }

    async fn apply(harness: &Harness, hash: &[u8]) {
        harness
            .commands
            .send(ProcessCommand::ApplyConfig {
                config: AgentRemoteConfig {
                    config_hash: hash.to_vec(),
                    ..Default::default()
                },
            })
            .await
            .expect("send the command");
    }

    async fn next_ack(
        events: &mut mpsc::Receiver<(usize, ProcessEvent)>,
    ) -> (Vec<u8>, Result<(), String>) {
        loop {
            let (_, event) = tokio::time::timeout(Duration::from_secs(10), events.recv())
                .await
                .expect("an event in time")
                .expect("an open channel");
            if let ProcessEvent::ConfigApplied { hash, result } = event {
                return (hash, result);
            }
        }
    }

    #[tokio::test]
    async fn a_process_surviving_the_apply_grace_is_acknowledged_applied() {
        let mut harness = start_with_grace(Duration::from_millis(200), || Some(sh("sleep 600")));
        apply(&harness, b"h1").await;
        let (hash, result) = next_ack(&mut harness.events).await;
        assert_eq!(hash, b"h1".to_vec());
        assert!(result.is_ok(), "survived the grace: {result:?}");
        harness.shutdown_tx.send(true).expect("signal shutdown");
        let _ = harness.task.await;
    }

    #[tokio::test]
    async fn a_process_exiting_within_the_grace_fails_the_apply_and_stays_supervised() {
        let mut harness = start_with_grace(Duration::from_millis(500), || Some(sh("exit 3")));
        apply(&harness, b"h1").await;
        let (hash, result) = next_ack(&mut harness.events).await;
        assert_eq!(hash, b"h1".to_vec());
        let error = result.expect_err("the exit within the grace fails the apply");
        assert!(error.contains("apply grace"), "{error}");
        // The watchdog keeps trying with backoff — the process is not abandoned.
        let respawned = next_health(&mut harness.events).await;
        assert!(respawned.healthy, "the backoff respawn happened");
        harness.shutdown_tx.send(true).expect("signal shutdown");
        let _ = harness.task.await;
    }

    #[tokio::test]
    async fn shutdown_during_the_grace_stops_promptly_without_an_ack() {
        let mut harness = start_with_grace(Duration::from_secs(600), || Some(sh("sleep 600")));
        apply(&harness, b"h1").await;
        // The spawn health event arrives; then the runner sits in the grace.
        let started = next_health(&mut harness.events).await;
        assert!(started.healthy);
        harness.shutdown_tx.send(true).expect("signal shutdown");
        tokio::time::timeout(Duration::from_secs(10), harness.task)
            .await
            .expect("the runner exits in time despite the long grace")
            .expect("no panic");
        // No ConfigApplied was ever emitted.
        while let Ok((_, event)) = harness.events.try_recv() {
            assert!(
                !matches!(event, ProcessEvent::ConfigApplied { .. }),
                "no acknowledgement during shutdown"
            );
        }
    }

    #[tokio::test]
    async fn a_restart_command_cycles_the_process_without_a_config_ack() {
        let mut harness = start(|| Some(sh("sleep 600")));
        let first = next_health(&mut harness.events).await;
        assert!(first.healthy);

        harness
            .commands
            .send(ProcessCommand::Restart)
            .await
            .expect("send the restart");

        // The respawned process reports healthy again — and nothing acknowledges a config,
        // because none changed.
        let respawned = next_health(&mut harness.events).await;
        assert!(respawned.healthy);
        assert!(
            harness.events.try_recv().is_err(),
            "a restart must not emit a ConfigApplied"
        );
        harness.shutdown_tx.send(true).expect("signal shutdown");
        let _ = harness.task.await;
    }

    async fn next_package_ack(
        events: &mut mpsc::Receiver<(usize, ProcessEvent)>,
    ) -> (Vec<u8>, Result<String, String>) {
        loop {
            let (_, event) = tokio::time::timeout(Duration::from_secs(10), events.recv())
                .await
                .expect("an event in time")
                .expect("an open channel");
            if let ProcessEvent::PackageApplied { hash, result } = event {
                return (hash, result);
            }
        }
    }

    #[tokio::test]
    async fn apply_package_swaps_the_binary_and_acknowledges_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("agent");
        // The "old" binary: a script that sleeps (stays up).
        std::fs::write(&binary, "#!/bin/sh\nexec sleep 600\n").expect("write");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let run_binary = binary.clone();
        let (event_tx, events) = mpsc::channel(64);
        let (commands, command_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown) = shutdown_channel();
        let runner = Runner {
            name: "test".to_string(),
            stop_timeout: Duration::from_secs(5),
            apply_grace: Duration::from_millis(200),
            binary: Some(binary.clone()),
            events: EventSender::new(0, event_tx),
            commands: command_rx,
            build: Box::new(move || {
                Some(ProcessSpec {
                    program: run_binary.clone(),
                    args: Vec::new(),
                    env: Vec::new(),
                    working_dir: None,
                })
            }),
        };
        let task = tokio::spawn(runner.run(shutdown));
        let mut harness = Harness {
            commands,
            events,
            shutdown_tx,
            task,
        };
        let _ = next_health(&mut harness.events).await; // initial spawn

        // A new binary that also stays up — it must survive the grace and be acknowledged. It
        // arrives as a downloaded *file*, the way the transport stages one.
        let new_bytes = b"#!/bin/sh\nexec sleep 600\n".to_vec();
        let staged = dir.path().join("downloaded.staged");
        std::fs::write(&staged, &new_bytes).expect("stage");
        harness
            .commands
            .send(ProcessCommand::ApplyPackage {
                staged: staged.clone(),
                version: "2.0.0".to_string(),
                hash: b"pkg-hash".to_vec(),
            })
            .await
            .expect("send");
        let (hash, result) = next_package_ack(&mut harness.events).await;
        assert_eq!(hash, b"pkg-hash".to_vec());
        assert_eq!(result, Ok("2.0.0".to_string()));
        // The binary on disk is the swapped one, and the staged download is cleaned up.
        assert_eq!(std::fs::read(&binary).expect("read"), new_bytes);
        assert!(!staged.exists(), "the staged artifact is not left behind");
        assert!(
            !binary.with_extension("rollback").exists(),
            "a succeeded install keeps no backup"
        );

        harness.shutdown_tx.send(true).expect("shutdown");
        let _ = harness.task.await;
    }

    #[tokio::test]
    async fn a_package_that_will_not_stay_up_is_rolled_back_and_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("agent");
        let good = b"#!/bin/sh\nexec sleep 600\n".to_vec();
        std::fs::write(&binary, &good).expect("write");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let run_binary = binary.clone();
        let (event_tx, events) = mpsc::channel(64);
        let (commands, command_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown) = shutdown_channel();
        let runner = Runner {
            name: "test".to_string(),
            stop_timeout: Duration::from_secs(5),
            apply_grace: Duration::from_millis(500),
            binary: Some(binary.clone()),
            events: EventSender::new(0, event_tx),
            commands: command_rx,
            build: Box::new(move || {
                Some(ProcessSpec {
                    program: run_binary.clone(),
                    args: Vec::new(),
                    env: Vec::new(),
                    working_dir: None,
                })
            }),
        };
        let task = tokio::spawn(runner.run(shutdown));
        let mut harness = Harness {
            commands,
            events,
            shutdown_tx,
            task,
        };
        let _ = next_health(&mut harness.events).await;

        // A binary that exits at once: it fails the grace and must be rolled back.
        let staged = dir.path().join("downloaded.staged");
        std::fs::write(&staged, b"#!/bin/sh\nexit 1\n").expect("stage");
        harness
            .commands
            .send(ProcessCommand::ApplyPackage {
                staged,
                version: "9.9.9".to_string(),
                hash: b"bad-hash".to_vec(),
            })
            .await
            .expect("send");
        let (hash, result) = next_package_ack(&mut harness.events).await;
        assert_eq!(hash, b"bad-hash".to_vec());
        assert!(result.is_err(), "a binary that exits fails the install");
        // The binary on disk is the original one again.
        assert_eq!(std::fs::read(&binary).expect("read"), good, "rolled back");

        harness.shutdown_tx.send(true).expect("shutdown");
        let _ = harness.task.await;
    }

    #[tokio::test]
    async fn apply_config_restarts_and_acknowledges() {
        let mut harness = start(|| Some(sh("sleep 600")));
        let _ = next_health(&mut harness.events).await;

        harness
            .commands
            .send(ProcessCommand::ApplyConfig {
                config: AgentRemoteConfig {
                    config_hash: b"h1".to_vec(),
                    ..Default::default()
                },
            })
            .await
            .expect("send the command");

        // Restart health, then the acknowledgement.
        let mut acked = false;
        for _ in 0..4 {
            let (_, event) = tokio::time::timeout(Duration::from_secs(10), harness.events.recv())
                .await
                .expect("an event in time")
                .expect("an open channel");
            if let ProcessEvent::ConfigApplied { hash, result } = event {
                assert_eq!(hash, b"h1".to_vec());
                assert!(result.is_ok());
                acked = true;
                break;
            }
        }
        assert!(acked, "ApplyConfig must be acknowledged");
        harness.shutdown_tx.send(true).expect("signal shutdown");
        let _ = harness.task.await;
    }
}
