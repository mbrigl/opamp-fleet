//! Ownership of the OpenTelemetry Collector process.
//!
//! Like the upstream Supervisor, the Rust Supervisor does not embed the Collector: it writes the
//! configuration it is told to run out to a file and (re)starts the Collector process against it.
//! "Applying a configuration" is therefore a file write followed by a process restart — a spurious
//! restart is a spurious outage, so the caller applies only on an actual hash change.
//!
//! A configuration is **validated before it is applied**, by running the collector's own `validate`
//! subcommand against a throwaway copy. A rejected configuration is thus reported as `FAILED` (with the
//! collector's own error) without ever restarting — so a bad config never takes the running, good
//! collector down. This is how "a rejected configuration is visible" (specification goal 4) is met
//! without the timing guesswork of watching a fresh process for an early crash.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::{Child, Command};
use tracing::info;

/// A supervised OpenTelemetry Collector: an executable and the config file it is launched against.
pub struct Collector {
    executable: PathBuf,
    config_path: PathBuf,
    child: Option<Child>,
}

impl Collector {
    pub fn new(executable: impl Into<PathBuf>, config_path: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            config_path: config_path.into(),
            child: None,
        }
    }

    /// Validates `config`, then writes it to the collector's config file and restarts the process so
    /// it picks the new configuration up. On any failure — a rejected configuration, or a collector
    /// that will not start — it returns a human-readable message, which the caller reports verbatim as
    /// the `error_message` of a `FAILED` remote-config status (specification goal 4). A rejected
    /// configuration is caught by validation *before* the running collector is touched.
    pub async fn apply(&mut self, config: &[u8]) -> Result<(), String> {
        self.validate(config).await?;

        if let Some(dir) = self.config_path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create config directory {}: {e}", dir.display()))?;
        }
        std::fs::write(&self.config_path, config).map_err(|e| {
            format!(
                "cannot write collector config to {}: {e}",
                self.config_path.display()
            )
        })?;
        self.restart().await
    }

    /// Runs the collector's `validate` subcommand against a throwaway copy of `config`. Returns the
    /// collector's own error message if it rejects the configuration. The check never disturbs the
    /// running collector, so a bad push is a reported event rather than an outage.
    async fn validate(&self, config: &[u8]) -> Result<(), String> {
        if let Some(dir) = self.config_path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create config directory {}: {e}", dir.display()))?;
        }
        let candidate = self.config_path.with_extension("candidate.yaml");
        std::fs::write(&candidate, config).map_err(|e| {
            format!(
                "cannot write candidate config to {}: {e}",
                candidate.display()
            )
        })?;

        let result = Command::new(&self.executable)
            .arg("validate")
            .arg("--config")
            .arg(&candidate)
            .output()
            .await;
        let _ = std::fs::remove_file(&candidate);

        let output = result
            .map_err(|e| format!("cannot run '{} validate': {e}", self.executable.display()))?;
        if output.status.success() {
            return Ok(());
        }
        // The collector prints validation errors to stderr; fall back to stdout if it is empty.
        let mut details = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if details.is_empty() {
            details = String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
        Err(format!(
            "collector rejected the configuration: {}",
            tail(&details)
        ))
    }

    /// Stops the current Collector process, if any, and starts a fresh one against the config file.
    async fn restart(&mut self) -> Result<(), String> {
        self.stop().await;
        let child = Command::new(&self.executable)
            .arg("--config")
            .arg(&self.config_path)
            // If the Supervisor exits, the Collector it owns must not outlive it.
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                format!(
                    "cannot start collector {} --config {}: {e}",
                    self.executable.display(),
                    self.config_path.display()
                )
            })?;
        info!(pid = child.id(), executable = %self.executable.display(), "collector started");
        self.child = Some(child);
        Ok(())
    }

    /// Restarts the collector against the configuration already on disk — the last one that applied.
    /// Used to recover from a crash without re-validating or rewriting a config that already passed.
    pub async fn restart_current(&mut self) -> Result<(), String> {
        self.restart().await
    }

    /// The path the collector is launched against — where the applied configuration lives on disk.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Whether a collector process is currently supervised (spawned and not yet observed to have
    /// exited). Not a health signal on its own — a running process may still be unhealthy.
    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }

    /// The collector's version string, read from `<executable> --version`. `None` if the command
    /// cannot be run or does not succeed. Bounded by a short timeout so a binary that ignores the flag
    /// (and, say, starts running) cannot hang supervisor startup.
    pub async fn version(&self) -> Option<String> {
        let output = tokio::time::timeout(
            Duration::from_secs(5),
            Command::new(&self.executable).arg("--version").output(),
        )
        .await
        .ok()? // timed out
        .ok()?; // could not spawn
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        // Collectors print e.g. "otelcol-contrib version 0.156.0"; take the last token of the first
        // line as the version.
        text.lines()
            .next()?
            .split_whitespace()
            .last()
            .map(str::to_string)
    }

    /// If the supervised collector has exited on its own since the last check, returns its exit status
    /// and forgets the process. `None` means it is still running, or there is no collector. Because a
    /// deliberate stop takes the child first, any status seen here is an *unexpected* exit — a crash.
    pub fn check_exited(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.child.as_mut()?.try_wait().ok()??;
        self.child = None;
        Some(status)
    }

    /// Terminates the current Collector process gracefully (SIGTERM, then SIGKILL), waiting for it to
    /// exit so a restart does not race two collectors writing the same telemetry.
    pub async fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            crate::agent::terminate(&mut child).await;
        }
    }
}

/// The tail of a collector error message, bounded (by characters, so it never splits a UTF-8 boundary)
/// so a huge validation dump does not fill a status report. The tail carries the actual error.
fn tail(text: &str) -> String {
    const MAX: usize = 600;
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= MAX {
        return text.to_string();
    }
    format!("…{}", chars[chars.len() - MAX..].iter().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_short_messages_whole() {
        assert_eq!(tail("boom"), "boom");
    }

    #[test]
    fn tail_bounds_long_messages_on_a_char_boundary() {
        let long = "ä".repeat(1000);
        let t = tail(&long);
        // Truncated with a leading ellipsis, and still valid UTF-8 (no panic building it).
        assert!(t.starts_with('…'));
        assert_eq!(t.chars().filter(|&c| c == 'ä').count(), 600);
    }

    #[test]
    fn check_exited_is_none_without_a_collector() {
        let mut collector = Collector::new("/bin/true", "/tmp/opamp-sup-none.yaml");
        assert!(collector.check_exited().is_none());
    }

    #[tokio::test]
    async fn check_exited_reports_a_process_that_died() {
        // `false` exits immediately with a non-zero status: a stand-in for a crashing collector.
        let mut collector = Collector::new("false", "/tmp/opamp-sup-crash.yaml");
        collector.restart_current().await.expect("spawn");
        // Give the short-lived process a moment to exit before we poll it.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let status = collector.check_exited().expect("the process has exited");
        assert!(!status.success(), "a crashed collector exits non-zero");
        // The crash is reported once, then forgotten.
        assert!(collector.check_exited().is_none());
    }
}
