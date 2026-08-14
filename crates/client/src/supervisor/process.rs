//! The shared child runner both plugins drive: spawn, watch, restart with backoff, apply a new
//! configuration by respawning, stop gracefully within the budget — plus the version probe both
//! plugins use to learn a Managed Process's own version, run at startup and after every swap.
//!
//! Mirrors the reference `opampsupervisor` (ADR-0011): SIGTERM → bounded wait → kill on Unix,
//! `Child::kill` on Windows (which has no SIGTERM equivalent), and exponential backoff for a
//! process that keeps exiting.

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opamp::proto::{AgentDescription, ComponentHealth};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::install;
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
        // rename would nest rather than replace. This also clears any *retained* predecessor
        // (ADR-0058) and its marker — each Supervisor keeps only the immediately previous version.
        self.drop_backup();
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

    /// The marker beside a retained backup (ADR-0058): a sibling file holding the Unix-seconds
    /// deadline after which the backup may be swept. A file even for a tree, whose backup is a
    /// directory, so a restart can find the deadline without opening the tree.
    fn backup_marker(&self) -> PathBuf {
        let mut raw = self.backup().into_os_string();
        raw.push(".until");
        PathBuf::from(raw)
    }

    /// Keeps the backup and records when it may be deleted (ADR-0058). Persisted, so the deadline
    /// survives a Client restart the way the self-update outcome marker does (ADR-0020).
    fn retain(&self, deadline_unix: u64) {
        let _ = std::fs::write(self.backup_marker(), deadline_unix.to_string());
    }

    /// Drops the backup — and its retention marker — once it is no longer wanted: immediately when
    /// retention is off, or when a later update supersedes it.
    fn drop_backup(&self) {
        self.remove(&self.backup());
        let _ = std::fs::remove_file(self.backup_marker());
    }

    /// Deletes a retained backup whose deadline has passed (ADR-0058). Best-effort; returns whether
    /// it removed one. An unreadable or absent marker leaves an unretained backup alone — only a
    /// backup this Runner deliberately retained carries a marker, and a marker whose value will not
    /// parse is treated as expired rather than kept forever.
    fn sweep(&self, now_unix: u64) -> bool {
        let marker = self.backup_marker();
        let Ok(text) = std::fs::read_to_string(&marker) else {
            return false;
        };
        let expired = text.trim().parse::<u64>().map_or(true, |d| now_unix >= d);
        if expired {
            self.drop_backup();
        }
        expired
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
    /// How long the version a successful update supersedes is kept before deletion (ADR-0058), so
    /// an operator has a fallback window. Zero deletes it on success, the pre-ADR-0058 behaviour.
    pub retain_previous: Duration,
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
        // Consecutive start failures, and when the current process started (ADR-0058): together they
        // stop a program that keeps crashing from being restarted forever. A process that runs past
        // `STABLE_RUN_FLOOR` clears the streak; a command (config, package, restart) resets it.
        let mut streak = 0usize;
        let mut last_start = Instant::now();
        // Clear any predecessor whose retention window already elapsed while the Client was down
        // (ADR-0058) — the deadline is wall-clock, so a restart is exactly when one may have passed.
        self.sweep_backup();
        // What runs today, before it runs: a Collector without the opampextension never reports
        // its own version, and one that is not configured yet never even starts.
        self.probe_version();
        let mut child = self.spawn_if_due().await;

        // Honour the retention window even when the Client stays up past it. Cheap and coarse: a
        // day-long window does not need a tight tick, and the sweep itself is a stat plus a delete.
        let mut sweep = tokio::time::interval(SWEEP_INTERVAL);
        sweep.tick().await; // the first tick is immediate; the startup sweep above already ran

        loop {
            let exited = async {
                match child.as_mut() {
                    Some(c) => c.wait().await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = sweep.tick() => self.sweep_backup(),
                command = self.commands.recv() => match command {
                    Some(ProcessCommand::ApplyConfig { config }) => {
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        backoff.reset();
                        streak = 0; // a new configuration is a fresh chance (ADR-0058)
                        child = self.spawn_if_due().await;
                        last_start = Instant::now();
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
                            // backoff — but not forever (ADR-0058): a configuration that will not
                            // start is held after a few tries rather than spun on.
                            if self
                                .respawn_or_hold(
                                    &mut child,
                                    &mut backoff,
                                    &mut streak,
                                    &mut last_start,
                                    &mut shutdown,
                                )
                                .await
                            {
                                break;
                            }
                        }
                    }
                    Some(ProcessCommand::ApplyPackage { staged, version, hash }) => {
                        // Swap the binary, restart, and health-gate on the apply grace — a binary
                        // that will not stay up is rolled back to the bytes it replaced (ADR-0015).
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        backoff.reset();
                        streak = 0; // a new package is a fresh chance (ADR-0058)
                        let result = self.swap_and_gate(staged, &version, &mut child, &mut shutdown).await;
                        if child.is_none() && !matches!(result, GraceOutcome::ShuttingDown) {
                            child = self.spawn_if_due().await;
                            last_start = Instant::now();
                        }
                        match result {
                            GraceOutcome::Ok => {
                                last_start = Instant::now();
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
                                // Stay supervised, exactly as a failed ApplyConfig does — and, like
                                // it, held rather than spun on once it keeps failing (ADR-0058).
                                if self
                                    .respawn_or_hold(
                                        &mut child,
                                        &mut backoff,
                                        &mut streak,
                                        &mut last_start,
                                        &mut shutdown,
                                    )
                                    .await
                                {
                                    break;
                                }
                            }
                            GraceOutcome::ShuttingDown => break,
                        }
                    }
                    Some(ProcessCommand::Restart) => {
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        backoff.reset();
                        streak = 0; // an operator restart is a fresh chance (ADR-0058)
                        child = self.spawn_if_due().await;
                        last_start = Instant::now();
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
                    // A process that had been up a while is ordinary supervision, not a start loop:
                    // its exit clears the streak (ADR-0058). One that keeps exiting quickly does not,
                    // so it is held after a few tries rather than restarted forever.
                    if last_start.elapsed() >= self.apply_grace.max(STABLE_RUN_FLOOR) {
                        streak = 0;
                    }
                    if self
                        .respawn_or_hold(
                            &mut child,
                            &mut backoff,
                            &mut streak,
                            &mut last_start,
                            &mut shutdown,
                        )
                        .await
                    {
                        break;
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
            // No predecessor: a first install with nothing behind it. It is *not* rolled back
            // (ADR-0058) — the verified program stays in place, reported InstallFailed, so a first
            // package that will not start does not empty `program/` and set the Server re-offering
            // it in a loop. Discarding it here is what used to make that loop turn.
            (GraceOutcome::Failed(_), false) => {
                warn!(supervisor = %self.name, "the first install would not start; kept in place, not rolled back (nothing to roll back to)");
            }
            // Applied: the predecessor is no longer what runs, but it is kept for the retention
            // window (ADR-0058) so an operator has a fallback, then swept once its deadline passes.
            (_, true) => self.retain_backup(&target),
            (_, false) => {}
        }
        outcome
    }

    /// Keeps the version a successful update superseded, or drops it now (ADR-0058). With retention
    /// off it is the old immediate delete; otherwise the backup stays and a marker records the
    /// deadline `now + retain_previous`, swept once it passes.
    fn retain_backup(&self, target: &InstallTarget) {
        if self.retain_previous.is_zero() {
            target.drop_backup();
            return;
        }
        let deadline = now_unix().saturating_add(self.retain_previous.as_secs());
        target.retain(deadline);
        info!(
            supervisor = %self.name,
            retain_secs = self.retain_previous.as_secs(),
            "keeping the previous version until it may be rolled back to no longer"
        );
    }

    /// Deletes a retained predecessor whose deadline has passed (ADR-0058). Called at startup and on
    /// a periodic tick, so the window is honoured whether or not the Client restarts within it.
    fn sweep_backup(&self) {
        if let Some(target) = &self.install {
            if target.sweep(now_unix()) {
                info!(supervisor = %self.name, "swept the retained previous version past its window");
            }
        }
    }

    /// Respawns after a start failure — or, once the process has failed to stay up
    /// [`MAX_CRASH_RESTARTS`] times in a row, **holds** it down instead of spinning (ADR-0058). A
    /// held Supervisor reports unhealthy and waits: the next configuration, package, or restart is
    /// a fresh chance and resets the streak. Returns `true` when shutdown was requested mid-wait.
    async fn respawn_or_hold(
        &self,
        child: &mut Option<Child>,
        backoff: &mut Backoff,
        streak: &mut usize,
        last_start: &mut Instant,
        shutdown: &mut Shutdown,
    ) -> bool {
        *streak += 1;
        if *streak >= MAX_CRASH_RESTARTS {
            warn!(
                supervisor = %self.name, restarts = *streak,
                "not restarting: the program keeps failing to start — holding until a new configuration, package, or restart (ADR-0058)"
            );
            self.events
                .send(ProcessEvent::Health(unhealthy(
                    "not restarting: the program keeps failing to start".to_string(),
                    format!("held after {} failed starts in a row", *streak),
                )))
                .await;
            *child = None;
            return false;
        }
        let delay = backoff.advance();
        tokio::select! {
            _ = tokio::time::sleep(delay) => {
                *child = self.spawn_if_due().await;
                *last_start = Instant::now();
                false
            }
            _ = shutdown.requested() => true,
        }
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
    /// with `ETXTBSY`, which is transient and says nothing about the artifact
    /// ([`install::is_text_file_busy`] states why). So the swap retries briefly rather than rolling
    /// back a binary that is perfectly good.
    async fn try_spawn(&self) -> Result<Child, String> {
        let mut attempt = 0;
        loop {
            match self.spawn_once().await {
                Err(e) if install::is_text_file_busy(&e) && attempt < install::BUSY_RETRIES => {
                    attempt += 1;
                    warn!(
                        supervisor = %self.name, attempt,
                        "the new binary is momentarily busy (another spawn holds it); retrying"
                    );
                    tokio::time::sleep(install::BUSY_DELAY).await;
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

/// How long a version probe may take before it is abandoned — it must never stall startup.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often a running Supervisor re-checks whether a retained predecessor's window has passed
/// (ADR-0058). Coarse on purpose: the window is measured in hours to a day, and the startup sweep
/// covers a Client that was down when a deadline elapsed.
const SWEEP_INTERVAL: Duration = Duration::from_secs(600);

/// How many times in a row a Managed Process may fail to stay up before the Runner stops restarting
/// it and holds until something changes — a new configuration, a new package, or a restart (ADR-0058).
/// It is the self-update's give-up (three attempts, ADR-0020), so the two update paths behave alike.
/// A restart loop is a denial of service against the fleet's own Server; this bounds it.
const MAX_CRASH_RESTARTS: usize = 3;

/// A process that stayed up at least this long counts as *stable*: a later exit is ordinary
/// supervision, not a start loop, so it clears the streak. Below the floor a zero-grace Supervisor
/// would treat every restart as stable; above it, one that survives its grace comfortably resets.
const STABLE_RUN_FLOOR: Duration = Duration::from_secs(10);

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
    // A raw artifact is already the program, and it was downloaded into this Supervisor's own
    // directory — so it can be renamed into place instead of copied, which the staging path below
    // cannot assume and the Client's own update (whose artifact is not next to its destination)
    // has no equivalent for.
    if crate::archive::detect(artifact)? == crate::archive::Kind::Raw
        && std::fs::rename(artifact, &temp).is_ok()
    {
        info!(artifact = %artifact.display(), "moved the package artifact into place");
        install::make_executable(&temp)?;
    } else {
        let member = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{} has no file name to look for", path.display()))?;
        install::write_program(artifact, &temp, &member, archive_key)?;
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
    install::make_executable(&program)?;

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

/// Wall-clock seconds since the Unix epoch — what a retention deadline (ADR-0058) is written and
/// compared in, so it survives a Client restart.
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
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

    /// ADR-0058: a retained predecessor is swept only once its deadline passes, never before, and
    /// the marker goes with it.
    #[test]
    fn a_retained_backup_is_swept_only_after_its_deadline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = InstallTarget::Binary(dir.path().join("prog"));
        std::fs::write(target.backup(), b"the previous version").expect("write backup");
        target.retain(1000); // deadline: 1000 unix-seconds
        assert!(
            target.backup_marker().exists(),
            "the marker records the deadline"
        );

        assert!(!target.sweep(999), "before the deadline it is kept");
        assert!(
            target.backup().exists(),
            "the previous version is still there"
        );

        assert!(target.sweep(1000), "at the deadline it is swept");
        assert!(!target.backup().exists(), "the previous version is gone");
        assert!(!target.backup_marker().exists(), "and so is its marker");
    }

    /// A backup with no marker is not something this Runner retained (the pre-ADR-0058 immediate
    /// drop, or a half-finished install), so a sweep leaves it alone.
    #[test]
    fn a_sweep_leaves_an_unmarked_backup_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = InstallTarget::Binary(dir.path().join("prog"));
        std::fs::write(target.backup(), b"unmarked").expect("write backup");
        assert!(!target.sweep(u64::MAX), "no marker, nothing to sweep");
        assert!(target.backup().exists());
    }

    /// Dropping a backup takes its marker too, so a superseding update does not leave a dangling
    /// deadline behind.
    #[test]
    fn dropping_a_backup_clears_its_marker() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = InstallTarget::Binary(dir.path().join("prog"));
        std::fs::write(target.backup(), b"old").expect("write");
        target.retain(5000);
        assert!(target.backup_marker().exists());
        target.drop_backup();
        assert!(!target.backup().exists());
        assert!(!target.backup_marker().exists());
    }
}
