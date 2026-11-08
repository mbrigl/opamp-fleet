//! The audit record's export
//! (ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one
//! clause 9): every entry the store has accepted, sent as an OTLP log record to the OTLP/HTTP
//! endpoint `[audit.export]` names — a copy the Server cannot rewrite once it has left.
//!
//! The writer of the record hands each line it has written to a [`Sink`], which never waits: the
//! line goes into a queue of [`QUEUE`] entries, and one that finds the queue full is counted as
//! dropped. A thread of its own takes the lines out, encodes them with `opentelemetry-otlp`, the
//! crate the Client's own telemetry uses (ADR-0017), and sends them through a `reqwest` client on
//! the TLS 1.3 *ring* configuration (ADR-0012 clauses 5 and 21); a batch the receiver does not take
//! is tried again, until it does, while the queue fills behind it. The count of what was dropped is
//! written into the record itself as `audit.export_dropped` ([`AuditExport::report_dropped`]),
//! about every [`DROPPED_EVERY`] while it is not zero and once at shutdown.

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use opentelemetry::logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity};
use opentelemetry::{InstrumentationScope, KeyValue};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::logs::{LogBatch, LogExporter as _, SdkLogRecord, SdkLoggerProvider};
use tracing::{info, warn};

use crate::audit::{Audit, Entry};
use crate::config::AuditExportConfig;

/// The entries waiting to be exported at most; one more is dropped and counted.
pub const QUEUE: usize = 2048;

/// How often the count of dropped entries is written into the record while it is not zero.
pub const DROPPED_EVERY: Duration = Duration::from_secs(60);

/// The entries sent in one request at most.
const BATCH: usize = 512;

/// How long a connection to the receiver may take to open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one request may take before it is abandoned and tried again.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The wait before a batch the receiver did not take is sent again, doubled up to
/// [`RETRY_MOST`].
const RETRY_FIRST: Duration = Duration::from_secs(1);

/// The longest wait between two attempts at one batch.
const RETRY_MOST: Duration = Duration::from_secs(60);

/// How long [`AuditExport::finish`] waits for the export thread to end once it is told to stop.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// How often the export thread, waiting for a line, looks whether it is told to stop.
const STOP_POLL: Duration = Duration::from_millis(100);

/// The name the records are exported under, as their resource's `service.name` and their scope.
const SERVICE: &str = "opamp-fleet-server";

/// One line of the record, as the store accepted it.
struct Exported {
    event: String,
    line: String,
    time_ms: u64,
}

/// Where the writer of the record hands each line the store has accepted.
#[derive(Clone)]
pub struct Sink {
    tx: SyncSender<Exported>,
    dropped: Arc<AtomicU64>,
    /// The lines handed in and not yet taken by the receiver.
    outstanding: Arc<AtomicUsize>,
}

impl Sink {
    /// Queues one line, `event` the entry's event and `time_ms` its time; counts it as dropped
    /// when the queue is full. Never waits.
    pub(crate) fn offer(&self, event: &str, line: &str, time_ms: u64) {
        self.outstanding.fetch_add(1, Ordering::SeqCst);
        let exported = Exported {
            event: event.to_string(),
            line: line.to_string(),
            time_ms,
        };
        if self.tx.try_send(exported).is_err() {
            self.outstanding.fetch_sub(1, Ordering::SeqCst);
            self.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// The export, started when `[audit.export]` names an endpoint.
pub struct AuditExport {
    sink: Sink,
    /// Set by [`finish`](Self::finish): the thread abandons what it is sending and ends.
    stop: tokio::sync::watch::Sender<bool>,
    /// The export thread, joined by [`finish`](Self::finish) before the runtime it sends on goes.
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl AuditExport {
    /// Builds the exporter for `config` and starts the thread that sends what the [`Sink`] is
    /// handed, its requests driven on `runtime`. Opens no connection until the first entry.
    ///
    /// # Errors
    /// Returns an error naming the section when the endpoint or the `ca_file` cannot be used.
    pub fn start(
        config: &AuditExportConfig,
        runtime: tokio::runtime::Handle,
    ) -> Result<Self, String> {
        let client = ConfiguredOnly(http_client(config)?);
        // The endpoint, the protocol and the timeout set here override the `OTEL_*` variables.
        // No compression is compiled into `opentelemetry-otlp`, so none those variables name is
        // taken up; the headers they name are dropped by the client. The exporter's own retry is
        // off: `run` tries a batch again, and is the one place that waits.
        let mut exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_endpoint(&config.endpoint)
            .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
            .with_timeout(REQUEST_TIMEOUT)
            .with_retry_policy(opentelemetry_otlp::RetryPolicy::disabled())
            .with_http_client(client)
            .build()
            .map_err(|e| format!("[audit.export] cannot build the exporter: {e}"))?;
        exporter.set_resource(
            &opentelemetry_sdk::Resource::builder_empty()
                .with_attributes([KeyValue::new("service.name", SERVICE)])
                .build(),
        );
        let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
        let outstanding = Arc::new(AtomicUsize::new(0));
        let sink = Sink {
            tx,
            dropped: Arc::new(AtomicU64::new(0)),
            outstanding: outstanding.clone(),
        };
        let endpoint = config.endpoint.clone();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        // A thread of its own: it blocks on each request and on the wait before a batch is sent
        // again, and must not hold a runtime worker.
        let thread = std::thread::Builder::new()
            .name("audit-export".to_string())
            .spawn(move || run(&rx, &exporter, &runtime, &outstanding, &endpoint, stopped))
            .map_err(|e| format!("cannot start the audit export: {e}"))?;
        info!(endpoint = %config.endpoint, "exporting the audit record");
        Ok(AuditExport {
            sink,
            stop,
            thread: std::sync::Mutex::new(Some(thread)),
        })
    }

    /// Where the writer of the record hands the lines it has written.
    #[must_use]
    pub fn sink(&self) -> Sink {
        self.sink.clone()
    }

    /// Writes the entries dropped since the last report as `audit.export_dropped` with `count`,
    /// when there are any. A count the record cannot take is kept for the next report.
    pub fn report_dropped(&self, audit: &dyn Audit) {
        let count = self.sink.dropped.swap(0, Ordering::SeqCst);
        if count == 0 {
            return;
        }
        let entry = Entry::new("audit.export_dropped", "recorded").with("count", count);
        if audit.record(entry).is_err() {
            self.sink.dropped.fetch_add(count, Ordering::SeqCst);
        }
    }

    /// Reports the dropped entries every [`DROPPED_EVERY`], for as long as the process runs.
    pub fn report_every(self: &Arc<Self>, audit: Arc<dyn Audit>) {
        let export = self.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(DROPPED_EVERY);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            every.tick().await;
            loop {
                every.tick().await;
                export.report_dropped(audit.as_ref());
            }
        });
    }

    /// At shutdown, once the record is closed: waits until the receiver has taken every line
    /// handed in so far, for at most `timeout`, then stops the export thread, so that no request
    /// of it outlives the runtime it is sent on, and answers what the export leaves behind — the
    /// lines dropped since the last report, which no entry can count any more, and those still
    /// waiting, the batch it abandoned included.
    #[must_use]
    pub fn finish(&self, timeout: Duration) -> Unexported {
        let deadline = Instant::now() + timeout;
        while self.sink.outstanding.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.stop.send_replace(true);
        let thread = self
            .thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(thread) = thread {
            let joined_by = Instant::now() + STOP_WAIT;
            while !thread.is_finished() && Instant::now() < joined_by {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                warn!("the audit export did not stop in time");
            }
        }
        Unexported {
            dropped: self.sink.dropped.swap(0, Ordering::SeqCst),
            waiting: self.sink.outstanding.load(Ordering::SeqCst),
        }
    }
}

/// What the export leaves behind at shutdown ([`AuditExport::finish`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unexported {
    /// Lines dropped since the last report, after the record was closed.
    pub dropped: u64,
    /// Lines handed in that the receiver had not taken when the time ran out.
    pub waiting: usize,
}

/// The headers a request to the receiver may carry: what the OTLP encoding of its body needs.
const SENT_HEADERS: [&str; 2] = ["content-type", "content-encoding"];

/// The client the exporter sends through, which sends no header beyond [`SENT_HEADERS`].
/// `opentelemetry-otlp` adds the headers `OTEL_EXPORTER_OTLP_HEADERS` and
/// `OTEL_EXPORTER_OTLP_LOGS_HEADERS` name to every request, whatever the configuration says; the
/// export takes every setting from `[audit.export]` alone and presents no credential.
#[derive(Debug)]
struct ConfiguredOnly(reqwest::Client);

#[async_trait::async_trait]
impl opentelemetry_http::HttpClient for ConfiguredOnly {
    async fn send_bytes(
        &self,
        mut request: opentelemetry_http::Request<opentelemetry_http::Bytes>,
    ) -> Result<
        opentelemetry_http::Response<opentelemetry_http::Bytes>,
        opentelemetry_http::HttpError,
    > {
        let headers = request.headers_mut();
        let dropped: Vec<_> = headers
            .keys()
            .filter(|name| !SENT_HEADERS.contains(&name.as_str()))
            .cloned()
            .collect();
        for name in dropped {
            headers.remove(name);
        }
        opentelemetry_http::HttpClient::send_bytes(&self.0, request).await
    }
}

/// The client the exporter sends through: TLS 1.3 on the public roots and the optional
/// `ca_file`, plaintext only to the loopback endpoint the configuration checked, no proxy and no
/// redirect, every request bounded.
fn http_client(config: &AuditExportConfig) -> Result<reqwest::Client, String> {
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    if let Some(path) = &config.ca_file {
        add_roots(&mut roots, path)?;
    }
    let tls = opamp::tls::client_builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .https_only(!opamp::endpoint::is_plaintext(&config.endpoint))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("[audit.export] cannot build the HTTP client: {e}"))
}

fn add_roots(roots: &mut rustls::RootCertStore, path: &Path) -> Result<(), String> {
    let named =
        |e: &dyn std::fmt::Display| format!("[audit.export] ca_file {}: {e}", path.display());
    let pem = std::fs::read(path).map_err(|e| named(&e))?;
    for certificate in opamp::tls::certificates(&pem).map_err(|e| named(&e))? {
        roots.add(certificate).map_err(|e| named(&e))?;
    }
    Ok(())
}

/// The export thread: takes up to [`BATCH`] lines at once and sends them until the receiver takes
/// them, then the next.
fn run(
    rx: &Receiver<Exported>,
    exporter: &opentelemetry_otlp::LogExporter,
    runtime: &tokio::runtime::Handle,
    outstanding: &AtomicUsize,
    endpoint: &str,
    mut stopped: tokio::sync::watch::Receiver<bool>,
) {
    // A provider without a processor: it only makes the records, which are sent below.
    let provider = SdkLoggerProvider::builder().build();
    let scope = InstrumentationScope::builder(SERVICE).build();
    let logger = provider.logger_with_scope(scope.clone());
    let mut failing = false;
    loop {
        if *stopped.borrow() {
            return;
        }
        let first = match rx.recv_timeout(STOP_POLL) {
            Ok(first) => first,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let mut taken = vec![first];
        while taken.len() < BATCH {
            match rx.try_recv() {
                Ok(exported) => taken.push(exported),
                Err(_) => break,
            }
        }
        let records: Vec<SdkLogRecord> = taken
            .iter()
            .map(|exported| {
                let mut record = logger.create_log_record();
                record.set_timestamp(UNIX_EPOCH + Duration::from_millis(exported.time_ms));
                record.set_observed_timestamp(SystemTime::now());
                record.set_severity_number(Severity::Info);
                record.set_severity_text("INFO");
                record.set_body(AnyValue::from(exported.line.clone()));
                record.add_attribute("audit.event", exported.event.clone());
                record
            })
            .collect();
        let batch: Vec<(&SdkLogRecord, &InstrumentationScope)> =
            records.iter().map(|record| (record, &scope)).collect();
        let mut wait = RETRY_FIRST;
        loop {
            // The request, and the wait before it is sent again, end as soon as the thread is
            // told to stop: their timers belong to the runtime, which goes after the stop.
            let sent = runtime.block_on(async {
                tokio::select! {
                    sent = exporter.export(LogBatch::new(&batch)) => Some(sent),
                    _ = stopped.wait_for(|stop| *stop) => None,
                }
            });
            let Some(sent) = sent else { return };
            match sent {
                Ok(()) => {
                    if std::mem::take(&mut failing) {
                        info!(endpoint, "the audit export reaches its receiver again");
                    }
                    break;
                }
                Err(e) => {
                    if !std::mem::replace(&mut failing, true) {
                        warn!(endpoint, error = %e, "the audit export does not reach its receiver; \
                              trying again, and counting what its queue drops meanwhile");
                    }
                    let waited = runtime.block_on(async {
                        tokio::select! {
                            () = tokio::time::sleep(wait) => true,
                            _ = stopped.wait_for(|stop| *stop) => false,
                        }
                    });
                    if !waited {
                        return;
                    }
                    wait = (wait * 2).min(RETRY_MOST);
                }
            }
        }
        outstanding.fetch_sub(taken.len(), Ordering::SeqCst);
    }
}

/// What a [`test_sink`] was handed.
#[cfg(test)]
pub(crate) struct Taken(Receiver<Exported>);

#[cfg(test)]
impl Taken {
    /// The lines handed in since the last call, in order.
    pub(crate) fn lines(&self) -> Vec<String> {
        self.0.try_iter().map(|exported| exported.line).collect()
    }
}

/// A sink whose lines the test takes itself, with nothing sent anywhere.
#[cfg(test)]
pub(crate) fn test_sink() -> (Sink, Taken) {
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    let sink = Sink {
        tx,
        dropped: Arc::new(AtomicU64::new(0)),
        outstanding: Arc::new(AtomicUsize::new(0)),
    };
    (sink, Taken(rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entries the record was given.
    #[derive(Default)]
    struct Recorded(std::sync::Mutex<Vec<Entry>>, std::sync::atomic::AtomicBool);

    impl Audit for Recorded {
        fn record(&self, entry: Entry) -> Result<(), crate::audit::Unavailable> {
            if self.1.load(Ordering::SeqCst) {
                return Err(crate::audit::Unavailable);
            }
            self.0.lock().expect("recorded").push(entry);
            Ok(())
        }

        fn reserve(&self) -> Result<crate::audit::Permit, crate::audit::Unavailable> {
            Err(crate::audit::Unavailable)
        }
    }

    /// A full queue drops what comes after it without waiting, counts it, and the count is written
    /// into the record as `audit.export_dropped`; a count the record cannot take is kept for the
    /// next report, and nothing is written while none was dropped.
    /// Verifies: ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one
    #[test]
    fn a_full_queue_drops_counts_and_writes_the_count_into_the_record() {
        let (sink, _taken) = test_sink();
        let export = AuditExport {
            sink: sink.clone(),
            stop: tokio::sync::watch::channel(false).0,
            thread: std::sync::Mutex::new(None),
        };
        let audit = Recorded::default();
        export.report_dropped(&audit);
        assert!(audit.0.lock().expect("recorded").is_empty());

        let started = Instant::now();
        for seq in 0..QUEUE + 5 {
            sink.offer("admission.admitted", &format!("{{\"seq\":{seq}}}"), 0);
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the sink never waits"
        );
        assert_eq!(sink.dropped.load(Ordering::SeqCst), 5);

        audit.1.store(true, Ordering::SeqCst);
        export.report_dropped(&audit);
        audit.1.store(false, Ordering::SeqCst);
        sink.offer("admission.admitted", "{}", 0);
        export.report_dropped(&audit);
        let recorded = audit.0.lock().expect("recorded");
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].event, "audit.export_dropped");
        assert_eq!(
            recorded[0].fields.get("count"),
            Some(&crate::audit::Field::Unsigned(6))
        );
        assert_eq!(sink.dropped.load(Ordering::SeqCst), 0);
    }

    /// At shutdown the export answers what it leaves behind: the lines dropped since the last
    /// report, counted once, and those the receiver had not taken when the time ran out.
    /// Verifies: ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one
    #[test]
    fn at_shutdown_the_export_answers_what_it_leaves_behind() {
        let (sink, _taken) = test_sink();
        let export = AuditExport {
            sink: sink.clone(),
            stop: tokio::sync::watch::channel(false).0,
            thread: std::sync::Mutex::new(None),
        };
        assert_eq!(
            export.finish(Duration::ZERO),
            Unexported {
                dropped: 0,
                waiting: 0
            }
        );
        for _ in 0..QUEUE + 3 {
            sink.offer("audit.chain_closed", "{}", 0);
        }
        assert_eq!(
            export.finish(Duration::from_millis(50)),
            Unexported {
                dropped: 3,
                waiting: QUEUE
            }
        );
        assert_eq!(export.finish(Duration::ZERO).dropped, 0, "counted once");
    }
}
