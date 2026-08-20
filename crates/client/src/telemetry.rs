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
use std::time::Duration;

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

/// How often process metrics are sampled and handed to the periodic exporter.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(30);

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

/// The providers currently in force, if any.
///
/// Held rather than left to the SDK's globals because the destination is not a startup decision: it
/// arrives from the Server and can change. Applying a new offer means building fresh providers and
/// shutting these down, which needs a handle on exactly what is running.
#[derive(Default)]
pub struct Telemetry {
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

    /// Puts the offered destinations in force, replacing whatever was running.
    ///
    /// Returns the destinations it refused, if any, so the caller can report them rather than drop
    /// them silently — the same honesty the OpAMP settings get (ADR-0035).
    pub fn apply(
        &mut self,
        settings: &ConnectionSettingsOffers,
        description: &AgentDescription,
        config: &ClientConfig,
    ) -> Vec<String> {
        let wanted = Endpoints {
            metrics: endpoint_of(settings.own_metrics.as_ref()),
            traces: endpoint_of(settings.own_traces.as_ref()),
            logs: endpoint_of(settings.own_logs.as_ref()),
        };
        if wanted == self.in_force {
            return Vec::new();
        }

        let mut refused = Vec::new();
        let resource = resource(description);
        self.shutdown();

        if let Some(settings) = settings.own_metrics.as_ref() {
            match check(settings, "own_metrics")
                .and_then(|()| metric_provider(settings, resource.clone(), config))
            {
                Ok(provider) => self.meters = Some(provider),
                Err(e) => refused.push(e),
            }
        }
        if let Some(settings) = settings.own_traces.as_ref() {
            match check(settings, "own_traces")
                .and_then(|()| trace_provider(settings, resource.clone(), config))
            {
                Ok(provider) => {
                    opentelemetry::global::set_tracer_provider(provider.clone());
                    self.tracers = Some(provider);
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
                    self.loggers = Some(provider);
                }
                Err(e) => refused.push(e),
            }
        }

        self.in_force = wanted;
        if self.meters.is_some() || self.tracers.is_some() || self.loggers.is_some() {
            info!("reporting own telemetry to the destinations the Server offered");
        }
        refused
    }

    /// Samples the process behind `pid` and records it against this Agent's meter. A pid that has
    /// gone away records nothing — a Managed Process between restarts is not an error.
    pub fn sample(&self, system: &mut sysinfo::System, pid: u32, agent: &str) {
        let Some(meters) = &self.meters else {
            return;
        };
        let pid = sysinfo::Pid::from_u32(pid);
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        let Some(process) = system.process(pid) else {
            return;
        };

        let meter = meters.meter(SCOPE);
        let attributes = [KeyValue::new(
            opentelemetry_semantic_conventions::attribute::SERVICE_INSTANCE_ID,
            agent.to_string(),
        )];
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
        self.meters.is_some() || self.tracers.is_some() || self.loggers.is_some()
    }

    /// The logger provider, for the `tracing` bridge to attach to.
    pub fn loggers(&self) -> Option<&SdkLoggerProvider> {
        self.loggers.as_ref()
    }

    /// Stops every provider in force, flushing what it holds. Called before a new destination is
    /// installed and on shutdown; an exporter that cannot flush is logged, never fatal.
    pub fn shutdown(&mut self) {
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
/// Client logs, so plaintext beyond the loopback interface is refused rather than warned about —
/// one step firmer than the credential warning of ADR-0013, because this is a continuous stream.
/// `tls` and `proxy` are refused for the same reasons they are on the OpAMP settings (ADR-0035).
fn check(settings: &TelemetryConnectionSettings, field: &str) -> Result<(), String> {
    let endpoint = &settings.destination_endpoint;
    if endpoint.starts_with("http://") && !is_loopback(endpoint) {
        return Err(format!(
            "{field}: refusing to send own telemetry to {endpoint} in cleartext — use https://"
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
fn exporter_client(
    settings: &TelemetryConnectionSettings,
    field: &str,
    config: &ClientConfig,
) -> Result<reqwest::Client, String> {
    let offered = settings
        .certificate
        .as_ref()
        .map(|certificate| certificate.cert.as_slice())
        .filter(|cert| !cert.is_empty());
    let builder = crate::tls::trust_and_identity_for(
        reqwest::Client::builder().use_rustls_tls(),
        config,
        offered,
    )
    .map_err(|e| format!("{field}: {e}"))?;
    builder
        .build()
        .map_err(|e| format!("{field}: cannot build the OTLP client: {e}"))
}

fn is_loopback(endpoint: &str) -> bool {
    let host = endpoint
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
}

/// The OTLP Resource: the Agent's identifying attributes, as the Baseline asks — "the combination
/// of identifying attributes SHOULD be sufficient to uniquely identify the Agent's own telemetry".
fn resource(description: &AgentDescription) -> Resource {
    let attributes = description.identifying_attributes.iter().filter_map(|kv| {
        let value = match kv.value.as_ref()?.value.as_ref()? {
            opamp::proto::any_value::Value::StringValue(s) => s.clone(),
            _ => return None,
        };
        Some(KeyValue::new(kv.key.clone(), value))
    });
    Resource::builder_empty()
        .with_attributes(attributes)
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
    Ok(SdkMeterProvider::builder()
        .with_resource(resource)
        .with_periodic_exporter(exporter)
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
            non_identifying_attributes: vec![],
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
        let mut telemetry = Telemetry::new();
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
    fn a_cleartext_destination_beyond_loopback_is_refused() {
        let mut telemetry = Telemetry::new();
        let offer = ConnectionSettingsOffers {
            own_metrics: Some(destination("http://collector.example:4318/v1/metrics")),
            ..Default::default()
        };
        let refused = telemetry.apply(&offer, &description(), &ClientConfig::default());
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("cleartext"), "{}", refused[0]);
        assert!(!telemetry.reporting());
    }

    /// Loopback is the exception: a Collector on the same host over plain HTTP is the ordinary
    /// development and sidecar shape, and nothing leaves the machine.
    #[test]
    fn a_loopback_destination_is_allowed_in_cleartext() {
        crate::tls::install_ring_provider();
        let mut telemetry = Telemetry::new();
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
        let mut telemetry = Telemetry::new();
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
    #[test]
    fn an_offered_certificate_is_presented_by_the_exporter() {
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

        let mut telemetry = Telemetry::new();
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

        let mut telemetry = Telemetry::new();
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
        let mut telemetry = Telemetry::new();
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

    /// The Resource carries what identifies the Agent, which is what makes one host's several
    /// Agents distinguishable at the receiving end.
    #[test]
    fn the_resource_carries_the_agents_identifying_attributes() {
        let resource = resource(&description());
        let name = resource
            .iter()
            .find(|(key, _)| key.as_str() == "service.name")
            .map(|(_, value)| value.to_string());
        assert_eq!(name.as_deref(), Some("opamp-fleet-client"));
    }
}
