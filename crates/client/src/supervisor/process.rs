//! The shared child runner both plugins drive — the generic implementation of the whole
//! lifecycle vocabulary (ADR-0015): spawn, watch, restart with backoff, apply a new
//! configuration by respawning — or, when the plugin declared a reload mechanism, in place —
//! stop gracefully within the budget, and answer an uninstall before the core purges. Plus the
//! version probe both plugins use to learn a Managed Process's own version, run at startup and
//! after every swap.
//!
//! Mirrors the reference `opampsupervisor` (ADR-0015): SIGTERM → bounded wait → kill on Unix,
//! `Child::kill` on Windows (which has no SIGTERM equivalent), and exponential backoff for a
//! process that keeps exiting.

use std::path::PathBuf;

use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::service::runtime::Shutdown;
use crate::supervisor::ports::{EventSender, ProcessCommand, ProcessEvent};
use crate::transport::Backoff;

/// How a plugin wants its Managed Process invoked, rebuilt whenever the configuration changed.
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Where the process starts. `None` means **the directory the program lives in**, which the
    /// spawn resolves (ADR-0015): a single-file package's `program/`, a tree package's tree root —
    /// the latter being exactly what the GLPI Agent's own Windows launcher does by hand. Before
    /// that rule the process inherited whatever directory the service manager left this Client in,
    /// typically `/`: a default nobody chose. A kind that needs another directory still names one.
    pub working_dir: Option<PathBuf>,
    /// Directories the Managed Process **writes into**, created before every spawn.
    ///
    /// An agent the fleet delivers cannot expect a host prepared by hand: an installation that
    /// fails because a directory is missing is this Client's failure, not the operator's. Many
    /// agents create nothing themselves — Icinga 2 exits when `DataDir` is absent, the GLPI Agent
    /// exits when `--vardir` is — so the kind that knows an agent names what that agent needs, and
    /// the spawn guarantees it.
    ///
    /// **Before every spawn, not once at install**, so a directory removed under a running fleet
    /// comes back on the next restart rather than taking the Supervisor down. Created owner-only,
    /// for the reason [`crate::storage::create_private_dir`] gives: what an agent writes about a
    /// host is not for every local user to read.
    ///
    /// The program's own directories are **not** listed here — the install creates those
    /// ([`InstallTarget::prepare`]). This is for what the agent writes at run time, which lives
    /// outside `program/` precisely because a package swap replaces that whole.
    pub ensure_dirs: Vec<PathBuf>,
}

    pub program: PathBuf,
    pub args: Vec<String>,
/// The adapter task driving one Managed Process. The plugin supplies `build`: the current
/// [`ProcessSpec`], or `None` while the process should not run (a Collector before any
/// configuration arrived).
pub struct Runner {
    pub name: String,
    pub stop_timeout: Duration,
    /// How long a freshly (re)started process must survive before `ApplyConfig` is acknowledged
    /// (ADR-0015's health-gated acknowledgement); zero acknowledges on start.
    pub apply_grace: Duration,
    /// The signal that makes the running process re-read its configuration in place (ADR-0015);
    /// `None` — the generic behaviour — applies a configuration by restarting. A reload that
    /// fails, or a process that dies on it, falls back to the restart (`reload-or-restart`).
    pub reload_signal: Option<i32>,
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
                        backoff.reset();
                        // In place first (ADR-0015): a kind that declared a reload keeps its
                        // process — and its in-flight state — across the change; anything short
                        // of a survived grace falls back to the restart below.
                            Reloaded::Applied => {
                                self.events
                                    .send(ProcessEvent::ConfigApplied {
                                        hash: config.config_hash,
                                        result: Ok(()),
                                    })
                                    .await;
                                continue;
                            }
                            Reloaded::ShuttingDown => break,
                            Reloaded::NotApplied => {}
                        }
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        child = self.spawn_if_due().await;
                        // Applying means running on the new files — and surviving the apply
                        // grace (ADR-0015's health-gated acknowledgement): a process that exits
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
                            }
                        }
                    }
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        backoff.reset();
                    Some(ProcessCommand::Restart) => {
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        backoff.reset();
                        child = self.spawn_if_due().await;
                    }
                    Some(ProcessCommand::Uninstall) => {
                        // Retired for good (ADR-0015): the generic uninstall is exactly the
                        // graceful stop — nothing the generic Runner installs lives outside the
                        // Supervisor's directory, and that directory is the core's to purge
                        // (ADR-0022) once this answer is out.
                        stop(&mut child, self.stop_timeout, &self.name).await;
                        self.events
                            .send(ProcessEvent::Uninstalled { result: Ok(()) })
                            .await;
                        return;
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
                    }
                }
                _ = shutdown.requested() => break,
            }
        }
        stop(&mut child, self.stop_timeout, &self.name).await;
    }

            self.events
                .send(ProcessEvent::Health(unhealthy(
    /// The in-place apply (ADR-0015): sends the declared reload signal to the running process,
    /// which must then survive the apply grace — the same standard the restart path holds a
    /// fresh process to, and the only outside-observable evidence a reload leaves. Everything
    /// short of that is `NotApplied`, and the caller restarts on the new files instead
    /// (`reload-or-restart`): no mechanism declared, nothing running to signal, a failed
    /// signal, or a death within the grace.
    async fn try_reload(&self, child: &mut Option<Child>, shutdown: &mut Shutdown) -> Reloaded {
        let Some(signal) = self.reload_signal else {
            return Reloaded::NotApplied;
        };
        let Some(mut proc) = child.take() else {
            return Reloaded::NotApplied;
        };
        let Some(pid) = proc.id() else {
            // Already exited; the restart path is about to notice and respawn.
            *child = Some(proc);
            return Reloaded::NotApplied;
        };
        if let Err(e) = send_reload_signal(pid, signal) {
            warn!(supervisor = %self.name, signal, error = %e, "cannot signal a reload; restarting instead");
            *child = Some(proc);
            return Reloaded::NotApplied;
        }
        info!(supervisor = %self.name, signal, "reload signalled");
        if self.apply_grace.is_zero() {
            *child = Some(proc);
            return Reloaded::Applied;
        }
        tokio::select! {
            status = proc.wait() => {
                let describe = status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|e| format!("wait failed: {e}"));
                warn!(supervisor = %self.name, status = %describe, "process exited on the reload; restarting on the new files");
                Reloaded::NotApplied
            }
            _ = tokio::time::sleep(self.apply_grace) => {
                *child = Some(proc);
                Reloaded::Applied
            }
            _ = shutdown.requested() => {
                *child = Some(proc);
                Reloaded::ShuttingDown
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
        };
        // What the agent writes into, guaranteed before it runs. A failure here is a spawn failure
        // like any other: the Runner reports it and retries with backoff, rather than the process
        // starting and exiting on a directory nobody made.
        for dir in &spec.ensure_dirs {
            if let Err(e) = crate::storage::create_private_dir(dir) {
                warn!(supervisor = %self.name, dir = %dir.display(), error = %e, "cannot prepare a directory the agent writes into");
                return Err(std::io::Error::other(format!(
                    "cannot prepare {}: {e}",
                    dir.display()
                )));
            }
        }
        // The program's own directory where nothing else was said (ADR-0015). A bare relative
        // name — which ADR-0022's path rule does not produce — yields an empty parent, and an
        // empty `current_dir` fails the spawn, so that case keeps inheriting as it did.
        let derived = spec
            .program
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty());
        let working_dir = spec.working_dir.as_deref().or(derived);
        // **The program is made absolute whenever a working directory is set.** `Command` does
        // `chdir` before `exec` on Unix, so a relative program would be resolved against the
        // directory it was just moved into — `<dir>/<dir>/<program>`, which is nothing. This is
        // not a corner: `state_dir` defaults to the relative `client-state`, so every Managed
        // Process under a default configuration hit it, and the failure arrives as a bare
        // `No such file or directory` naming a file that is plainly there.
        let program = match working_dir {
            Some(_) => crate::config::absolute(&spec.program),
            None => spec.program.clone(),
        };
        let mut command = Command::new(&program);
        command.args(&spec.args).envs(spec.env.iter().cloned());
        if let Some(dir) = working_dir {
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
            }
            Err(e) => {
                warn!(supervisor = %self.name, program = %spec.program.display(), error = %e, "cannot spawn");
                self.events
                    .send(ProcessEvent::Health(unhealthy(
                        format!("cannot spawn {}: {e}", spec.program.display()),
                    )))
                    .await;
            }
        }
    }
}

///
                    identifying_attributes: vec![opamp::attributes::string_attr(
                        opamp::attributes::SERVICE_VERSION,
                        &version,
                    )],
/// Graceful stop: SIGTERM and a bounded wait on Unix, then (or on Windows, directly) kill.
async fn stop(child: &mut Option<Child>, timeout: Duration, name: &str) {
    let Some(mut c) = child.take() else {
        return;
    };
    #[cfg(unix)]
    if let Some(pid) = c.id() {
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

/// The result of an in-place reload attempt (ADR-0015).
enum Reloaded {
    /// Signalled and survived the grace — applied, the process kept running.
    Applied,
    /// No reload happened (no mechanism, nothing running, a failed signal, or a death within
    /// the grace) — the caller falls back to the restart.
    NotApplied,
/// Delivers the reload signal (ADR-0015). Unix-only in substance: the settings parse refuses a
/// `reload_signal` anywhere else, so the other arm exists for the compiler, not for a host.
#[cfg(unix)]
fn send_reload_signal(pid: u32, signal: i32) -> Result<(), String> {
    // SAFETY: plain kill(2) on the child's pid; no memory is touched.
    match unsafe { libc::kill(pid as libc::pid_t, signal) } {
        0 => Ok(()),
        _ => Err(std::io::Error::last_os_error().to_string()),
    }
}

#[cfg(not(unix))]
fn send_reload_signal(_pid: u32, _signal: i32) -> Result<(), String> {
    Err("this platform has no signal a process can reload on".to_string())
}

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

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
#[cfg(test)]
mod tests {
    use super::*;

    // What remains here are the cases that reach *into* this module — a private helper and the
    // install function — and need no program to spawn. Everything that supervises a running
    // process moved to `tests/supervisor_process.rs` when ADR-0011 made a real stub reachable;
    // those cases were gated to Unix for want of one, and now run on all three platforms.

    /// The mode assertion is the one this could genuinely break, and it is Unix's alone. A written
    /// file gets its permissions from the process umask and was always chmod'ed afterwards; a moved
    /// one carries whatever the download had — 0644 here, as `File::create` leaves it — so skipping
    /// the chmod would install a program that cannot be executed.
        std::fs::write(&artifact, b"the-program").expect("stage");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&artifact, std::fs::Permissions::from_mode(0o644))
                .expect("chmod");
        }
        assert_eq!(std::fs::read(&program).expect("read"), b"the-program");
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
            let content = b"the-member".as_slice();
        assert_eq!(std::fs::read(&program).expect("read"), b"the-member");
}
