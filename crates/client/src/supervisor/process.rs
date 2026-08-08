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
            install: None,
            archive_key: None,
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

    /// A Supervisor whose program is not on the machine keeps running and says so. The wording is
    /// the point: what the Server should read is the situation — there is no process — not the
    /// syscall that reported it. This is the state a failed *first* install leaves behind, once
    /// the artifact it could not run has been removed again.
    #[tokio::test]
    async fn a_missing_program_is_reported_as_no_process_not_fatal() {
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
        assert_eq!(health.status, "no process installed");
        assert!(
            health.last_error.contains("definitely-not-here"),
            "the detail still names the path: {}",
            health.last_error
        );
        harness.shutdown_tx.send(true).expect("signal shutdown");
        let _ = harness.task.await;
    }

    /// A program that exists but cannot be executed is a different situation, and keeps the
    /// wording that describes it.
    #[tokio::test]
    async fn an_unexecutable_program_is_reported_as_a_spawn_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = dir.path().join("not-executable");
        std::fs::write(&program, b"data").expect("write");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        let mut harness = start(move || {
            Some(ProcessSpec {
                program: program.clone(),
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
            install: Some(InstallTarget::Binary(binary.clone())),
            archive_key: None,
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

    /// The case ADR-0018 exists for: what upstream publishes is a `.tar.gz`, not a bare binary.
    /// The Supervisor takes the member named after its own binary and installs that.
    #[tokio::test]
    async fn a_package_delivered_as_a_tar_gz_is_unpacked_and_installed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("agent");
        std::fs::write(&binary, "#!/bin/sh\nexec sleep 600\n").expect("write");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        // A release-shaped archive: the program under a versioned directory, next to other files.
        let program = b"#!/bin/sh\n# v2\nexec sleep 600\n";
        let staged = dir.path().join("release.tar.gz");
        {
            let file = std::fs::File::create(&staged).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            for (name, content) in [
                ("agent-2.0.0/LICENSE", b"text".as_slice()),
                ("agent-2.0.0/agent", program.as_slice()),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder
                    .append_data(&mut header, name, content)
                    .expect("append");
            }
            builder.into_inner().expect("tar").finish().expect("gzip");
        }

        let run_binary = binary.clone();
        let (event_tx, events) = mpsc::channel(64);
        let (commands, command_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown) = shutdown_channel();
        let runner = Runner {
            name: "test".to_string(),
            stop_timeout: Duration::from_secs(5),
            apply_grace: Duration::from_millis(200),
            install: Some(InstallTarget::Binary(binary.clone())),
            archive_key: None,
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

        harness
            .commands
            .send(ProcessCommand::ApplyPackage {
                staged: staged.clone(),
                version: "2.0.0".to_string(),
                hash: b"tar-hash".to_vec(),
            })
            .await
            .expect("send");
        let (hash, result) = next_package_ack(&mut harness.events).await;
        assert_eq!(hash, b"tar-hash".to_vec());
        assert_eq!(
            result,
            Ok("2.0.0".to_string()),
            "the unpacked program stays up"
        );
        assert_eq!(
            std::fs::read(&binary).expect("read"),
            program,
            "the installed binary is the member, not the archive"
        );

        harness.shutdown_tx.send(true).expect("shutdown");
        let _ = harness.task.await;
    }

    /// Builds a release-shaped `.tar.gz`: the program and a library it "loads", under one
    /// version-named wrapper directory (ADR-0023).
    fn tree_release(path: &std::path::Path, wrapper: &str, program: &[u8], library: &[u8]) {
        let file = std::fs::File::create(path).expect("create");
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for (name, content) in [
            (format!("{wrapper}/bin/agent"), program),
            (format!("{wrapper}/lib/libagent.so"), library),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, name, content)
                .expect("append");
        }
        builder.into_inner().expect("tar").finish().expect("gzip");
    }

    /// A Runner installing into a tree, spawning whatever sits at `program/tree/bin/agent`.
    fn tree_harness(root: &std::path::Path) -> Harness {
        let program = root.join("tree/bin/agent");
        let (event_tx, events) = mpsc::channel(64);
        let (commands, command_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown) = shutdown_channel();
        let runner = Runner {
            name: "test".to_string(),
            stop_timeout: Duration::from_secs(5),
            apply_grace: Duration::from_millis(200),
            install: Some(InstallTarget::Tree {
                root: root.to_path_buf(),
                program_path: std::path::PathBuf::from("bin/agent"),
            }),
            archive_key: None,
            events: EventSender::new(0, event_tx),
            commands: command_rx,
            build: Box::new(move || {
                Some(ProcessSpec {
                    program: program.clone(),
                    args: Vec::new(),
                    env: Vec::new(),
                    working_dir: None,
                })
            }),
        };
        Harness {
            commands,
            events,
            shutdown_tx,
            task: tokio::spawn(runner.run(shutdown)),
        }
    }

    async fn apply_tree(
        harness: &mut Harness,
        staged: &std::path::Path,
        version: &str,
    ) -> Result<String, String> {
        harness
            .commands
            .send(ProcessCommand::ApplyPackage {
                staged: staged.to_path_buf(),
                version: version.to_string(),
                hash: version.as_bytes().to_vec(),
            })
            .await
            .expect("send");
        next_package_ack(&mut harness.events).await.1
    }

    /// The case ADR-0023 exists for: an agent that is a program *plus* what it loads, arriving
    /// with nothing on the host first — and then being replaced the same way.
    #[tokio::test]
    async fn a_tree_package_lands_whole_and_replaces_the_one_before_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("program");
        std::fs::create_dir_all(&root).expect("create the program directory");

        let first = dir.path().join("agent-1.0.0.tar.gz");
        tree_release(
            &first,
            "agent-1.0.0",
            b"#!/bin/sh\nexec sleep 600\n",
            b"v1-library",
        );
        let mut harness = tree_harness(&root);
        let _ = next_health(&mut harness.events).await; // nothing installed yet

        assert_eq!(
            apply_tree(&mut harness, &first, "1.0.0").await,
            Ok("1.0.0".to_string()),
            "a first install needs nothing on the host"
        );
        assert_eq!(
            std::fs::read(root.join("tree/lib/libagent.so")).expect("the library"),
            b"v1-library",
            "what the program loads came with it"
        );
        assert!(
            !root.join("tree.rollback").exists(),
            "a first install leaves no rollback: there was nothing to keep"
        );
        assert!(!root.join(".staging").exists(), "staging does not survive");

        let second = dir.path().join("agent-2.0.0.tar.gz");
        tree_release(
            &second,
            "agent-2.0.0-linux-amd64",
            b"#!/bin/sh\nexec sleep 600\n",
            b"v2-library",
        );
        assert_eq!(
            apply_tree(&mut harness, &second, "2.0.0").await,
            Ok("2.0.0".to_string())
        );
        assert_eq!(
            std::fs::read(root.join("tree/lib/libagent.so")).expect("the library"),
            b"v2-library",
            "the wrapper directory was renamed between releases and nothing had to follow it"
        );
        assert!(
            !root.join("tree.rollback").exists(),
            "a succeeded install keeps no previous tree"
        );

        harness.shutdown_tx.send(true).expect("shutdown");
        let _ = harness.task.await;
    }

    /// The health gate, one level up: a tree whose program will not stay up puts the *whole*
    /// previous tree back — libraries included, since half of each would run nothing.
    #[tokio::test]
    async fn a_tree_that_will_not_stay_up_is_rolled_back_whole() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("program");
        std::fs::create_dir_all(&root).expect("create the program directory");

        let good = dir.path().join("agent-1.0.0.tar.gz");
        tree_release(
            &good,
            "agent-1.0.0",
            b"#!/bin/sh\nexec sleep 600\n",
            b"v1-library",
        );
        let mut harness = tree_harness(&root);
        let _ = next_health(&mut harness.events).await;
        assert_eq!(
            apply_tree(&mut harness, &good, "1.0.0").await,
            Ok("1.0.0".to_string())
        );

        // A version that exits immediately — rejected by the apply grace.
        let bad = dir.path().join("agent-2.0.0.tar.gz");
        tree_release(&bad, "agent-2.0.0", b"#!/bin/sh\nexit 1\n", b"v2-library");
        assert!(
            apply_tree(&mut harness, &bad, "2.0.0").await.is_err(),
            "a program that exits in the grace has rejected itself"
        );

        assert_eq!(
            std::fs::read(root.join("tree/bin/agent")).expect("the program"),
            b"#!/bin/sh\nexec sleep 600\n",
            "the program that ran before is back"
        );
        assert_eq!(
            std::fs::read(root.join("tree/lib/libagent.so")).expect("the library"),
            b"v1-library",
            "and so is everything beside it — a rollback of half a tree is not a rollback"
        );
        assert!(
            !root.join("tree.rollback").exists(),
            "nothing is left behind"
        );

        harness.shutdown_tx.send(true).expect("shutdown");
        let _ = harness.task.await;
    }

    /// The archive names a member the configuration does not — refused, with the old tree left
    /// exactly where it was.
    #[tokio::test]
    async fn a_tree_missing_the_configured_program_is_refused_and_changes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("program");
        std::fs::create_dir_all(&root).expect("create the program directory");

        let good = dir.path().join("agent-1.0.0.tar.gz");
        tree_release(
            &good,
            "agent-1.0.0",
            b"#!/bin/sh\nexec sleep 600\n",
            b"v1-library",
        );
        let mut harness = tree_harness(&root);
        let _ = next_health(&mut harness.events).await;
        assert_eq!(
            apply_tree(&mut harness, &good, "1.0.0").await,
            Ok("1.0.0".to_string())
        );

        // Same shape, wrong program name: `bin/agent` is not in it.
        let wrong = dir.path().join("other.tar.gz");
        {
            let file = std::fs::File::create(&wrong).expect("create");
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "other-1.0.0/bin/other", &b"nope"[..])
                .expect("append");
            builder.into_inner().expect("tar").finish().expect("gzip");
        }
        let outcome = apply_tree(&mut harness, &wrong, "2.0.0").await;
        assert!(outcome.is_err(), "{outcome:?}");

        assert_eq!(
            std::fs::read(root.join("tree/lib/libagent.so")).expect("the library"),
            b"v1-library",
            "the tree that was running is untouched"
        );
        assert!(!root.join(".staging").exists(), "staging does not survive");

        harness.shutdown_tx.send(true).expect("shutdown");
        let _ = harness.task.await;
    }

    /// Bringing a host into the fleet the other way round: the Supervisor is configured, the
    /// program is not installed yet, and the Server delivers it. A plugin with nothing to run —
    /// a Collector awaiting its configuration — must not turn that into a failed install, which
    /// would delete the binary that was just put in place.
    #[tokio::test]
    async fn an_install_with_nothing_to_run_yet_keeps_the_binary_and_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("otelcol");
        assert!(
            !binary.exists(),
            "the program is not installed on this host"
        );

        let staged = dir.path().join("downloaded.staged");
        std::fs::write(&staged, b"#!/bin/sh\nexec sleep 600\n").expect("stage");

        let (event_tx, events) = mpsc::channel(64);
        let (commands, command_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown) = shutdown_channel();
        let runner = Runner {
            name: "test".to_string(),
            stop_timeout: Duration::from_secs(5),
            apply_grace: Duration::from_millis(200),
            install: Some(InstallTarget::Binary(binary.clone())),
            archive_key: None,
            events: EventSender::new(0, event_tx),
            commands: command_rx,
            // No configuration yet, so the plugin has nothing to run.
            build: Box::new(|| None),
        };
        let task = tokio::spawn(runner.run(shutdown));
        let mut harness = Harness {
            commands,
            events,
            shutdown_tx,
            task,
        };
        let _ = next_health(&mut harness.events).await; // "awaiting configuration"

        harness
            .commands
            .send(ProcessCommand::ApplyPackage {
                staged,
                version: "1.0.0".to_string(),
                hash: b"first".to_vec(),
            })
            .await
            .expect("send");

        let (hash, result) = next_package_ack(&mut harness.events).await;
        assert_eq!(hash, b"first".to_vec());
        assert_eq!(
            result,
            Ok("1.0.0".to_string()),
            "the artifact is installed; running it is the configuration's business"
        );
        assert!(binary.exists(), "the installed binary stays on disk");

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
            install: Some(InstallTarget::Binary(binary.clone())),
            archive_key: None,
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

    /// ADR-0021 stages the download in the same directory the program lives in, so installing a
    /// raw artifact is a move and not a second full write of several hundred megabytes. What makes
    /// that observable is *why* the artifact is gone: it became the program, rather than being
    /// copied and deleted.
    ///
    /// The mode assertion is the one this change could genuinely break. A written file gets its
    /// permissions from the process umask and was always chmod'ed afterwards; a moved one carries
    /// whatever the download had — 0644 here, as `File::create` leaves it — so skipping the chmod
    /// would install a program that cannot be executed.
    #[test]
    fn a_raw_artifact_beside_the_program_is_moved_into_place() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact = dir.path().join("packages/agent.staged");
        std::fs::create_dir_all(artifact.parent().expect("parent")).expect("mkdir");
        std::fs::write(&artifact, b"#!/bin/sh\nexec sleep 600\n").expect("stage");
        std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        let program = dir.path().join("program/agent");
        std::fs::create_dir_all(program.parent().expect("parent")).expect("mkdir");
        install_executable(&artifact, &program, None).expect("install");

        assert_eq!(
            std::fs::read(&program).expect("read"),
            b"#!/bin/sh\nexec sleep 600\n"
        );
        assert!(
            !artifact.exists(),
            "the artifact was moved, not copied — the caller's cleanup of it is best-effort"
        );
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
            let content = b"#!/bin/sh\n# v2\nexec sleep 600\n".as_slice();
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

        assert_eq!(
            std::fs::read(&program).expect("read"),
            b"#!/bin/sh\n# v2\nexec sleep 600\n"
        );
        assert!(
            artifact.exists(),
            "the archive is read, never consumed — only its member is installed"
        );
    }
}
