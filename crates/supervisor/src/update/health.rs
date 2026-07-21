//! The self-update health gate (ADR-0007): a two-tier decision that tolerates a Server outage.
//!
//! The freshly started daemon writes a local health file ([`HealthWriter`]); the Updater
//! ([`FileHealthGate`]) resets it before the restart and then polls it. The gate passes when the new
//! version stays up for a settle window AND either it reported Healthy over OpAMP (the strong signal)
//! or the Server is demonstrably unreachable while the process is locally healthy — so a bad binary
//! that never comes up still fails, but a good binary is not rolled back merely because the Server is
//! down.

use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The local health signal the daemon publishes for the Updater to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReport {
    /// PID of the reporting process.
    pub pid: u32,
    /// Version of the running binary.
    pub version: String,
    /// The process came up and is running its report loop.
    pub healthy: bool,
    /// The process completed at least one successful OpAMP round-trip (reported Healthy).
    pub server_reported: bool,
}

/// Writes the health file the gate reads. The daemon owns one of these.
pub struct HealthWriter {
    path: PathBuf,
    pid: u32,
    version: String,
}

impl HealthWriter {
    /// Create a writer targeting `path`.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            pid: std::process::id(),
            version: supervisor::version().to_string(),
        }
    }

    /// Publish the current health, recording whether a successful OpAMP round-trip has happened yet.
    ///
    /// # Errors
    /// Returns an error if the health file cannot be written.
    pub fn publish(&self, server_reported: bool) -> Result<()> {
        let report = HealthReport {
            pid: self.pid,
            version: self.version.clone(),
            healthy: true,
            server_reported,
        };
        let json = serde_json::to_vec(&report).context("serializing the health report")?;
        let staging = self.path.with_extension("tmp");
        std::fs::write(&staging, &json)
            .with_context(|| format!("writing {}", staging.display()))?;
        std::fs::rename(&staging, &self.path)
            .with_context(|| format!("publishing {}", self.path.display()))?;
        Ok(())
    }
}

/// The gate the Updater applies after restarting the new version.
pub trait HealthGate {
    /// Clear any stale health signal so the next one is unambiguously the new process's.
    ///
    /// # Errors
    /// Returns an error on unexpected I/O failure.
    fn reset(&self) -> Result<()>;

    /// Wait up to `settle` for the new version to prove healthy. Returns whether the gate passed.
    ///
    /// # Errors
    /// Returns an error on unexpected I/O failure.
    fn await_healthy(&self, settle: Duration) -> Result<bool>;
}

/// The production gate: reads the daemon's health file, and on a missing Server signal falls back to
/// probing Server reachability (tier two).
pub struct FileHealthGate {
    path: PathBuf,
    endpoint: String,
}

impl FileHealthGate {
    /// Create a gate reading `path`, probing `endpoint` for the tier-two reachability fallback.
    #[must_use]
    pub fn new(path: PathBuf, endpoint: String) -> Self {
        Self { path, endpoint }
    }

    fn read(&self) -> Option<HealthReport> {
        let bytes = std::fs::read(&self.path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

impl HealthGate for FileHealthGate {
    fn reset(&self) -> Result<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err).with_context(|| format!("resetting {}", self.path.display())),
        }
    }

    fn await_healthy(&self, settle: Duration) -> Result<bool> {
        let deadline = Instant::now() + settle;
        let mut ever_healthy = false;
        loop {
            if let Some(report) = self.read() {
                if report.healthy {
                    ever_healthy = true;
                    // Tier one: the new version reported Healthy over OpAMP.
                    if report.server_reported {
                        return Ok(true);
                    }
                }
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(200).min(settle));
        }
        // Tier two: locally healthy but no Server signal — pass only if the Server is unreachable, so
        // a Server outage does not roll back a good binary.
        Ok(ever_healthy && !endpoint_reachable(&self.endpoint, Duration::from_secs(2)))
    }
}

/// Best-effort TCP reachability of the Server endpoint's host:port.
fn endpoint_reachable(endpoint: &str, timeout: Duration) -> bool {
    let after_scheme = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    let mut authority = after_scheme.split('/').next().unwrap_or("").to_string();
    if authority.is_empty() {
        return false;
    }
    if !authority.contains(':') {
        authority.push_str(":80");
    }
    let Ok(addrs) = authority.to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, timeout).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("opamp-health-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    // An endpoint that nothing listens on (reserved TCP port 0 authority → unreachable).
    const UNREACHABLE: &str = "http://127.0.0.1:9/v1/opamp";

    #[test]
    fn passes_immediately_when_server_reported() {
        let tmp = TempDir::new();
        let path = tmp.0.join("health.json");
        let gate = FileHealthGate::new(path.clone(), UNREACHABLE.to_string());
        HealthWriter::new(path).publish(true).unwrap();
        assert!(gate.await_healthy(Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn passes_when_healthy_and_server_unreachable() {
        let tmp = TempDir::new();
        let path = tmp.0.join("health.json");
        let gate = FileHealthGate::new(path.clone(), UNREACHABLE.to_string());
        // Healthy but never reported to the Server; the Server is unreachable → tier two passes.
        HealthWriter::new(path).publish(false).unwrap();
        assert!(gate.await_healthy(Duration::from_millis(400)).unwrap());
    }

    #[test]
    fn fails_when_no_health_appears() {
        let tmp = TempDir::new();
        let path = tmp.0.join("health.json");
        let gate = FileHealthGate::new(path, UNREACHABLE.to_string());
        gate.reset().unwrap();
        assert!(!gate.await_healthy(Duration::from_millis(400)).unwrap());
    }

    #[test]
    fn reset_removes_stale_health() {
        let tmp = TempDir::new();
        let path = tmp.0.join("health.json");
        HealthWriter::new(path.clone()).publish(true).unwrap();
        let gate = FileHealthGate::new(path.clone(), UNREACHABLE.to_string());
        gate.reset().unwrap();
        assert!(!path.exists());
    }
}
