//! The [`ManagedAgent`] adapter for a **Foreign Agent** that does not speak OpAMP — the **Custom
//! Supervisor** (ADR-0009).
//!
//! It owns a plain process (nginx, a legacy daemon, anything with a config file). "Applying a config"
//! is: write the received bytes to the agent's config file, then run a configured **reload** command or,
//! by default, **restart** the process. It reports **process liveness** as health and **echoes** the
//! written config as effective config — translating a non-OpAMP agent's lifecycle into the OpAMP control
//! loop so it appears in the fleet like any other Agent (specification Goal 7).
//!
//! Health is liveness-only for now (running = healthy); an async health-check hook is a later ADR.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::process::{Child, Command};
use tracing::info;

use opamp_proto::proto::{AgentConfigFile, AgentConfigMap, EffectiveConfig};

use crate::agent::{liveness_health, AgentStatus, ChangeSignal, ManagedAgent};

const MAIN_CONFIG_KEY: &str = "";

/// How to run and reconfigure a Foreign Agent.
pub struct ProcessConfig {
    /// A logical name for logs (the supervisor reports the fleet identity separately).
    pub name: String,
    /// The command to run the agent: executable followed by its arguments.
    pub command: Vec<String>,
    /// Where to write the configuration the Server distributes.
    pub config_path: PathBuf,
    /// An optional command to reload the agent in place after a config write; if absent, the process is
    /// restarted instead.
    pub reload: Option<Vec<String>>,
}

/// A Foreign Agent under management: a process, the last config written to it, and how to run it.
pub struct ProcessAgent {
    config: ProcessConfig,
    child: Option<Child>,
    last_config: Vec<u8>,
    start_time_unix_nano: u64,
}

impl ProcessAgent {
    pub fn new(config: ProcessConfig) -> Self {
        let start_time_unix_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            config,
            child: None,
            last_config: Vec::new(),
            start_time_unix_nano,
        }
    }

    /// Stops the current process, if any, and starts a fresh one from the configured command.
    async fn start(&mut self) -> Result<(), String> {
        self.stop().await;
        let Some((exe, args)) = self.config.command.split_first() else {
            return Err("the foreign agent command is empty".to_string());
        };
        let child = Command::new(exe)
            .args(args)
            // If the Supervisor exits, the Foreign Agent it owns must not outlive it.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("cannot start foreign agent '{}': {e}", self.config.name))?;
        info!(pid = child.id(), agent = %self.config.name, "foreign agent started");
        self.child = Some(child);
        Ok(())
    }

    /// Terminates the current process gracefully (SIGTERM, then SIGKILL), waiting for it to exit.
    async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            crate::agent::terminate(&mut child).await;
        }
    }

    /// Writes the configuration to the agent's config file.
    fn write_config(&self, config: &[u8]) -> Result<(), String> {
        if let Some(dir) = self.config.config_path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create config directory {}: {e}", dir.display()))?;
        }
        std::fs::write(&self.config.config_path, config).map_err(|e| {
            format!(
                "cannot write foreign agent config to {}: {e}",
                self.config.config_path.display()
            )
        })
    }

    /// Runs the configured reload command, returning its error output if it fails.
    async fn run_reload(&self, reload: &[String]) -> Result<(), String> {
        let Some((exe, args)) = reload.split_first() else {
            return Err("the reload command is empty".to_string());
        };
        let output = Command::new(exe)
            .args(args)
            .output()
            .await
            .map_err(|e| format!("cannot run reload command: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let details = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("reload command failed: {details}"))
    }
}

impl ManagedAgent for ProcessAgent {
    // prepare_config: identity — a Foreign Agent takes the config as distributed.

    async fn apply(&mut self, config: &[u8]) -> Result<(), String> {
        self.write_config(config)?;
        self.last_config = config.to_vec();
        match self.config.reload.clone() {
            Some(reload) if self.child.is_some() => self.run_reload(&reload).await,
            // No reload command, or nothing running yet: (re)start the process on the new config.
            _ => self.start().await,
        }
    }

    async fn restart(&mut self) -> Result<(), String> {
        self.start().await
    }

    fn status(&self) -> AgentStatus {
        let health = liveness_health(
            self.child.is_some(),
            String::new(),
            self.start_time_unix_nano,
        );
        let effective_config = (!self.last_config.is_empty()).then(|| EffectiveConfig {
            config_map: Some(AgentConfigMap {
                config_map: [(
                    MAIN_CONFIG_KEY.to_string(),
                    AgentConfigFile {
                        body: self.last_config.clone(),
                        content_type: String::new(),
                    },
                )]
                .into_iter()
                .collect(),
            }),
        });
        AgentStatus {
            health,
            effective_config,
            // A Foreign Agent does not report its own OpAMP identity; the supervisor synthesizes it.
            agent_description: None,
            available_components: None,
        }
    }

    fn change_signal(&self) -> ChangeSignal {
        // A Foreign Agent has no push channel; the supervision tick surfaces liveness changes.
        ChangeSignal::never()
    }

    async fn supervise(&mut self) -> Option<String> {
        let status = self.child.as_mut()?.try_wait().ok()??;
        self.child = None;
        Some(status.to_string())
    }

    async fn shutdown(&mut self) {
        self.stop().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("opamp-proc-{tag}-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn apply_writes_the_config_and_starts_the_process() {
        let dir = unique_dir("apply");
        let cfg_path = dir.join("agent.conf");
        let mut agent = ProcessAgent::new(ProcessConfig {
            name: "sleeper".to_string(),
            command: vec!["sleep".to_string(), "30".to_string()],
            config_path: cfg_path.clone(),
            reload: None,
        });

        agent.apply(b"hello foreign agent\n").await.unwrap();
        assert_eq!(std::fs::read(&cfg_path).unwrap(), b"hello foreign agent\n");

        let status = agent.status();
        assert!(status.health.healthy, "a running process reports healthy");
        // Effective config echoes what was written.
        let body = status
            .effective_config
            .unwrap()
            .config_map
            .unwrap()
            .config_map[MAIN_CONFIG_KEY]
            .body
            .clone();
        assert_eq!(body, b"hello foreign agent\n");

        agent.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn supervise_reports_a_process_that_exited() {
        let dir = unique_dir("exit");
        let mut agent = ProcessAgent::new(ProcessConfig {
            name: "quitter".to_string(),
            // `false` exits immediately with a non-zero status: a stand-in for a crashing agent.
            command: vec!["false".to_string()],
            config_path: dir.join("agent.conf"),
            reload: None,
        });
        agent.apply(b"x").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let reason = agent.supervise().await.expect("the process has exited");
        assert!(!reason.is_empty());
        // Reported once, then forgotten.
        assert!(agent.supervise().await.is_none());
        assert!(
            !agent.status().health.healthy,
            "a dead process is unhealthy"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
