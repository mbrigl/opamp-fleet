//! The two OpAMP transports (ADR-0007). Both feed the same [`Agent`](crate::agent::Agent) state
//! machine; they differ only in how bytes travel.

pub mod http;
pub mod ws;

use std::time::Duration;

use crate::config::ClientConfig;
use crate::engine::Engine;

/// How often a download in flight is reported as `Downloading` with its details. The Baseline
/// leaves the cadence open; this is slow enough to stay a rounding error next to the transfer and
/// fast enough that an operator watching a rollout sees it move.
const DOWNLOAD_REPORT_INTERVAL: Duration = Duration::from_secs(5);

/// How a transport puts reports on the wire. The two transports differ only in that — a frame on
/// an open socket, or a POST per report — so a long-running job that needs to report while it runs
/// takes one of these rather than learning both.
///
/// `Err(())` means the connection is gone, not that a single report was refused.
// The trait is internal to this binary and none of these futures is ever spawned, so the missing
// `Send` bound the `async_fn_in_trait` lint warns about cannot bite here.
#[allow(async_fn_in_trait)]
pub trait ReportSink {
    async fn send(&mut self, reports: Vec<opamp::proto::AgentToServer>) -> Result<(), ()>;
}

/// Downloads, verifies, and applies any package the Engine has queued (ADR-0015). Each is handled
/// in turn — download and verification are the transport's, the swap is the Supervisor's — and its
/// outcome (`Installed`/`InstallFailed`) is reported back through the Engine. Returns whether any
/// package was processed, so the caller flushes the owed status reports.
///
/// While an artifact is on the wire, interim `Downloading` reports go out through `sink`. A failed
/// interim report is not fatal: the download continues, and the terminal status is reported by the
/// caller on the next exchange.
pub async fn process_package_downloads<S: ReportSink>(
    engine: &mut Engine,
    config: &ClientConfig,
    sink: &mut S,
) -> bool {
    let downloads = engine.take_package_downloads();
    if downloads.is_empty() {
        return false;
    }
    for (index, package) in downloads {
        // Taken before the download borrows the offer for as long as it runs.
        let (name, version, hash) = (
            package.name.clone(),
            package.version.clone(),
            package.hash.clone(),
        );
        let progress = crate::packages::Progress::default();
        let started = std::time::Instant::now();
        let download = crate::packages::download_and_verify(&package, config, &progress);
        tokio::pin!(download);
        // Poll the download and a ticker together: every tick turns the progress the download has
        // been writing into a status report, without the download itself knowing about reporting.
        let result = loop {
            tokio::select! {
                result = &mut download => break result,
                () = tokio::time::sleep(DOWNLOAD_REPORT_INTERVAL) => {
                    engine.package_downloading(index, progress.details(started));
                    let _ = sink.send(engine.owed_reports()).await;
                }
            }
        };
        match result {
            Ok(staged) => engine.apply_package(index, staged, version, hash),
            Err(e) => {
                tracing::warn!(package = %name, error = %e, "package download or verification failed");
                engine.package_download_failed(index, hash, e);
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
    use crate::storage::Storage;
    use crate::supervisor::agent::AgentState;
    use opamp::proto::{
        AgentToServer, DownloadableFile, PackageAvailable, PackageStatusEnum, PackagesAvailable,
        ServerToAgent,
    };
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

    /// A sink that keeps what the transport would have sent.
    struct Recorder(Vec<AgentToServer>);

    impl ReportSink for Recorder {
        async fn send(&mut self, reports: Vec<AgentToServer>) -> Result<(), ()> {
            self.0.extend(reports);
            Ok(())
        }
    }

    /// The Baseline permits interim status reports while a package downloads, and this is what
    /// they are for: a transfer that takes longer than a moment stays visible instead of looking
    /// like a stuck install. Driven by a server that trickles the artifact out.
    #[tokio::test]
    async fn a_slow_download_is_reported_as_downloading_with_progress() {
        let artifact = vec![7u8; 3072];
        let content_hash = Sha256::digest(&artifact).to_vec();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served = artifact.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut scratch = [0u8; 1024];
            let _ = stream.read(&mut scratch).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n",
                served.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            // Three seconds per third: long enough that the reporting tick fires mid-transfer.
            for chunk in served.chunks(1024) {
                let _ = stream.write_all(chunk).await;
                let _ = stream.flush().await;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });

        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let mut state = AgentState::supervised("otelcol".to_string(), storage).expect("agent");
        state.accept_package("otelcol".to_string());
        let mut engine = Engine::new(vec![state]);
        let uid = engine.poll_reports()[0].instance_uid.clone();
        let config = ClientConfig {
            state_dir: dir.path().to_path_buf(),
            ..ClientConfig::default()
        };

        // The offer queues the download the transport then runs.
        engine.handle(&ServerToAgent {
            instance_uid: uid,
            packages_available: Some(PackagesAvailable {
                packages: [(
                    "otelcol".to_string(),
                    PackageAvailable {
                        version: "2.0.0".to_string(),
                        file: Some(DownloadableFile {
                            download_url: format!("http://{addr}/otelcol"),
                            content_hash: content_hash.clone(),
                            ..Default::default()
                        }),
                        hash: b"pkg-hash".to_vec(),
                        ..Default::default()
                    },
                )]
                .into(),
                all_packages_hash: b"agg".to_vec(),
            }),
            ..Default::default()
        });

        let mut sink = Recorder(Vec::new());
        assert!(process_package_downloads(&mut engine, &config, &mut sink).await);

        // At least one interim report went out while the bytes were still arriving, and it says
        // Downloading — with a percentage that actually moved.
        let downloading: Vec<_> = sink
            .0
            .iter()
            .filter_map(|report| report.package_statuses.as_ref())
            .filter_map(|statuses| statuses.packages.get("otelcol"))
            .filter(|status| status.status == PackageStatusEnum::Downloading as i32)
            .collect();
        assert!(
            !downloading.is_empty(),
            "a transfer this slow must be reported while it runs, not only when it ends"
        );
        let details = downloading[0]
            .download_details
            .expect("Downloading carries its details");
        assert!(
            details.download_percent > 0.0 && details.download_percent < 100.0,
            "a partial transfer reports partial progress, got {}",
            details.download_percent
        );
        assert!(details.download_bytes_per_second > 0.0);

        // And the artifact itself arrived intact, verified against its content hash.
        let staged = dir.path().join("packages").join("otelcol.staged");
        assert_eq!(
            std::fs::read(&staged).expect("the staged artifact"),
            artifact
        );
    }
}
