//! The two OpAMP transports (ADR-0007). Both feed the same [`Agent`](crate::agent::Agent) state
//! machine; they differ only in how bytes travel.

pub mod http;
pub mod ws;

use std::time::Duration;

use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

use crate::config::ClientConfig;
use crate::engine::Engine;

/// The close the Baseline names for a message past the size limit: 1009, Message Too Big.
///
/// Both sockets this Client owns send it — the upstream one it dials and the Supervisor Endpoint it
/// serves — and they sent identical copies of it until ADR-0044. The sentence itself is the
/// Server's too, and lives in `opamp::frame`.
pub(crate) fn too_big_close() -> CloseFrame {
    CloseFrame {
        code: CloseCode::Size,
        reason: opamp::frame::TOO_BIG_CLOSE_REASON.into(),
    }
}

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
        // Each Agent stages into its own directory (ADR-0021), so the Supervisor's install is a
        // rename beside the download rather than a copy across filesystems. Keyed by the block
        // name behind the Agent, never by index — the Agent set can change at runtime (ADR-0056).
        let staging_dir = config.staging_dir_for(engine.block_name(index));
        let download =
            crate::packages::download_and_verify(&package, config, &staging_dir, &progress);
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
            Ok(staged) => {
                engine.apply_package(index, staged, version, hash);
                if engine.restart_for_update() {
                    // A self-update moved the `current` pointer (ADR-0020). Whatever else was
                    // queued is moot: this process is about to be replaced, and the caller ends
                    // the run once the owed `Installing` has gone out.
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(package = %name, error = %e, "package download or verification failed");
                engine.package_download_failed(index, hash, e);
            }
        }
    }
    true
}

/// Applies the self-Agent's received configuration — its Supervisor set (ADR-0056) — if one is
/// pending, and sends the retired Agents' goodbyes through `sink`. Returns whether an apply ran,
/// so the caller flushes the owed status reports.
pub async fn process_self_configuration<S: ReportSink>(
    engine: &mut Engine,
    config: &mut ClientConfig,
    shutdown: &crate::service::runtime::Shutdown,
    sink: &mut S,
) -> bool {
    let Some(offer) = engine.take_self_config() else {
        return false;
    };
    let goodbyes = crate::reconfigure::apply(engine, config, offer, shutdown).await;
    if !goodbyes.is_empty() {
        let _ = sink.send(goodbyes).await;
    }
    true
}

/// What handling a connection-settings offer asks of the transport loop.
#[derive(Debug, PartialEq, Eq)]
pub enum OfferOutcome {
    /// No offer was pending.
    None,
    /// The offer was applied — or refused — in place. The acknowledgement is owed, and the
    /// connection stays up. A telemetry-only offer always lands here (ADR-0086 clause 4): no
    /// destination it names is reached over the OpAMP connection, so there is nothing to reconnect
    /// for.
    Applied,
    /// Verified OpAMP settings took effect (ADR-0014): the caller drops the connection so the
    /// runtime re-resolves the effective configuration and reconnects with them.
    Reconnect,
}

/// Handles a pending connection-settings offer, whichever transport is carrying it.
///
/// The two transports differ in how they end a connection, not in what an offer means — so the
/// meaning lives here, once. Before ADR-0086 both carried a byte-identical copy of this, and both
/// assumed every offer had to be proved by reconnecting.
///
/// The order of the steps is load-bearing:
///
/// 1. **The OpAMP half, only when there is one.** It is verified by actually connecting, which is
///    ADR-0014's MUST and is scoped to this half alone — the Baseline puts that requirement under
///    `ConnectionSettingsOffers.opamp` and justifies it by not losing access to the *Server*. A
///    telemetry destination cannot be proved that way and is not: a receiver that is momentarily
///    down is not an offer that is wrong.
/// 2. **Persist, then apply telemetry from what was persisted** — never from the raw offer. An
///    offer that says nothing about telemetry — an endpoint move, a credential rotation, a
///    certificate — would otherwise compare against `metrics: None, traces: None, logs: None` and
///    tear down exporters the Server never mentioned. `merge` is what puts those back. What it no
///    longer puts back is a signal left out of an offer that *does* name one: that is a stop, and
///    ADR-0089 is where the difference is decided.
/// 3. **One acknowledgement for the whole message** (ADR-0086 clause 3). The Baseline hashes all
///    settings together, so the Agent answers the message, not its parts: a single status whose
///    `error_message` names everything dropped across both halves.
///
/// A failed verification of the OpAMP half persists and applies **nothing**, telemetry included, and
/// reports `FAILED`. Half-applying an offer whose other half was rejected would leave the Server
/// unable to tell what is running.
pub async fn process_connection_offer(
    engine: &mut Engine,
    config: &ClientConfig,
    telemetry: &crate::telemetry::Telemetry,
) -> OfferOutcome {
    let Some(offer) = engine.take_connection_offer() else {
        return OfferOutcome::None;
    };
    let mut errors: Vec<String> = Vec::new();
    let mut reconnect = false;

    if let Some(settings) = &offer.opamp {
        let probe = || engine.probe_report();
        if let Err(e) = crate::connection::verify(settings, config, probe).await {
            tracing::warn!(error = %e, "offered connection settings failed verification");
            engine.connection_settings_outcome(&offer.hash, Err(&e));
            return OfferOutcome::Applied;
        }
        // The issued certificate is stored only now, after connecting with it proved it works — the
        // old one stayed in force until here (ADR-0035).
        if let Some(certificate) = &settings.certificate {
            if let Err(e) = crate::csr::accept(&config.state_dir, &certificate.cert) {
                tracing::warn!(error = %e, "cannot store the issued certificate");
            } else {
                tracing::info!("a client certificate was issued and is now in force");
            }
        }
        if let Err(e) = crate::connection::unhonoured(settings) {
            tracing::warn!(error = %e, "connection settings partly applied");
            errors.push(e);
        }
        reconnect = true;
    }

    let merged =
        crate::connection::merge(crate::connection::load(&config.state_dir).as_ref(), &offer);
    if let Err(e) = crate::connection::store(&config.state_dir, &merged) {
        tracing::warn!(error = %e, "cannot persist the connection settings");
    }
    errors.extend(telemetry.apply(&merged, &engine.self_description(), config));

    if errors.is_empty() {
        engine.connection_settings_outcome(&offer.hash, Ok(()));
    } else {
        let error = errors.join("; ");
        tracing::warn!(error = %error, "connection settings partly applied");
        engine.connection_settings_outcome(&offer.hash, Err(&error));
    }

    if reconnect {
        tracing::info!("connection settings verified; reconnecting with them");
        OfferOutcome::Reconnect
    } else {
        tracing::info!("telemetry destinations applied");
        OfferOutcome::Applied
    }
}

/// Why a transport run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The operator stopped the Client; processes are down, goodbyes sent.
    Shutdown,
    /// Verified connection settings took effect (ADR-0014): the runtime re-resolves the
    /// effective configuration and reconnects — possibly on the other transport.
    Reconfigured,
    /// A self-update installed a new version of the Client and moved the `current` pointer
    /// (ADR-0020). The run ends here and the process exits asking for a restart; what comes back
    /// up is the new version, which reports the outcome.
    RestartForUpdate,
}

/// Reconnect backoff: exponential from one second, capped at a minute, **with jitter**.
///
/// The Baseline asks for the jitter by name — *"use exponential backoff strategy with jitter to
/// avoid overwhelming the Server"* — and the reason is a fleet, not one Client: after a Server
/// restart every Agent reconnects at once, and a deterministic 1 s, 2 s, 4 s ladder has all of them
/// arrive on the same instants. `HARDENING.md` names that reconnect storm under H16; this is its
/// Client half.
///
/// The shape is *equal jitter*: half the ceiling plus a random share of the other half. Full jitter
/// — a uniform draw over the whole interval — spreads marginally better but lets a retry land at
/// nearly zero, which for a fleet reconnecting together is the very burst being avoided. This keeps
/// a floor and still decorrelates.
///
/// `pub(crate)` deliberately: nothing outside this crate has a use for it, and ADR-0024 widens
/// visibility by need rather than by default. Left `pub` it would be a public type whose `new`
/// takes no arguments, which is a `Default` this crate would then have to keep meaning something.
pub(crate) struct Backoff {
    /// The ceiling of the next delay, before jitter. Doubles per failure, capped.
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

    /// The delay to wait now — somewhere in `[ceiling/2, ceiling)`; subsequent failures wait longer.
    pub fn advance(&mut self) -> Duration {
        let ceiling = self.next;
        self.next = (self.next * 2).min(Self::CAP);
        let half = ceiling / 2;
        half + Duration::from_nanos(random_below(half.as_nanos() as u64))
    }
}

/// A uniform draw over `[0, bound)`, from the system's randomness — `ring` is already linked for
/// package signature verification, so this needs no new dependency for a handful of jitter bits.
///
/// A randomness source that will not answer is not worth failing a reconnect over: the fallback is
/// `0`, which degrades the delay to `ceiling/2` — still a valid backoff, just without the spread.
fn random_below(bound: u64) -> u64 {
    use ring::rand::SecureRandom;
    if bound == 0 {
        return 0;
    }
    let mut bytes = [0u8; 8];
    match ring::rand::SystemRandom::new().fill(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes) % bound,
        Err(_) => 0,
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

    /// The ceiling still doubles and still caps; each delay lands in the lower-bounded half of its
    /// ceiling. Bounds rather than exact values, because the jitter is the point.
    #[test]
    fn backoff_doubles_and_caps_within_its_jittered_bounds() {
        // `half + rand(0, half)` never reaches `half + half`, so the ceiling is exclusive.
        let within = |delay: Duration, ceiling: Duration| delay >= ceiling / 2 && delay < ceiling;

        let mut backoff = Backoff::new();
        let first = backoff.advance();
        assert!(within(first, Duration::from_secs(1)), "{first:?}");
        let second = backoff.advance();
        assert!(within(second, Duration::from_secs(2)), "{second:?}");
        for _ in 0..10 {
            backoff.advance();
        }
        // Capped: the ceiling stops at a minute, so every later delay is in [30 s, 60 s).
        for _ in 0..3 {
            let capped = backoff.advance();
            assert!(within(capped, Duration::from_secs(60)), "{capped:?}");
        }
        backoff.reset();
        let after_reset = backoff.advance();
        assert!(
            within(after_reset, Duration::from_secs(1)),
            "{after_reset:?}"
        );
    }

    /// An Engine holding one self-Agent, and the config that goes with it.
    fn engine_with_state_dir(dir: &tempfile::TempDir) -> (Engine, ClientConfig, Vec<u8>) {
        let storage = Storage::new(dir.path().to_path_buf()).expect("storage");
        let state = AgentState::new("self".to_string(), storage).expect("agent");
        let mut engine = Engine::new(vec![state]);
        let uid = engine.poll_reports()[0].instance_uid.clone();
        let config = ClientConfig {
            state_dir: dir.path().to_path_buf(),
            ..ClientConfig::default()
        };
        (engine, config, uid)
    }

    fn telemetry_only_offer(uid: Vec<u8>, endpoint: &str) -> ServerToAgent {
        ServerToAgent {
            instance_uid: uid,
            connection_settings: Some(opamp::proto::ConnectionSettingsOffers {
                hash: b"telemetry-1".to_vec(),
                own_metrics: Some(opamp::proto::TelemetryConnectionSettings {
                    destination_endpoint: endpoint.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// ADR-0086: an offer carrying only a telemetry destination is applied **in place** — no
    /// verification by connecting, no reconnect — and acknowledged. Before it, the Client required
    /// `opamp` to be present and dropped this message whole: no `APPLYING`, no status, no
    /// exporters, and a Server whose hash gate therefore never closed and re-offered for ever.
    #[tokio::test]
    async fn a_telemetry_only_offer_is_applied_without_reconnecting() {
        crate::tls::install_ring_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, config, uid) = engine_with_state_dir(&dir);
        // Loopback is the cleartext exception (ADR-0036) — nothing leaves the machine.
        engine.handle(&telemetry_only_offer(
            uid,
            "http://127.0.0.1:4318/v1/metrics",
        ));

        let telemetry = crate::telemetry::Telemetry::new();
        let outcome = process_connection_offer(&mut engine, &config, &telemetry).await;

        assert_eq!(
            outcome,
            OfferOutcome::Applied,
            "a telemetry destination is not reached over the OpAMP connection, so nothing is \
             gained by dropping it"
        );
        assert!(telemetry.reporting(), "the exporter is in force");

        // Persisted — and honestly: the Server offered no OpAMP settings, so none are claimed.
        let stored = crate::connection::load(&config.state_dir).expect("settings persisted");
        assert!(stored.own_metrics.is_some());
        assert!(
            stored.opamp.is_none(),
            "a synthetic empty block would claim an offer that was never made"
        );

        // And the Server is told, which is what closes its gate.
        let status = engine.owed_reports()[0]
            .connection_settings_status
            .clone()
            .expect("an acknowledgement is owed");
        assert_eq!(status.last_connection_settings_hash, b"telemetry-1");
        assert_eq!(
            status.status,
            opamp::proto::ConnectionSettingsStatuses::Applied as i32,
            "{}",
            status.error_message
        );
        telemetry.shutdown();
    }

    /// And a destination this Client refuses is reported `FAILED` naming the reason, on the same
    /// offer — not warned to a log while the Server is told everything applied.
    #[tokio::test]
    async fn a_refused_telemetry_destination_is_reported_failed_on_the_same_offer() {
        crate::tls::install_ring_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, config, uid) = engine_with_state_dir(&dir);
        // Cleartext to a public host name: the Baseline's "MAY refuse", taken (ADR-0088).
        engine.handle(&telemetry_only_offer(
            uid,
            "http://collector.example:4318/v1/metrics",
        ));

        let telemetry = crate::telemetry::Telemetry::new();
        let outcome = process_connection_offer(&mut engine, &config, &telemetry).await;

        assert_eq!(outcome, OfferOutcome::Applied);
        assert!(!telemetry.reporting(), "the destination was refused");

        let status = engine.owed_reports()[0]
            .connection_settings_status
            .clone()
            .expect("an acknowledgement is owed");
        assert_eq!(
            status.status,
            opamp::proto::ConnectionSettingsStatuses::Failed as i32
        );
        assert!(
            status.error_message.contains("cleartext"),
            "the Server must learn *why*: {}",
            status.error_message
        );
    }

    /// The whole point of the jitter: two Clients failing at the same instant do not come back on
    /// the same instant. Drawn over enough attempts that an accidental tie is not a flake — with a
    /// nanosecond-resolution draw over half a second, ten matching pairs is not something that
    /// happens.
    #[test]
    fn two_backoffs_do_not_produce_the_same_ladder() {
        let mut one = Backoff::new();
        let mut other = Backoff::new();
        let differs = (0..10).any(|_| one.advance() != other.advance());
        assert!(
            differs,
            "a deterministic ladder is the reconnect storm this jitter exists to break"
        );
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
        crate::tls::install_ring_provider();
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
        let mut state =
            AgentState::supervised("otelcol".to_string(), "otelcol".to_string(), storage)
                .expect("agent");
        state.accept_packages();
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
