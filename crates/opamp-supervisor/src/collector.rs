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

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, Command};
use tracing::info;

/// The largest crash-log snippet the supervisor will capture, in KiB — the same ceiling the Go
/// supervisor caps `collector_crash_log_snippet_kib` at, so a huge log cannot flood a health report.
pub const MAX_CRASH_LOG_KIB: usize = 1024;

/// A supervised OpenTelemetry Collector: an executable and the config file it is launched against.
pub struct Collector {
    executable: PathBuf,
    config_path: PathBuf,
    child: Option<Child>,
    /// How much of the collector's most recent stderr to include in a crash report, in KiB. `0` (the
    /// default, matching the Go supervisor) disables capture: the collector inherits the supervisor's
    /// stderr as before. When non-zero, the collector's stderr is redirected to a log file next to its
    /// config, truncated on each (re)start, and its tail is read back on an unexpected exit.
    crash_log_kib: usize,
}

impl Collector {
    pub fn new(executable: impl Into<PathBuf>, config_path: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            config_path: config_path.into(),
            child: None,
            crash_log_kib: 0,
        }
    }

    /// Enables capturing up to `kib` KiB of the collector's stderr, included in the crash report when the
    /// collector exits unexpectedly (`collector_crash_log_snippet_kib`, ADR-0008 Go-reference parity).
    /// `0` disables it; the value is clamped to [`MAX_CRASH_LOG_KIB`].
    pub fn with_crash_log_snippet(mut self, kib: usize) -> Self {
        self.crash_log_kib = kib.min(MAX_CRASH_LOG_KIB);
        self
    }

    /// Where the collector's stderr is captured when crash-log snippets are enabled: a file next to the
    /// config it runs against.
    fn log_path(&self) -> PathBuf {
        self.config_path.with_extension("stderr.log")
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
        let mut command = Command::new(&self.executable);
        command
            .arg("--config")
            .arg(&self.config_path)
            // If the Supervisor exits, the Collector it owns must not outlive it.
            .kill_on_drop(true);
        // Capture stderr to a fresh log file when crash-log snippets are enabled, so its tail can be
        // read back on an unexpected exit; otherwise the collector inherits the supervisor's stderr.
        if self.crash_log_kib > 0 {
            command.stderr(self.open_log_file()?);
        }
        let child = command.spawn().map_err(|e| {
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

    /// Opens (creating, truncating) the crash-log file for a fresh collector process, so the captured
    /// stderr reflects only the current run. The parent directory is created by [`Collector::apply`].
    fn open_log_file(&self) -> Result<Stdio, String> {
        let path = self.log_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("cannot create log directory {}: {e}", dir.display()))?;
        }
        std::fs::File::create(&path)
            .map(Stdio::from)
            .map_err(|e| format!("cannot open collector log file {}: {e}", path.display()))
    }

    /// The tail of the collector's captured stderr — up to the configured KiB — for enriching a crash
    /// report, or `None` when capture is disabled or nothing was captured (ADR-0008 Go-reference parity).
    pub fn crash_log_tail(&self) -> Option<String> {
        if self.crash_log_kib == 0 {
            return None;
        }
        read_log_tail(&self.log_path(), self.crash_log_kib * 1024)
    }

    /// Restarts the collector against the configuration already on disk — the last one that applied.
    /// Used to recover from a crash without re-validating or rewriting a config that already passed.
    pub async fn restart_current(&mut self) -> Result<(), String> {
        self.restart().await
    }

    /// Where the previous executable is kept while a package update is in place, so a failed update can
    /// be rolled back to it ([`Collector::rollback_binary`], ADR-0018).
    fn backup_path(&self) -> PathBuf {
        self.executable.with_extension("previous")
    }

    /// Replaces the collector executable with `binary` and restarts onto it, keeping the previous
    /// executable as a backup so [`Collector::rollback_binary`] can revert (ADR-0018). The new binary is
    /// staged in the executable's directory and swapped in with an atomic rename, so a reader never sees a
    /// half-written file. The executable must live in a directory the supervisor can write — a bare
    /// `PATH` name (no parent directory) is rejected, since there is nothing to swap in place. On any
    /// failure the running collector is left untouched (the swap happens before the restart).
    pub async fn install_binary(&mut self, binary: &[u8]) -> Result<(), String> {
        let dir = self.executable.parent().filter(|d| !d.as_os_str().is_empty()).ok_or_else(|| {
            format!(
                "cannot install a package for collector {}: it has no writable directory (configure an absolute path)",
                self.executable.display()
            )
        })?;

        let staged = self.executable.with_extension("incoming");
        std::fs::write(&staged, binary).map_err(|e| {
            format!(
                "cannot stage the new collector binary in {}: {e}",
                dir.display()
            )
        })?;
        // A binary must be executable; mirror a typical 0755 so the supervisor (and only it, for write)
        // can run the swapped-in collector.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
            {
                let _ = std::fs::remove_file(&staged);
                return Err(format!(
                    "cannot make the new collector binary executable: {e}"
                ));
            }
        }

        // Stop the current process before moving its executable aside, then swap the new binary in and
        // start it. Keeping the old executable as a backup is what makes rollback possible.
        self.stop().await;
        let backup = self.backup_path();
        if self.executable.exists() {
            std::fs::rename(&self.executable, &backup).map_err(|e| {
                let _ = std::fs::remove_file(&staged);
                format!("cannot back up the current collector binary: {e}")
            })?;
        }
        if let Err(e) = std::fs::rename(&staged, &self.executable) {
            // Put the backup back so the collector can still start on its previous binary.
            let _ = std::fs::rename(&backup, &self.executable);
            let _ = std::fs::remove_file(&staged);
            return Err(format!("cannot install the new collector binary: {e}"));
        }
        self.restart().await
    }

    /// Restores the executable backed up by the last [`Collector::install_binary`] and restarts onto it —
    /// the rollback when a freshly installed binary does not become healthy (ADR-0018). `Err` if there is
    /// no backup to restore.
    pub async fn rollback_binary(&mut self) -> Result<(), String> {
        let backup = self.backup_path();
        if !backup.exists() {
            return Err("no previous collector binary to roll back to".to_string());
        }
        self.stop().await;
        std::fs::rename(&backup, &self.executable)
            .map_err(|e| format!("cannot restore the previous collector binary: {e}"))?;
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

/// Reads the last `max_bytes` bytes of a file as text, or `None` if it cannot be read or is empty.
/// Bytes (not characters) are bounded — the collector's stderr may not split cleanly — so the result is
/// decoded lossily, and any partial leading line is left in place (the tail is a best-effort snippet).
fn read_log_tail(path: &Path, max_bytes: usize) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf).trim().to_string();
    (!text.is_empty()).then_some(text)
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

    #[test]
    fn read_log_tail_bounds_and_trims() {
        let path = std::env::temp_dir().join("opamp-sup-tail-bounds.log");
        std::fs::write(&path, b"  line-a\nline-b\nline-c\n  ").unwrap();
        // The whole (trimmed) content fits under a generous bound.
        assert_eq!(
            read_log_tail(&path, 4096).as_deref(),
            Some("line-a\nline-b\nline-c")
        );
        // A tight bound keeps only the trailing bytes (then trims) — cutting mid-line is fine, the tail
        // is a best-effort snippet — and never the head.
        let tail = read_log_tail(&path, 8).unwrap();
        assert!(
            tail.ends_with("ine-c"),
            "kept the trailing bytes, got {tail:?}"
        );
        assert!(
            !tail.contains("line-a") && !tail.contains("line-b"),
            "dropped the head, got {tail:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn read_log_tail_is_none_for_missing_or_empty() {
        assert!(read_log_tail(Path::new("/tmp/opamp-sup-tail-absent.log"), 4096).is_none());
        let empty = std::env::temp_dir().join("opamp-sup-tail-empty.log");
        std::fs::write(&empty, b"   \n").unwrap();
        assert!(
            read_log_tail(&empty, 4096).is_none(),
            "whitespace-only is empty"
        );
        let _ = std::fs::remove_file(&empty);
    }

    #[test]
    fn crash_log_tail_is_none_when_capture_disabled() {
        let collector = Collector::new("/bin/true", "/tmp/opamp-sup-nolog.yaml");
        assert!(collector.crash_log_tail().is_none());
    }

    #[tokio::test]
    async fn crash_log_tail_captures_a_failing_collectors_stderr() {
        // `cat --config <path>` is rejected by GNU coreutils with an error on stderr and a non-zero
        // exit — a stand-in for a collector that crashes after writing to its log.
        let config = std::env::temp_dir().join("opamp-sup-crashlog.yaml");
        let mut collector = Collector::new("cat", &config).with_crash_log_snippet(64);
        collector.restart_current().await.expect("spawn");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let status = collector.check_exited().expect("the process has exited");
        assert!(!status.success(), "an unknown flag makes it exit non-zero");
        let tail = collector.crash_log_tail().expect("captured stderr");
        assert!(
            tail.contains("config"),
            "the tail carries the error, got {tail:?}"
        );
        let _ = std::fs::remove_file(collector.log_path());
    }

    #[tokio::test]
    async fn install_binary_swaps_backs_up_and_rollback_restores() {
        let seq = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("opamp-sup-binswap-{}-{}", std::process::id(), seq));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("collector");
        let original = b"#!/bin/sh\nexit 0\n";
        let updated = b"#!/bin/sh\nexit 3\n";
        std::fs::write(&exe, original).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let mut collector = Collector::new(&exe, dir.join("config.yaml"));
        collector
            .install_binary(updated)
            .await
            .expect("install the new binary");
        // The executable now carries the new bytes, and the previous one is kept for rollback.
        assert_eq!(std::fs::read(&exe).unwrap(), updated);
        assert_eq!(
            std::fs::read(exe.with_extension("previous")).unwrap(),
            original,
            "the previous binary is backed up"
        );

        collector
            .rollback_binary()
            .await
            .expect("roll back to the previous binary");
        assert_eq!(
            std::fs::read(&exe).unwrap(),
            original,
            "rollback restores the original"
        );
        collector.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn install_binary_rejects_a_bare_executable_name() {
        // A bare PATH name has no writable directory to swap in — installing must fail cleanly.
        let mut collector = Collector::new("otelcol-contrib", "/tmp/opamp-sup-bare.yaml");
        assert!(collector.install_binary(b"x").await.is_err());
    }

    #[tokio::test]
    async fn rollback_binary_without_a_backup_errors() {
        let mut collector = Collector::new("/tmp/opamp-sup-nobackup", "/tmp/opamp-sup-nb.yaml");
        assert!(collector.rollback_binary().await.is_err());
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
