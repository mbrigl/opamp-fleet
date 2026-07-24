//! The two OpAMP transports (ADR-0007). Both feed the same [`Agent`](crate::agent::Agent) state
//! machine; they differ only in how bytes travel.

pub mod http;
pub mod ws;

use std::time::Duration;

use crate::config::ClientConfig;
use crate::engine::Engine;

/// Downloads, verifies, and applies any package the Engine has queued (ADR-0015). Each is handled
/// in turn — download and verification are the transport's, the swap is the Supervisor's — and its
/// outcome (`Installed`/`InstallFailed`) is reported back through the Engine. Returns whether any
/// package was processed, so the caller flushes the owed status reports.
pub async fn process_package_downloads(engine: &mut Engine, config: &ClientConfig) -> bool {
    let downloads = engine.take_package_downloads();
    if downloads.is_empty() {
        return false;
    }
    for (index, package) in downloads {
        match crate::packages::download_and_verify(&package, config).await {
            Ok(staged) => {
                engine.apply_package(index, staged, package.version, package.hash);
            }
            Err(e) => {
                tracing::warn!(package = %package.name, error = %e, "package download or verification failed");
                engine.package_download_failed(index, package.hash, e);
            }
        }
    }
    true
}

/// Why a transport run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The operator stopped the Client; processes are down, goodbyes sent.
    Shutdown,
    /// Verified connection settings took effect (ADR-0014): the runtime re-resolves the
    /// effective configuration and reconnects — possibly on the other transport.
    Reconfigured,
}

/// Reconnect backoff: exponential from one second, capped at a minute.
pub struct Backoff {
    next: Duration,
}

impl Backoff {
    const START: Duration = Duration::from_secs(1);
    const CAP: Duration = Duration::from_secs(60);

    pub fn new() -> Self {
        Backoff { next: Self::START }
    }

    pub fn reset(&mut self) {
        self.next = Self::START;
    }

    /// The delay to wait now; subsequent failures wait longer.
    pub fn advance(&mut self) -> Duration {
        let current = self.next;
        self.next = (self.next * 2).min(Self::CAP);
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let mut backoff = Backoff::new();
        assert_eq!(backoff.advance(), Duration::from_secs(1));
        assert_eq!(backoff.advance(), Duration::from_secs(2));
        for _ in 0..10 {
            backoff.advance();
        }
        assert_eq!(backoff.advance(), Duration::from_secs(60));
        backoff.reset();
        assert_eq!(backoff.advance(), Duration::from_secs(1));
    }
}
