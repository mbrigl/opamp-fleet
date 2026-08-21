//! The Agent's own telemetry (ADR-0036): OTLP/HTTP to the destinations the Server names.
//!
//! Three capabilities, one mechanism. `ReportsOwnMetrics`, `ReportsOwnTraces`, and `ReportsOwnLogs`
//! each mean "the Agent can report own <signal> to the destination specified by the Server via
//! `ConnectionSettingsOffers.own_*`" — so nothing here is configured in `supervisor.toml`, and with no
//! destination offered this module builds nothing and costs nothing.
//!
//! What "own" means here is what this Client can honestly observe: its own process, and the
//! Managed Processes it spawned and holds the pids of. It does **not** reach into a Collector's
//! internal telemetry the way upstream's supervisor does — ADR-0011 forbids this Client from
//! touching a Managed Process's configuration, and the specification's non-goal forbids inventing
//! an abstraction over it.
//!
//! The wire format is the standard's own implementation rather than a copy of its schema: the
//! exporters come from `opentelemetry-otlp` over HTTP with protobuf bodies, which is what the
//! Baseline's `destination_endpoint` requires, and the names come from
//! `opentelemetry-semantic-conventions` rather than string literals of this project's own.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::time::Duration;

use opamp::attributes::{string_value, SERVICE_INSTANCE_NAME};
use opamp::proto::{AgentDescription, ConnectionSettingsOffers, TelemetryConnectionSettings};
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::{
    LogExporter, MetricExporter, SpanExporter, WithExportConfig, WithHttpConfig,
};
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use opentelemetry_sdk::Resource;
use tracing::{info, warn};

use crate::config::ClientConfig;

/// How often process metrics are sampled, and how often the exporter ships them.
///
/// The Baseline's recommendation for own metrics, taken as written: *"The Agent SHOULD periodically
/// report its metrics to the destination offered in the own_metrics field. The recommended
/// reporting interval is 10 seconds."*
///
/// One constant for both halves on purpose. What reaches the backend is only as fresh as the
/// sampling behind it: exporting every 10 s off a 30 s sample would ship each value three times and
/// turn a gauge series into a step function — a reporting interval in name only. The SDK's periodic
/// reader defaults to 60 s and must therefore be told this explicitly; leaving it at the default is
/// how the interval silently became six times the recommendation.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// How long one export may take before it is abandoned.
///
/// Nothing else on the export path bounds it. The SDK's `PeriodicReader` says so in its own
/// documentation — *"does **not** enforce a timeout for exports … If an export operation never
/// returns, `PeriodicReader` will **stop exporting new metrics**"* — and the batch processors
/// behind traces and logs block on their export the same way, then drop records once their queue
/// fills. `opentelemetry-otlp` resolves a timeout but applies it only to the HTTP client it builds
/// itself; the one handed to it through `with_http_client` keeps whatever bound it was built with,
/// and `reqwest`'s default is none. A destination that stops answering rather than refusing — a
/// host asleep, a NAT that dropped the mapping, a network gone dark — therefore left the exporter
/// thread blocked on a socket that never closes, and own telemetry stayed dead until this Client
/// was restarted. That is the failure this bound exists for: refusal already recovers by itself,
/// since OTLP/HTTP is a fresh request per interval and the next one simply succeeds.
///
/// Half the reporting interval, so a stalled export gives up in time for the next one to be tried
/// on schedule. A bound at the interval would have every cycle finish late, and the reader answers
/// a late export by running the next one immediately — the stall would turn into a queue.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(SAMPLE_INTERVAL.as_secs() / 2);

/// The instrumentation scope every signal this Client emits is attributed to.
const SCOPE: &str = "opamp-fleet-client";

/// The layer the `tracing` subscriber reserves for the OTLP bridge, so a destination that arrives
/// at runtime has somewhere to go (ADR-0036).
///
/// A global, because the subscriber it belongs to is one: `tracing` has exactly one, installed
/// before anything is configured, and the bridge cannot be added to it afterwards without a slot
/// held open from the start. Nothing else here is global — the providers are owned.
type BridgeLayer = Option<
    opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge<
        SdkLoggerProvider,
        opentelemetry_sdk::logs::SdkLogger,
    >,
>;
static BRIDGE: std::sync::OnceLock<
    tracing_subscriber::reload::Handle<BridgeLayer, tracing_subscriber::Registry>,
> = std::sync::OnceLock::new();

/// Hands this module the slot the subscriber reserved. Called once, from the binary's startup.
pub fn hold_log_bridge(
    handle: tracing_subscriber::reload::Handle<BridgeLayer, tracing_subscriber::Registry>,
) {
    let _ = BRIDGE.set(handle);
}

fn set_bridge(provider: Option<&SdkLoggerProvider>) {
    let Some(handle) = BRIDGE.get() else {
        return;
    };
    let layer = provider.map(|provider| {
        opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(provider)
    });
    if let Err(e) = handle.modify(|slot| *slot = layer) {
        warn!(error = %e, "cannot install the OTLP log bridge");
    }
}

/// One Agent's own metrics: which Agent a series belongs to, and the process to read it from.
///
/// Both names travel together because neither answers on its own. The uid is the identity the
/// protocol keys everything by, and the instance name is the only part of it an operator recognises
/// (ADR-0033) — a series labelled with one and not the other is either unreadable or ambiguous.
#[derive(Clone)]
pub struct SamplingTarget {
    /// The Agent's `service.instance.id`.
    pub uid: String,
    /// The operator's name for it.
    pub instance_name: String,
    /// The process to sample: this one for the Client's own Agent, the Managed Process for a
    /// Supervisor-backed one.
    pub pid: u32,
}

/// The providers currently in force, if any.
///
/// Held rather than left to the SDK's globals because the destination is not a startup decision: it
/// arrives from the Server and can change. Applying a new offer means building fresh providers and
/// shutting these down, which needs a handle on exactly what is running.
///
/// **Interior mutability, deliberately.** The exporters outlive a connection (ADR-0036), so this is
/// owned by the runtime loop — but a destination is put in force from *inside* a transport, where
/// the offer's acknowledgement is composed (ADR-0086). Both the sampler arm of the runtime's
/// `select!` and the transport future it drives therefore hold this at once, which `&mut self`
/// cannot express. One `Mutex` gives both a shared borrow and costs nothing else: every method
/// under the lock is synchronous, so the guard is never held across an `.await` and no deadlock
/// class is introduced. An `Arc` would buy a clone nobody needs.
#[derive(Default)]
pub struct Telemetry {
    inner: std::sync::Mutex<Providers>,
}

#[derive(Default)]
struct Providers {
    meters: Option<SdkMeterProvider>,
    tracers: Option<SdkTracerProvider>,
    loggers: Option<SdkLoggerProvider>,
    /// The endpoints in force, so an offer that repeats them rebuilds nothing.
    in_force: Endpoints,
}

#[derive(Default, PartialEq, Eq, Clone)]
struct Endpoints {
    metrics: Option<String>,
    traces: Option<String>,
    logs: Option<String>,
}

impl Telemetry {
    pub fn new() -> Self {
        Telemetry::default()
    }

    /// The providers, recovering from a poisoned lock rather than propagating the panic: a thread
    /// that died mid-export must not take the Client down with it.
    fn providers(&self) -> std::sync::MutexGuard<'_, Providers> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Puts the offered destinations in force, replacing whatever was running.
    ///
    /// Returns the destinations it refused, if any, so the caller can report them rather than drop
    /// them silently — the same honesty the OpAMP settings get (ADR-0035).
    pub fn apply(
        &self,
        settings: &ConnectionSettingsOffers,
        description: &AgentDescription,
        config: &ClientConfig,
    ) -> Vec<String> {
        let wanted = Endpoints {
            metrics: endpoint_of(settings.own_metrics.as_ref()),
            traces: endpoint_of(settings.own_traces.as_ref()),
            logs: endpoint_of(settings.own_logs.as_ref()),
        };
        let mut this = self.providers();
        if wanted == this.in_force {
            return Vec::new();
        }

        let mut refused = Vec::new();
        let resource = resource(description);
        this.stop();

        if let Some(settings) = settings.own_metrics.as_ref() {
            match check(settings, "own_metrics")
                .and_then(|()| metric_provider(settings, resource.clone(), config))
            {
                Ok(provider) => this.meters = Some(provider),
                Err(e) => refused.push(e),
            }
        }
        if let Some(settings) = settings.own_traces.as_ref() {
            match check(settings, "own_traces")
                .and_then(|()| trace_provider(settings, resource.clone(), config))
            {
                Ok(provider) => {
                    opentelemetry::global::set_tracer_provider(provider.clone());
                    this.tracers = Some(provider);
                }
                Err(e) => refused.push(e),
            }
        }
        if let Some(settings) = settings.own_logs.as_ref() {
            match check(settings, "own_logs")
                .and_then(|()| log_provider(settings, resource, config))
            {
                Ok(provider) => {
                    set_bridge(Some(&provider));
                    this.loggers = Some(provider);
                }
                Err(e) => refused.push(e),
            }
        }

        this.in_force = wanted;
        if this.meters.is_some() || this.tracers.is_some() || this.loggers.is_some() {
            info!("reporting own telemetry to the destinations the Server offered");
        }
        refused
    }

    /// Samples the process behind `target`'s pid and records it against that Agent's meter. A pid
    /// that has gone away records nothing — a Managed Process between restarts is not an error.
    pub fn sample(&self, system: &mut sysinfo::System, target: &SamplingTarget) {
        let this = self.providers();
        let Some(meters) = &this.meters else {
            return;
        };
        let pid = sysinfo::Pid::from_u32(target.pid);
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        let Some(process) = system.process(pid) else {
            return;
        };

        let meter = meters.meter(SCOPE);
        // Per Agent rather than left to the Resource: the Resource is the *Client's* identity
        // (`apply` is handed the self-Agent's description), so a Managed Process's series would
        // otherwise carry the Supervisor's uid and the Supervisor's name. Both keys are restated
        // here for the same reason — one identifies the series, the other makes it readable.
        let attributes = [
            KeyValue::new(
                opentelemetry_semantic_conventions::attribute::SERVICE_INSTANCE_ID,
                target.uid.clone(),
            ),
            KeyValue::new(SERVICE_INSTANCE_NAME, target.instance_name.clone()),
        ];
        // Gauges rather than counters: what is sampled is a level, and the exporter's periodic
        // reader is what turns a series of levels into a time series.
        meter
            .u64_gauge(opentelemetry_semantic_conventions::metric::PROCESS_MEMORY_USAGE)
            .with_unit("By")
            .build()
            .record(process.memory(), &attributes);
        meter
            .f64_gauge(opentelemetry_semantic_conventions::metric::PROCESS_CPU_UTILIZATION)
            .with_unit("1")
            .build()
            .record(f64::from(process.cpu_usage()) / 100.0, &attributes);
        meter
            .u64_gauge(opentelemetry_semantic_conventions::metric::PROCESS_UPTIME)
            .with_unit("s")
            .build()
            .record(process.run_time(), &attributes);
    }

    /// The interval between samples — the caller owns the timer, this owns the number.
    pub fn sample_interval(&self) -> Duration {
        SAMPLE_INTERVAL
    }

    /// Whether any destination is in force, so a caller can skip the sampling work entirely.
    pub fn reporting(&self) -> bool {
        let this = self.providers();
        this.meters.is_some() || this.tracers.is_some() || this.loggers.is_some()
    }

    /// Stops every provider in force, flushing what it holds. Called before a new destination is
    /// installed and on shutdown; an exporter that cannot flush is logged, never fatal.
    pub fn shutdown(&self) {
        self.providers().stop();
    }
}

impl Providers {
    /// Stops and drops every provider, flushing what each holds.
    fn stop(&mut self) {
        if let Some(provider) = self.meters.take() {
            if let Err(e) = provider.shutdown() {
                warn!(error = %e, "the metrics exporter did not shut down cleanly");
            }
        }
        if let Some(provider) = self.tracers.take() {
            if let Err(e) = provider.shutdown() {
                warn!(error = %e, "the traces exporter did not shut down cleanly");
            }
        }
        if let Some(provider) = self.loggers.take() {
            // Detach the bridge before the provider goes: an event logged during shutdown must not
            // reach an exporter that is closing, which is how a flush deadlocks.
            set_bridge(None);
            if let Err(e) = provider.shutdown() {
                warn!(error = %e, "the logs exporter did not shut down cleanly");
            }
        }
    }
}

fn endpoint_of(settings: Option<&TelemetryConnectionSettings>) -> Option<String> {
    settings
        .map(|s| s.destination_endpoint.clone())
        .filter(|endpoint| !endpoint.is_empty())
}

/// The Baseline's "MAY refuse to send the telemetry if the URL begins with `http://`", taken.
///
/// The Resource carries the Agent's identifying attributes and the log records carry whatever this
/// Client logs, so plaintext across a network the operator does not control is refused rather than
/// warned about — one step firmer than the credential warning of ADR-0013, because this is a
/// continuous stream. What that leaves is the private address space (ADR-0088): loopback, and the
/// RFC 1918 and unique-local ranges, where the stream stays inside the boundary the operator
/// already owns. `tls` and `proxy` are refused for the same reasons they are on the OpAMP settings
/// (ADR-0035).
fn check(settings: &TelemetryConnectionSettings, field: &str) -> Result<(), String> {
    let endpoint = &settings.destination_endpoint;
    if endpoint.starts_with("http://") && !is_private(endpoint) {
        return Err(format!(
            "{field}: refusing to send own telemetry to {endpoint} in cleartext — a cleartext \
             destination must be loopback or a private address (10/8, 172.16/12, 192.168/16, \
             fc00::/7), otherwise use https://"
        ));
    }
    if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
        return Err(format!(
            "{field}: {endpoint} is not an OTLP/HTTP endpoint — the protocol requires a full \
             http(s) URL with path"
        ));
    }
    let mut unhonoured = Vec::new();
    if settings.tls.is_some() {
        unhonoured.push("tls");
    }
    if settings.proxy.is_some() {
        unhonoured.push("proxy");
    }
    // An offered certificate *is* honoured (ADR-0036 point 10) — but only its `cert`, paired with
    // the key this Client already holds. A `private_key` in the offer is a key the Server generated
    // for us, and ADR-0035's rule is that this Client's private key never leaves its host and is
    // never handed to it: that is the whole point of asking for a certificate through a CSR. Refused
    // by name rather than quietly ignored, so a Server issuing pairs learns why nothing happened.
    if settings
        .certificate
        .as_ref()
        .is_some_and(|certificate| !certificate.private_key.is_empty())
    {
        unhonoured.push("certificate.private_key");
    }
    if !unhonoured.is_empty() {
        return Err(format!(
            "{field}: this Client does not implement the offered {} settings",
            unhonoured.join(" and ")
        ));
    }
    Ok(())
}

/// The HTTP client the OTLP exporters send through: this Client's TLS trust, plus the client
/// certificate the offer named, if it named one.
///
/// The certificate machinery is ADR-0035's, reused as-is (ADR-0036 point 10): the offered `cert` is
/// paired with the key already on disk — the one the CSR was made for — because that key is what
/// proves the certificate belongs to this host, and it never travels.
///
/// `ca_cert` is deliberately *not* added to the trust store. The Baseline is explicit about it:
/// *"It is not recommended that the Agent accepts this CA as an authority for any purposes."* It
/// exists so a TLS-terminating intermediary can verify the client later, not so the Agent can widen
/// whom it trusts on a Server's say-so — the same reasoning that refuses `tls`.
/// An OTLP HTTP client bound to the Tokio runtime this process runs on.
///
/// **Why this wrapper exists.** The SDK's exporters do not run on the async runtime. A
/// `BatchLogProcessor` — and the metrics `PeriodicReader`, and the span batch processor — each
/// spawn a **dedicated OS thread** and drive the export with `futures_executor::block_on`. That
/// thread has no Tokio reactor. An asynchronous `reqwest::Client` handed to it panics the moment it
/// resolves a name:
///
/// ```text
/// thread 'OpenTelemetry.Logs.BatchProcessor' panicked at hyper-util .../connect/dns.rs:
/// there is no reactor running, must be called from the context of a Tokio 1.x runtime
/// ```
///
/// This is not a consequence of supplying our own client: with the `reqwest-client` feature
/// `opentelemetry-otlp` builds exactly the same asynchronous client when given none, so the fault
/// was latent from the day ADR-0036 chose that feature and surfaced only once a destination was
/// actually offered. ADR-0036's reasoning — *"this Client is a tokio process"* — is true of the
/// process and false of the thread the export happens on.
///
/// The fix keeps the asynchronous client the ADR chose and puts the work where it belongs: every
/// request is `spawn`ed onto the runtime handle captured when the exporter was built, and the
/// exporter thread merely awaits the join. Awaiting a `JoinHandle` needs no reactor of its own —
/// it is a waker-driven channel — so the blocking executor on that thread is satisfied while the
/// socket work happens on the runtime that owns the reactor.
struct RuntimeBoundClient {
    client: reqwest::Client,
    handle: tokio::runtime::Handle,
}

#[async_trait::async_trait]
impl opentelemetry_http::HttpClient for RuntimeBoundClient {
    async fn send_bytes(
        &self,
        request: opentelemetry_http::Request<opentelemetry_http::Bytes>,
    ) -> Result<
        opentelemetry_http::Response<opentelemetry_http::Bytes>,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let client = self.client.clone();
        // The request/response translation stays the library's: this delegates to its own
        // `HttpClient for reqwest::Client`, only from inside the runtime.
        self.handle
            .spawn(
                async move { opentelemetry_http::HttpClient::send_bytes(&client, request).await },
            )
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                format!("the OTLP export task did not complete: {e}").into()
            })?
    }
}

impl std::fmt::Debug for RuntimeBoundClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RuntimeBoundClient")
    }
}

fn exporter_client(
    settings: &TelemetryConnectionSettings,
    field: &str,
    config: &ClientConfig,
) -> Result<RuntimeBoundClient, String> {
    let offered = settings
        .certificate
        .as_ref()
        .map(|certificate| certificate.cert.as_slice())
        .filter(|cert| !cert.is_empty());
    let builder = crate::tls::trust_and_identity_for(
        // The timeout belongs here rather than on the exporter: `opentelemetry-otlp` keeps its own
        // for the client it would have built, and never applies it to this one.
        reqwest::Client::builder()
            .use_rustls_tls()
            .timeout(EXPORT_TIMEOUT),
        config,
        offered,
    )
    .map_err(|e| format!("{field}: {e}"))?;
    let client = builder
        .build()
        .map_err(|e| format!("{field}: cannot build the OTLP client: {e}"))?;
    // Captured here, on the runtime thread that applies the offer — the exporter thread this is
    // handed to has no runtime of its own to ask.
    let handle = tokio::runtime::Handle::try_current().map_err(|_| {
        format!("{field}: own telemetry can only be started from within the Tokio runtime")
    })?;
    Ok(RuntimeBoundClient { client, handle })
}

/// Whether a cleartext destination stays inside the private address space (ADR-0088).
///
/// Literal addresses only, plus `localhost` by name. A host name is **not** resolved to decide
/// this: the answer would depend on what DNS says at the moment the offer is admitted, and an
/// admission test that a re-resolve can flip is not one an operator can reason about. A collector
/// reached by name over cleartext is therefore refused — name it by address, or put TLS in front
/// of it.
fn is_private(endpoint: &str) -> bool {
    let host = host_of(endpoint);
    if host == "localhost" {
        return true;
    }
    match host.parse::<IpAddr>() {
        // `is_private` is the RFC 1918 trio — 10/8, 172.16/12, 192.168/16 — and nothing else.
        Ok(IpAddr::V4(v4)) => v4.is_loopback() || v4.is_private(),
        // `Ipv6Addr::is_unique_local` is still unstable, so fc00::/7 is spelled out here.
        Ok(IpAddr::V6(v6)) => v6.is_loopback() || is_unique_local(v6),
        Err(_) => false,
    }
}

/// The host of an endpoint, without scheme, port, or path: `192.168.10.5:4318/v1/logs` →
/// `192.168.10.5`, `[fd00::5]:4318/v1/logs` → `fd00::5`. An IPv6 literal is bracketed in a URL, so
/// its own colons are only separable once the brackets are found.
fn host_of(endpoint: &str) -> &str {
    let authority = endpoint
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("");
    match authority.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    }
}

/// `fc00::/7`, IPv6's answer to RFC 1918.
fn is_unique_local(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xfe00) == 0xfc00
}

/// The OTLP Resource: the Agent's identifying attributes, as the Baseline asks — "the combination
/// of identifying attributes SHOULD be sufficient to uniquely identify the Agent's own telemetry"
/// — plus the one name that makes the result readable.
fn resource(description: &AgentDescription) -> Resource {
    let attributes = description.identifying_attributes.iter().filter_map(|kv| {
        let value = match kv.value.as_ref()?.value.as_ref()? {
            opamp::proto::any_value::Value::StringValue(s) => s.clone(),
            _ => return None,
        };
        Some(KeyValue::new(kv.key.clone(), value))
    });
    // `service.instance.name` is non-identifying only because the Baseline has no key for a human
    // instance name and identity stays `service.instance.id` (ADR-0033) — that is a statement about
    // *identity*, not about what the telemetry is worth carrying. Without it every series at the
    // receiving end is a uuid the operator cannot place against the fleet view they searched by.
    let name = string_value(
        &description.non_identifying_attributes,
        SERVICE_INSTANCE_NAME,
    )
    .map(|name| KeyValue::new(SERVICE_INSTANCE_NAME, name.to_string()));
    Resource::builder_empty()
        .with_attributes(attributes.chain(name))
        .build()
}

fn headers(settings: &TelemetryConnectionSettings) -> HashMap<String, String> {
    settings
        .headers
        .as_ref()
        .map(|headers| {
            headers
                .headers
                .iter()
                .map(|header| (header.key.clone(), header.value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn metric_provider(
    settings: &TelemetryConnectionSettings,
    resource: Resource,
    config: &ClientConfig,
) -> Result<SdkMeterProvider, String> {
    let exporter = MetricExporter::builder()
        .with_http()
        .with_endpoint(&settings.destination_endpoint)
        .with_headers(headers(settings))
        .with_http_client(exporter_client(settings, "own_metrics", config)?)
        .build()
        .map_err(|e| format!("own_metrics: cannot build the exporter: {e}"))?;
    // The reader is built explicitly rather than through `with_periodic_exporter`, which would take
    // the SDK's 60 s default and quietly miss the Baseline's recommended reporting interval.
    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter)
        .with_interval(SAMPLE_INTERVAL)
        .build();
    Ok(SdkMeterProvider::builder()
        .with_resource(resource)
        .with_reader(reader)
        .build())
}

fn trace_provider(
    settings: &TelemetryConnectionSettings,
    resource: Resource,
    config: &ClientConfig,
) -> Result<SdkTracerProvider, String> {
    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(&settings.destination_endpoint)
        .with_headers(headers(settings))
        .with_http_client(exporter_client(settings, "own_traces", config)?)
        .build()
        .map_err(|e| format!("own_traces: cannot build the exporter: {e}"))?;
    Ok(SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build())
}

fn log_provider(
    settings: &TelemetryConnectionSettings,
    resource: Resource,
    config: &ClientConfig,
) -> Result<SdkLoggerProvider, String> {
    let exporter = LogExporter::builder()
        .with_http()
        .with_endpoint(&settings.destination_endpoint)
        .with_headers(headers(settings))
        .with_http_client(exporter_client(settings, "own_logs", config)?)
        .build()
        .map_err(|e| format!("own_logs: cannot build the exporter: {e}"))?;
    Ok(SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::{
        any_value, AnyValue, KeyValue as ProtoKeyValue, TlsCertificate, TlsConnectionSettings,
    };

    fn description() -> AgentDescription {
        AgentDescription {
            identifying_attributes: vec![ProtoKeyValue {
                key: "service.name".to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue(
                        "opamp-fleet-client".to_string(),
                    )),
                }),
            }],
            non_identifying_attributes: vec![ProtoKeyValue {
                key: "service.instance.name".to_string(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::StringValue("edge-01".to_string())),
                }),
            }],
        }
    }

    fn destination(endpoint: &str) -> TelemetryConnectionSettings {
        TelemetryConnectionSettings {
            destination_endpoint: endpoint.to_string(),
            ..Default::default()
        }
    }

    /// With nothing offered nothing is built — the capability says the Client *can* report, and the
    /// Server's offer is what arms it.
    #[test]
    fn no_destination_builds_nothing() {
        let telemetry = Telemetry::new();
        let refused = telemetry.apply(
            &ConnectionSettingsOffers::default(),
            &description(),
            &ClientConfig::default(),
        );
        assert!(refused.is_empty());
        assert!(!telemetry.reporting());
    }

    /// The Baseline's "MAY refuse" for cleartext, taken — and refused *loudly*, so the Server is
    /// told rather than left believing the telemetry flows.
    #[test]
    fn a_cleartext_destination_beyond_the_private_network_is_refused() {
        let telemetry = Telemetry::new();
        let offer = ConnectionSettingsOffers {
            own_metrics: Some(destination("http://collector.example:4318/v1/metrics")),
            ..Default::default()
        };
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("cleartext"), "{}", refused[0]);
        assert!(!telemetry.reporting());
    }

    /// The private address space is where cleartext is admitted and where it stops (ADR-0088).
    /// The last two cases are the ones a prefix test would wave through: a public address that
    /// merely reads like a private one, and a host *name* whose first labels are a private
    /// address.
    #[test]
    fn cleartext_is_admitted_by_address_and_nowhere_else() {
        for allowed in [
            "http://localhost:4318/v1/metrics",
            "http://127.0.0.1:4318/v1/metrics",
            "http://[::1]:4318/v1/metrics",
            "http://192.168.10.5:4318/v1/metrics",
            "http://10.0.0.5:4318/v1/metrics",
            "http://172.16.0.5:4318/v1/metrics",
            "http://[fd00::5]:4318/v1/metrics",
        ] {
            assert!(
                check(&destination(allowed), "own_metrics").is_ok(),
                "{allowed} is inside the private address space"
            );
        }
        for refused in [
            "http://collector.example:4318/v1/metrics",
            "http://203.0.113.5:4318/v1/metrics",
            "http://172.32.0.5:4318/v1/metrics",
            "http://[2001:db8::5]:4318/v1/metrics",
            "http://192.168.0.1.example.com:4318/v1/metrics",
        ] {
            let Err(error) = check(&destination(refused), "own_metrics") else {
                panic!("{refused} is not a private address");
            };
            assert!(error.contains("cleartext"), "{error}");
        }
    }

    /// A Collector on the LAN rather than on the host: the same shape as loopback, one hop out,
    /// and still inside the boundary the operator owns (ADR-0088).
    #[tokio::test]
    async fn a_private_network_destination_is_allowed_in_cleartext() {
        crate::tls::install_ring_provider();
        let telemetry = Telemetry::new();
        let offer = ConnectionSettingsOffers {
            own_metrics: Some(destination("http://192.168.10.5:4318/v1/metrics")),
            ..Default::default()
        };
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        assert!(refused.is_empty(), "{refused:?}");
        assert!(telemetry.reporting());
        telemetry.shutdown();
    }

    /// Loopback is the innermost case: a Collector on the same host over plain HTTP is the ordinary
    /// development and sidecar shape, and nothing leaves the machine at all.
    #[tokio::test]
    async fn a_loopback_destination_is_allowed_in_cleartext() {
        crate::tls::install_ring_provider();
        let telemetry = Telemetry::new();
        let offer = ConnectionSettingsOffers {
            own_metrics: Some(destination("http://127.0.0.1:4318/v1/metrics")),
            ..Default::default()
        };
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        assert!(refused.is_empty(), "{refused:?}");
        assert!(telemetry.reporting());
        telemetry.shutdown();
    }

    /// The same two fields refused on the OpAMP settings are refused here, and for the same
    /// reasons — named, not dropped in silence (ADR-0035, ADR-0036).
    #[test]
    fn offered_tls_settings_are_refused_by_name() {
        let telemetry = Telemetry::new();
        let offer = ConnectionSettingsOffers {
            own_logs: Some(TelemetryConnectionSettings {
                destination_endpoint: "https://collector.example:4318/v1/logs".to_string(),
                tls: Some(TlsConnectionSettings {
                    insecure_skip_verify: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("tls"), "{}", refused[0]);
        assert!(!telemetry.reporting());
    }

    /// ADR-0036 point 10: the offered `certificate` is *honoured*, not refused — the ADR-0035
    /// machinery is reused as-is, which means the offered `cert` is paired with the key this Client
    /// already generated for its CSR. With that key present, an offer naming a certificate builds
    /// an exporter that presents it.
    #[tokio::test]
    async fn an_offered_certificate_is_presented_by_the_exporter() {
        crate::tls::install_ring_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        // What the CSR flow leaves behind: the key the request was made for, and the certificate
        // the Server signed for it. Self-signed here — nothing verifies the chain in this test, the
        // point is that key and certificate pair into a usable identity.
        let key = rcgen::KeyPair::generate().expect("key");
        let params =
            rcgen::CertificateParams::new(vec!["agent.example".to_string()]).expect("params");
        let cert = params.self_signed(&key).expect("cert");
        std::fs::write(
            dir.path().join(crate::tls::ISSUED_KEY_FILE),
            key.serialize_pem(),
        )
        .expect("write key");

        let config = ClientConfig {
            state_dir: dir.path().to_path_buf(),
            ..ClientConfig::default()
        };
        let offer = ConnectionSettingsOffers {
            own_metrics: Some(TelemetryConnectionSettings {
                destination_endpoint: "http://127.0.0.1:4318/v1/metrics".to_string(),
                certificate: Some(TlsCertificate {
                    cert: cert.pem().into_bytes(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let telemetry = Telemetry::new();
        let refused = telemetry.apply(&offer, &description(), &config);
        assert!(refused.is_empty(), "{refused:?}");
        assert!(telemetry.reporting());
        telemetry.shutdown();
    }

    /// And an offered certificate with no key to go with it is refused *by name* rather than
    /// dropped: without the CSR key there is nothing to prove possession with, so an exporter that
    /// silently connected without the certificate would be reporting success it did not have.
    #[test]
    fn an_offered_certificate_without_its_key_is_refused_and_named() {
        crate::tls::install_ring_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = ClientConfig {
            state_dir: dir.path().to_path_buf(),
            ..ClientConfig::default()
        };
        let offer = ConnectionSettingsOffers {
            own_metrics: Some(TelemetryConnectionSettings {
                destination_endpoint: "http://127.0.0.1:4318/v1/metrics".to_string(),
                certificate: Some(TlsCertificate {
                    cert: b"-----BEGIN CERTIFICATE-----".to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let telemetry = Telemetry::new();
        let refused = telemetry.apply(&offer, &description(), &config);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("own_metrics"), "{}", refused[0]);
        assert!(refused[0].contains("no key"), "{}", refused[0]);
        assert!(!telemetry.reporting());
    }

    /// But a private key *in the offer* is refused by name. ADR-0035's rule is that this Client's
    /// private key never leaves its host and is never handed to it — which is the whole reason the
    /// certificate is obtained through a CSR.
    #[test]
    fn an_offered_private_key_is_refused_by_name() {
        let telemetry = Telemetry::new();
        let offer = ConnectionSettingsOffers {
            own_traces: Some(TelemetryConnectionSettings {
                destination_endpoint: "https://collector.example:4318/v1/traces".to_string(),
                certificate: Some(TlsCertificate {
                    cert: b"-----BEGIN CERTIFICATE-----".to_vec(),
                    private_key: b"-----BEGIN PRIVATE KEY-----".to_vec(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(
            refused[0].contains("certificate.private_key"),
            "{}",
            refused[0]
        );
        assert!(!telemetry.reporting());
    }

    /// The Baseline names a number for own metrics — *"The recommended reporting interval is 10
    /// seconds"* — and this pins it, because the way it drifts is silent: the SDK's periodic reader
    /// defaults to 60 s, so an exporter built without an explicit interval reports six times more
    /// slowly than recommended and nothing says so. Sampling and export share the constant, so this
    /// guards both.
    #[test]
    fn metrics_are_reported_at_the_interval_the_baseline_recommends() {
        assert_eq!(Telemetry::new().sample_interval(), Duration::from_secs(10));
    }

    /// The Resource carries what identifies the Agent, which is what makes one host's several
    /// Agents distinguishable at the receiving end — and the operator's name for it beside them,
    /// which is what makes the result placeable against the fleet view.
    #[test]
    fn the_resource_carries_the_agents_identifying_attributes() {
        let resource = resource(&description());
        let attribute = |key: &str| {
            resource
                .iter()
                .find(|(k, _)| k.as_str() == key)
                .map(|(_, value)| value.to_string())
        };
        assert_eq!(
            attribute("service.name").as_deref(),
            Some("opamp-fleet-client")
        );
        assert_eq!(
            attribute("service.instance.name").as_deref(),
            Some("edge-01")
        );
    }

    /// The instance name is non-identifying, so it is reported where an Agent has one and left out
    /// where it does not — an absent attribute says "unknown" where an empty one says nothing true.
    #[test]
    fn an_agent_without_an_instance_name_reports_none() {
        let mut description = description();
        description.non_identifying_attributes.clear();
        assert!(resource(&description)
            .iter()
            .all(|(key, _)| key.as_str() != "service.instance.name"));
    }

    /// The bound nothing else on the path supplies. A destination that *refuses* recovers by
    /// itself — the next interval is a fresh request — so what is worth a test is the one that
    /// does not: a socket accepted and then left silent, which without this holds the exporter
    /// thread for good and takes own telemetry down until the process restarts.
    #[tokio::test]
    async fn an_export_to_a_destination_that_never_answers_gives_up() {
        crate::tls::install_ring_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/metrics", listener.local_addr().unwrap());
        let _silent = tokio::spawn(async move {
            // Accepted and held: closing the socket would be an answer, and an answer is the case
            // that was never broken.
            let mut connections = Vec::new();
            while let Ok((connection, _)) = listener.accept().await {
                connections.push(connection);
            }
        });

        let client = exporter_client(
            &destination(&endpoint),
            "own_metrics",
            &ClientConfig::default(),
        )
        .expect("the client builds");
        // Bounded from the outside as well: without the client's own timeout this send never
        // returns, and a regression should fail the test rather than hang it.
        let outcome = tokio::time::timeout(SAMPLE_INTERVAL, client.client.post(&endpoint).send())
            .await
            .expect("the export gives up within the interval that drives it");

        assert!(
            outcome
                .as_ref()
                .err()
                .is_some_and(reqwest::Error::is_timeout),
            "a silent destination must time out, got {outcome:?}"
        );
    }
}
