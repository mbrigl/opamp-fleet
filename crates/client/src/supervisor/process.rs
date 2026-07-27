//! The shared child runner both plugins drive: spawn, watch, restart with backoff, apply a new
//! configuration by respawning, stop gracefully within the budget.
//!
//! Mirrors the reference `opampsupervisor` (ADR-0011): SIGTERM → bounded wait → kill on Unix,
//! `Child::kill` on Windows (which has no SIGTERM equivalent), and exponential backoff for a
//! process that keeps exiting.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use opamp::proto::ComponentHealth;
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
                        // Applying means running on the new files: acknowledge accordingly.
                        let result = match (&child, (self.build)().is_some()) {
                            (Some(_), _) => Ok(()),
                            (None, false) => Ok(()), // nothing should run; that is the config
                            (None, true) => Err("the process did not start".to_string()),
                        };
                        self.events
                            .send(ProcessEvent::ConfigApplied { hash: config.config_hash, result })
                            .await;
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
        let (event_tx, events) = mpsc::channel(64);
        let (commands, command_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown) = shutdown_channel();
        let runner = Runner {
            name: "test".to_string(),
            stop_timeout: Duration::from_secs(5),
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
