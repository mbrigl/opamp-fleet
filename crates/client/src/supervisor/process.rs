//! The shared child runner both plugins drive: spawn, watch, restart with backoff, apply a new
//! configuration by respawning, stop gracefully within the budget — plus the version probe both
//! plugins use to learn a Managed Process's own version, run at startup and after every swap.
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

/// What a package replaces on disk.
///
/// The two shapes share their whole lifecycle — set the old one aside, install the new one, prove
/// it starts, put the old one back if it does not — and differ only in what "it" is. Keeping that
/// difference here rather than inside [`Runner::swap_and_gate`] is what lets the health gate and
/// the rollback stay one piece of code for both (ADR-0015, ADR-0023).
#[derive(Debug, Clone)]
pub enum InstallTarget {
    /// One file: the artifact is the program, or holds it as its single named member.
    Binary(PathBuf),
    /// A whole directory tree, unpacked beside the running one and swapped by renaming
    /// directories — the same move the single-file case makes, one level up.
    Tree {
        /// This Supervisor's `program/` directory, which holds the live tree and the rolled-back
        /// one under fixed names.
        root: PathBuf,
        /// Where the program sits inside the tree, as written in the configuration.
        program_path: PathBuf,
    },
}

impl InstallTarget {
    /// What the Managed Process is spawned from.
    fn live(&self) -> PathBuf {
        match self {
            InstallTarget::Binary(path) => path.clone(),
            InstallTarget::Tree { root, .. } => root.join(crate::config::TREE_DIR),
        }
    }

    /// Where the thing being replaced is kept until the new one has proved itself.
    fn backup(&self) -> PathBuf {
        match self {
            InstallTarget::Binary(path) => path.with_extension("rollback"),
            InstallTarget::Tree { root, .. } => {
                root.join(format!("{}.rollback", crate::config::TREE_DIR))
            }
        }
    }

    /// Creates the directories this target needs before anything is installed into it — at
    /// startup, so a Supervisor whose program has not arrived yet still owns its place.
    ///
    /// For a tree that is the `program/` root and **nothing below it**: the live tree is put there
    /// by renaming a staging directory over the name, and a rename cannot replace a directory that
    /// something else has already created and filled. Creating the program's parent — which is what
    /// the single-file case wants — would make every first install of a tree fail.
    ///
    /// # Errors
    /// Returns an error when the directory cannot be created.
    pub fn prepare(&self) -> Result<(), String> {
        let dir = match self {
            InstallTarget::Binary(path) => path.parent().map(std::path::Path::to_path_buf),
            InstallTarget::Tree { root, .. } => Some(root.clone()),
        };
        match dir {
            Some(dir) => std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot prepare {}: {e}", dir.display())),
            None => Ok(()),
        }
    }

    /// Moves what runs today aside, so a failed install has something to go back to. `false` means
    /// there was nothing there — a first install, which is the ordinary way an agent arrives.
    fn set_aside(&self) -> Result<bool, String> {
        let (live, backup) = (self.live(), self.backup());
        // A rename never lands on an occupied name: Windows refuses it outright, and a directory
        // rename would nest rather than replace.
        self.remove(&backup);
        match std::fs::rename(&live, &backup) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("cannot set {} aside: {e}", live.display())),
        }
    }

    /// Installs the verified artifact as what runs next. The live path is free by the time this is
    /// called — [`set_aside`](Self::set_aside) has just moved it.
    fn install(&self, staged: &std::path::Path, archive_key: Option<&str>) -> Result<(), String> {
        match self {
            InstallTarget::Binary(path) => install_executable(staged, path, archive_key),
            InstallTarget::Tree { root, program_path } => {
                install_tree(staged, root, program_path, archive_key)
            }
        }
    }

    /// Puts back what ran before.
    fn restore(&self) -> Result<(), String> {
        let (live, backup) = (self.live(), self.backup());
        self.remove(&live);
        std::fs::rename(&backup, &live)
            .map_err(|e| format!("cannot restore {}: {e}", live.display()))
    }

    /// Throws away what was just installed — a failed *first* install, with nothing behind it.
    fn discard(&self) {
        self.remove(&self.live());
    }

    /// Drops the backup once the new version has proved it stays up.
    fn drop_backup(&self) {
        self.remove(&self.backup());
    }

    /// Removes a file or a whole tree, whichever this target deals in. Best-effort throughout:
    /// every caller is already committing to an outcome and has nothing better to do with a
    /// failure than report the one it is already reporting.
    fn remove(&self, path: &std::path::Path) {
        match self {
            InstallTarget::Binary(_) => {
                let _ = std::fs::remove_file(path);
            }
            InstallTarget::Tree { .. } => {
                let _ = std::fs::remove_dir_all(path);
            }
        }
    }
}

/// How to ask a Managed Process for its own version — the program to run and the arguments that
/// make it print one. `None` on a Runner whose plugin has no such convention.
pub struct VersionProbe {
    pub program: PathBuf,
    pub args: Vec<String>,
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
    /// What an `ApplyPackage` swap replaces (ADR-0015) — one file, or a whole tree (ADR-0023).
    /// `None` for a plugin with nothing swappable, which then reports a package `InstallFailed`.
    pub install: Option<InstallTarget>,
    /// Opens an encrypted `.7z` artifact (ADR-0018); `None` when no key is configured.
    pub archive_key: Option<String>,
    /// How to learn the Managed Process's own version, when the plugin knows how to ask.
    pub version_probe: Option<VersionProbe>,
    pub events: EventSender,
    pub commands: mpsc::Receiver<ProcessCommand>,
    pub build: Box<dyn Fn() -> Option<ProcessSpec> + Send + Sync>,
}

impl Runner {
    pub async fn run(mut self, mut shutdown: Shutdown) {
        let mut backoff = Backoff::new();
        // What runs today, before it runs: a Collector without the opampextension never reports
        // its own version, and one that is not configured yet never even starts.
        self.probe_version();
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
                                // A new binary is a new version, and the Agent's `service.version`
                                // is what the program says about itself — so ask it again. Without
                                // this the swap is reported as installed while the Agent goes on
                                // describing the version it replaced, until the Client restarts.
                                self.probe_version();
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
    /// memory, and the artifact is moved or streamed rather than loaded. A program can weigh
    /// hundreds of megabytes, and holding two copies of one in RAM to update it is not a trade
    /// this makes. The artifact may already be gone by the time it is cleaned up —
    /// [`install_executable`] moves it when it can — so every removal of it is best-effort.
    async fn swap_and_gate(
        &self,
        staged: PathBuf,
        version: &str,
        child: &mut Option<Child>,
        shutdown: &mut Shutdown,
    ) -> GraceOutcome {
        let Some(target) = self.install.clone() else {
            let _ = std::fs::remove_file(&staged);
            return GraceOutcome::Failed(
                "this supervisor manages nothing a package can replace".to_string(),
            );
        };
        // Move what runs today aside — a rename, so it is atomic and costs nothing — and it is
        // what a failed package is rolled back from.
        let has_backup = match target.set_aside() {
            Ok(has_backup) => has_backup,
            Err(e) => {
                let _ = std::fs::remove_file(&staged);
                return GraceOutcome::Failed(e);
            }
        };
        if let Err(e) = target.install(&staged, self.archive_key.as_deref()) {
            // Put the old one back before reporting: the process must not be left with none.
            if has_backup {
                let _ = target.restore();
            }
            let _ = std::fs::remove_file(&staged);
            return GraceOutcome::Failed(e);
        }
        let _ = std::fs::remove_file(&staged);
        info!(supervisor = %self.name, version = %version, program = %target.live().display(), "package staged; restarting");

        let started = self.try_spawn().await;
        // "Nothing started" has two meanings, and only one of them is a failed install. A plugin
        // with no process to run — a Collector that has not been configured yet — has not rejected
        // the artifact: the binary is in place and will be started by the configuration when it
        // arrives. `ApplyConfig` has always drawn this distinction; the package path must too, or
        // installing onto a host that is not yet configured deletes what it just installed.
        let nothing_to_run = started.is_err() && (self.build)().is_none();
        match (&started, nothing_to_run) {
            (Err(_), true) => {
                info!(supervisor = %self.name, version = %version, "package installed; nothing to run until a configuration arrives");
            }
            (Err(e), false) => {
                warn!(supervisor = %self.name, error = %e, "the new binary would not start");
            }
            _ => {}
        }
        let outcome = if nothing_to_run {
            GraceOutcome::Ok
        } else {
            self.gate(started.ok(), child, shutdown).await
        };
        match (&outcome, has_backup) {
            // Roll back to what ran before, so the next respawn is the old, known one.
            (GraceOutcome::Failed(_), true) => {
                if let Err(e) = target.restore() {
                    warn!(supervisor = %self.name, error = %e, "cannot roll the program back");
                } else {
                    warn!(supervisor = %self.name, "rolled the program back after a failed package");
                }
            }
            (GraceOutcome::Failed(_), false) => target.discard(),
            // Applied: what ran before is no longer needed.
            (_, true) => target.drop_backup(),
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

    /// Asks the program for its own version, if the plugin knows how to ask.
    ///
    /// Out of band by design: the answer arrives whenever it arrives, as a Description event, and
    /// a program that will not answer holds nothing up (see [`probe_version`]).
    fn probe_version(&self) {
        if let Some(probe) = &self.version_probe {
            tokio::spawn(probe_version(
                probe.program.clone(),
                probe.args.clone(),
                self.events.clone(),
            ));
        }
    }

    /// Spawns when the plugin says something should run, reporting health either way.
    async fn spawn_if_due(&self) -> Option<Child> {
        self.try_spawn().await.ok()
    }

    /// Spawns the Managed Process, keeping the reason when it fails.
    ///
    /// Right after a package swap the reason matters: exec of a freshly written binary can fail
    /// with `ETXTBSY` — "Text file busy" — when another thread of this Client forked for its own
    /// spawn while this one still held the new file open for writing. The forked child inherits
    /// that descriptor until it execs, and the kernel refuses to exec a file anyone holds open for
    /// writing. It is transient and says nothing about the artifact, so the swap retries briefly
    /// rather than rolling back a binary that is perfectly good.
    async fn try_spawn(&self) -> Result<Child, String> {
        const BUSY_RETRIES: u32 = 10;
        const BUSY_DELAY: Duration = Duration::from_millis(50);

        let mut attempt = 0;
        loop {
            match self.spawn_once().await {
                Err(e) if is_text_file_busy(&e) && attempt < BUSY_RETRIES => {
                    attempt += 1;
                    warn!(
                        supervisor = %self.name, attempt,
                        "the new binary is momentarily busy (another spawn holds it); retrying"
                    );
                    tokio::time::sleep(BUSY_DELAY).await;
                }
                other => return other.map_err(|e| e.to_string()),
            }
        }
    }

    async fn spawn_once(&self) -> Result<Child, std::io::Error> {
        let Some(spec) = (self.build)() else {
            self.events
                .send(ProcessEvent::Health(unhealthy(
                    "awaiting configuration".to_string(),
                    String::new(),
                )))
                .await;
            return Err(std::io::Error::other("nothing to run"));
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
                // Before the health report, so a sampler that wakes on it already has the pid.
                self.events.send(ProcessEvent::Pid(child.id())).await;
                self.events
                    .send(ProcessEvent::Health(ComponentHealth {
                        healthy: true,
                        status: "running".to_string(),
                        start_time_unix_nano: now_ns(),
                        status_time_unix_nano: now_ns(),
                        ..Default::default()
                    }))
                    .await;
                Ok(child)
            }
            Err(e) => {
                warn!(supervisor = %self.name, program = %spec.program.display(), error = %e, "cannot spawn");
                // What the Server should read is the *situation*, not the syscall. A binary that
                // is not there — a first install that failed and was undone, or a program never
                // installed — is a Supervisor with no process, and saying so is more use to an
                // operator than "spawn failed".
                let status = if e.kind() == std::io::ErrorKind::NotFound {
                    "no process installed"
                } else {
                    "spawn failed"
                };
                self.events
                    .send(ProcessEvent::Health(unhealthy(
                        status.to_string(),
                        format!("cannot spawn {}: {e}", spec.program.display()),
                    )))
                    .await;
                Err(e)
            }
        }
    }
}

/// `ETXTBSY`: the file cannot be executed because someone holds it open for writing.
fn is_text_file_busy(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
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
                    identifying_attributes: vec![opamp::attributes::string_attr(
                        opamp::attributes::SERVICE_VERSION,
                        &version,
                    )],
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

/// Installs a downloaded artifact as `path`: put in place beside it, made executable, then renamed
/// over `path` — the final rename being what makes the swap atomic, so a crash mid-install never
/// leaves a half-written program where one is about to be started.
///
/// A raw artifact is **moved** rather than copied when it can be: since ADR-0021 the download is
/// staged in the same Supervisor directory the program lives in, so the two are normally on one
/// filesystem and the install costs a metadata update instead of a second full write of several
/// hundred megabytes. The move consumes the artifact — the caller's cleanup of it is best-effort
/// for exactly this reason. A rename across filesystems fails, and so does one out of a staging
/// directory an operator has put elsewhere; either way the stream below is the fallback, and the
/// error that matters is reported from there rather than from the attempt.
///
/// The artifact may be the program or an archive holding it (ADR-0018). An archive is opened here,
/// where the binary's name is known, and the member of that name is what gets installed — nothing
/// upstream of this ever repacked the artifact, which is why the hash an Agent verified is the one
/// its author published. Unpacking always writes; only the raw case can be a move.
fn install_executable(
    artifact: &std::path::Path,
    path: &std::path::Path,
    archive_key: Option<&str>,
) -> Result<(), String> {
    let temp = path.with_extension("staged");
    let kind = crate::archive::detect(artifact)?;
    if kind == crate::archive::Kind::Raw && std::fs::rename(artifact, &temp).is_ok() {
        info!(artifact = %artifact.display(), "moved the package artifact into place");
    } else {
        let mut target = std::fs::File::create(&temp)
            .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        match kind {
            crate::archive::Kind::Raw => {
                let mut source = std::fs::File::open(artifact)
                    .map_err(|e| format!("cannot read {}: {e}", artifact.display()))?;
                std::io::copy(&mut source, &mut target)
                    .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
            }
            kind @ (crate::archive::Kind::TarGz | crate::archive::Kind::SevenZ) => {
                let member = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .ok_or_else(|| format!("{} has no file name to look for", path.display()))?;
                let written = match kind {
                    crate::archive::Kind::SevenZ => {
                        crate::archive::extract_7z(artifact, &member, &mut target, archive_key)?
                    }
                    _ => crate::archive::extract_tar_gz(artifact, &member, &mut target)?,
                };
                info!(archive = %artifact.display(), member = %member, bytes = written, "unpacked the package archive");
            }
        }
        drop(target);
    }
    // Whatever put the bytes there, the mode is ours to set: a moved artifact carries the
    // download's permissions, and a written one the process umask.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", temp.display()))?;
    }
    std::fs::rename(&temp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
}

/// Unpacks a package that is a whole directory tree into `<root>/tree` (ADR-0023).
///
/// The tree is built in a staging directory first and moved into place by one rename, so the live
/// name is either the old tree or the new one and never a half-written mixture. The live name is
/// already free — the caller set the previous tree aside — and that previous tree is what a failed
/// install is restored from, untouched throughout: nothing here ever writes into it.
///
/// `program_path` decides two things at once: which member of the archive is the program, and
/// which directory prefix is dropped so the unpacked tree starts where the configuration says it
/// does. A raw artifact — no archive at all — is written to that path directly, so an agent
/// configured for a tree does not fail merely because someone uploaded a bare binary.
fn install_tree(
    artifact: &std::path::Path,
    root: &std::path::Path,
    program_path: &std::path::Path,
    archive_key: Option<&str>,
) -> Result<(), String> {
    let staging = root.join(".staging");
    // A previous attempt that died between unpacking and the rename would leave this behind.
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)
        .map_err(|e| format!("cannot create {}: {e}", staging.display()))?;

    let unpack = |staging: &std::path::Path| -> Result<(), String> {
        match crate::archive::detect(artifact)? {
            crate::archive::Kind::Raw => {
                let program = staging.join(program_path);
                if let Some(parent) = program.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
                }
                std::fs::copy(artifact, &program)
                    .map_err(|e| format!("cannot write {}: {e}", program.display()))?;
                info!(artifact = %artifact.display(), program = %program_path.display(), "the package is a bare program; placed it where the tree expects it");
                Ok(())
            }
            crate::archive::Kind::TarGz => {
                let summary = crate::archive::extract_tree_tar_gz(artifact, program_path, staging)?;
                info!(archive = %artifact.display(), files = summary.files, bytes = summary.bytes, skipped = summary.skipped, "unpacked the package tree");
                Ok(())
            }
            crate::archive::Kind::SevenZ => {
                let summary =
                    crate::archive::extract_tree_7z(artifact, program_path, staging, archive_key)?;
                info!(archive = %artifact.display(), files = summary.files, bytes = summary.bytes, skipped = summary.skipped, "unpacked the package tree");
                Ok(())
            }
        }
    };

    // Whatever went wrong, the staging directory does not survive it: the next install must start
    // from an empty one, and a failed unpack has no value to anyone.
    if let Err(e) = unpack(&staging) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    let program = staging.join(program_path);
    if !program.is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!(
            "the unpacked package holds no file at {}",
            program_path.display()
        ));
    }
    // The tree carries its own modes where the archive had them, but whether the *program* can be
    // executed is not something to inherit from how someone built an archive.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", program.display()))?;
    }

    let live = root.join(crate::config::TREE_DIR);
    std::fs::rename(&staging, &live).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        format!(
            "cannot move the unpacked package to {}: {e}",
            live.display()
        )
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    // What remains here are the cases that reach *into* this module — a private helper and the
    // install function — and need no program to spawn. Everything that supervises a running
    // process moved to `tests/supervisor_process.rs` when ADR-0024 made a real stub reachable;
    // those cases were gated to Unix for want of one, and now run on all three platforms.

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

    /// ADR-0021 stages the download in the same directory the program lives in, so installing a
    /// raw artifact is a move and not a second full write of several hundred megabytes. What makes
    /// that observable is *why* the artifact is gone: it became the program, rather than being
    /// copied and deleted.
    ///
    /// The mode assertion is the one this could genuinely break, and it is Unix's alone. A written
    /// file gets its permissions from the process umask and was always chmod'ed afterwards; a moved
    /// one carries whatever the download had — 0644 here, as `File::create` leaves it — so skipping
    /// the chmod would install a program that cannot be executed.
    #[test]
    fn a_raw_artifact_beside_the_program_is_moved_into_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("packages/agent.staged");
        std::fs::create_dir_all(artifact.parent().expect("parent")).expect("mkdir");
        std::fs::write(&artifact, b"the-program").expect("stage");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
        }

        let program = dir.path().join("program/agent");
        std::fs::create_dir_all(program.parent().expect("parent")).expect("mkdir");
        install_executable(&artifact, &program, None).expect("install");

        assert_eq!(std::fs::read(&program).expect("read"), b"the-program");
        assert!(
            !artifact.exists(),
            "the artifact was moved, not copied — the caller's cleanup of it is best-effort"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&program)
                .expect("stat")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o755,
                "a moved artifact is still made executable"
            );
        }
    }

    /// An archive can never be moved: what belongs at the program's path is one member of it, not
    /// the container. It is unpacked, and the artifact stays for the caller to clean up.
    ///
    /// This is also the stream branch of `install_executable`. The other way into it — a rename
    /// that fails because the staging directory an operator configured is on another filesystem —
    /// runs the same code and is not forced here; doing so would need a second mount.
    #[test]
    fn an_archive_is_unpacked_and_the_artifact_survives_the_install() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("release.tar.gz");
        {
            let file = std::fs::File::create(&artifact).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let content = b"the-member".as_slice();
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "agent-2.0.0/agent", content)
                .expect("append");
            builder.into_inner().expect("tar").finish().expect("gzip");
        }

        let program = dir.path().join("agent");
        install_executable(&artifact, &program, None).expect("install");

        assert_eq!(std::fs::read(&program).expect("read"), b"the-member");
        assert!(
            artifact.exists(),
            "the archive is read, never consumed — only its member is installed"
        );
    }
}
