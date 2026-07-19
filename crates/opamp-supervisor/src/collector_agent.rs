//! The [`ManagedAgent`] adapter for an OpAMP-native OpenTelemetry Collector — the **Collector
//! Supervisor** (ADR-0008, ADR-0009).
//!
//! It owns an `otelcol` process ([`Collector`]), injects the `opamp`/`health_check` extensions into the
//! config so the collector reports back over a **local OpAMP server** ([`crate::local_server`]), and
//! surfaces the collector's *actual* health and effective config through the port. Behaviour matches the
//! upstream Go Supervisor oracle; only its seams sit behind [`ManagedAgent`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tracing::{info, warn};

use crate::agent::{
    liveness_health, AgentStatus, ChangeSignal, ManagedAgent, OwnTelemetry, TelemetryDestination,
};
use crate::collector::Collector;
use crate::local_server::CollectorLink;

/// How long the bootstrap step waits for the collector to report its identity before giving up and
/// falling back to a synthesized description. Bounded so a collector that never connects cannot hang
/// supervisor startup.
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(5);

/// A Collector under management: the process, the link to its local OpAMP reports, and the local
/// endpoint injected into its config.
pub struct CollectorAgent {
    collector: Collector,
    link: CollectorLink,
    local_opamp_endpoint: String,
    /// The supervisor's Instance UID as a canonical UUID string, injected into the collector's `opamp`
    /// extension so the collector reports under the same identity — matching the Go supervisor. `None`
    /// when the UID is not a valid UUIDv7 (e.g. an unusual Server-assigned UID), in which case the
    /// extension is left to generate its own and the supervisor keeps overriding `service.instance.id`.
    instance_uid: Option<String>,
    /// A base collector config merged underneath every remote config (remote keys win), so an operator
    /// can pin local settings the Server's config is layered on top of. `None` when not configured.
    base_config: Option<Vec<u8>>,
    /// The destinations the Server offered for the collector's own telemetry, injected into the
    /// collector's `service.telemetry` when the config is prepared (ADR-0010).
    own_telemetry: OwnTelemetry,
    start_time_unix_nano: u64,
}

impl CollectorAgent {
    pub fn new(
        collector: Collector,
        link: CollectorLink,
        local_opamp_endpoint: String,
        instance_uid: &[u8],
        base_config: Option<Vec<u8>>,
    ) -> Self {
        let start_time_unix_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            collector,
            link,
            local_opamp_endpoint,
            instance_uid: uuidv7_string(instance_uid),
            base_config,
            own_telemetry: OwnTelemetry::default(),
            start_time_unix_nano,
        }
    }
}

impl ManagedAgent for CollectorAgent {
    /// Inject the `opamp`/`health_check` extensions so the collector connects back to the local server.
    /// Re-injection is idempotent apart from the endpoint, so re-preparing an already-merged config on
    /// resume just re-points it at this run's local server.
    fn prepare_config(&self, config: Vec<u8>) -> Vec<u8> {
        let with_opamp = merge_opamp_extension(
            &config,
            &self.local_opamp_endpoint,
            self.instance_uid.as_deref(),
        );
        merge_own_telemetry(&with_opamp, &self.own_telemetry)
    }

    /// Deep-merge the offered config files (already in sorted-key order) into one collector config, so a
    /// multi-file remote config reaches the same effective config as the Go supervisor. A configured base
    /// config is placed underneath the remote files (remote keys win). Without a base config a single file
    /// is passed through unchanged; a file that is not a YAML mapping is skipped in the merge.
    fn merge_config(&self, files: &[(String, Vec<u8>)]) -> Option<Vec<u8>> {
        if files.is_empty() {
            return None;
        }
        match &self.base_config {
            Some(base) => {
                // Base first, remote files on top: later entries win, so the remote config overrides the
                // base, matching the Go supervisor's config layering.
                let mut layered = Vec::with_capacity(files.len() + 1);
                layered.push((String::new(), base.clone()));
                layered.extend(files.iter().cloned());
                Some(deep_merge_yaml(&layered))
            }
            None => match files {
                [(_, body)] => Some(body.clone()),
                _ => Some(deep_merge_yaml(files)),
            },
        }
    }

    /// Start the collector on a minimal config whose only job is to run the `opamp` extension pointed at
    /// the local server, so the collector connects back and reports its real `AgentDescription` and
    /// available components; capture those, then stop it. The domain (re)starts the collector on its real
    /// (resumed / fallback / remote) config next. If the collector cannot start or never reports in time,
    /// the supervisor keeps the synthesized description — no worse than before bootstrap existed.
    async fn bootstrap(&mut self) {
        let config = merge_opamp_extension(
            b"{}\n",
            &self.local_opamp_endpoint,
            self.instance_uid.as_deref(),
        );
        if let Err(e) = self.collector.apply(&config).await {
            warn!(error = %e, "collector bootstrap could not start; identity will be reported once the collector connects on its real config");
            return;
        }

        let notify = self.link.change_notify();
        let deadline = tokio::time::Instant::now() + BOOTSTRAP_TIMEOUT;
        while self.link.latest().agent_description.is_none() {
            tokio::select! {
                _ = notify.notified() => {}
                _ = tokio::time::sleep_until(deadline) => {
                    warn!("collector bootstrap timed out before the collector reported its identity; using a synthesized description");
                    break;
                }
            }
        }
        if self.link.latest().agent_description.is_some() {
            info!("collector bootstrap captured the agent-reported identity");
        }
        self.collector.stop().await;
    }

    async fn apply(&mut self, config: &[u8]) -> Result<(), String> {
        // Forget the previous process's health before restarting, so the domain confirms the new
        // config's health from a fresh report rather than the outgoing collector's (ADR-0008 rollback).
        self.link.clear_health();
        self.collector.apply(config).await
    }

    async fn restart(&mut self) -> Result<(), String> {
        self.collector.restart_current().await
    }

    fn status(&self) -> AgentStatus {
        let latest = self.link.latest();
        // Prefer the collector's self-reported health over process liveness (ADR-0008).
        let health = latest.health.unwrap_or_else(|| {
            liveness_health(
                self.collector.is_running(),
                String::new(),
                self.start_time_unix_nano,
            )
        });
        AgentStatus {
            health,
            effective_config: latest.effective_config,
            agent_description: latest.agent_description,
            available_components: latest.available_components,
        }
    }

    fn change_signal(&self) -> ChangeSignal {
        ChangeSignal::new(self.link.change_notify())
    }

    fn reported_health(&self) -> Option<opamp_proto::proto::ComponentHealth> {
        self.link.latest().health
    }

    async fn supervise(&mut self) -> Option<String> {
        let status = self.collector.check_exited()?;
        let mut reason = status.to_string();
        // Enrich the crash report with the collector's own recent stderr, when capture is enabled
        // (`collector_crash_log_snippet_kib`), so the fleet sees *why* it died, not just that it did.
        if let Some(log) = self.collector.crash_log_tail() {
            reason.push_str("; recent collector log:\n");
            reason.push_str(&log);
        }
        Some(reason)
    }

    async fn shutdown(&mut self) {
        self.collector.stop().await;
    }

    fn set_own_telemetry(&mut self, settings: OwnTelemetry) -> bool {
        if settings == self.own_telemetry {
            return false;
        }
        self.own_telemetry = settings;
        true
    }
}

/// Injects the collector's `opamp` and `health_check` extensions into a collector configuration and
/// lists them under `service.extensions`. The `opamp` extension, pointed at the local OpAMP server, is
/// what makes the collector connect back and report its real health and effective config; `health_check`
/// exposes the collector's health for it to observe. Existing extensions and the rest of the config are
/// preserved. If the config is not a YAML mapping it is returned unchanged — validation will reject a
/// genuinely broken config, and we do not want to mask that. When `instance_uid` is given it is set on
/// the `opamp` extension so the collector reports under the supervisor's identity.
fn merge_opamp_extension(config: &[u8], endpoint: &str, instance_uid: Option<&str>) -> Vec<u8> {
    let mut doc: serde_yaml::Value = match serde_yaml::from_slice(config) {
        Ok(doc) => doc,
        Err(_) => return config.to_vec(),
    };
    let Some(root) = doc.as_mapping_mut() else {
        return config.to_vec();
    };

    let mut opamp = match serde_yaml::from_str::<serde_yaml::Value>(&format!(
        "server:\n  ws:\n    endpoint: \"{endpoint}\"\n    tls:\n      insecure: true\n"
    )) {
        Ok(value) => value,
        Err(_) => return config.to_vec(),
    };
    if let (Some(uid), Some(map)) = (instance_uid, opamp.as_mapping_mut()) {
        map.insert("instance_uid".into(), uid.into());
    }
    let extensions = root
        .entry("extensions".into())
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let Some(extensions) = extensions.as_mapping_mut() else {
        return config.to_vec();
    };
    extensions.insert("opamp".into(), opamp);
    extensions
        .entry("health_check".into())
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let service = root
        .entry("service".into())
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    if let Some(service) = service.as_mapping_mut() {
        let enabled = service
            .entry("extensions".into())
            .or_insert_with(|| serde_yaml::Value::Sequence(Vec::new()));
        if let Some(list) = enabled.as_sequence_mut() {
            for name in ["opamp", "health_check"] {
                if !list.iter().any(|v| v.as_str() == Some(name)) {
                    list.push(name.into());
                }
            }
        }
    }

    serde_yaml::to_string(&doc)
        .map(String::into_bytes)
        .unwrap_or_else(|_| config.to_vec())
}

/// Injects the collector's own-telemetry destinations into `service.telemetry` (ADR-0010): the offered
/// OTLP/HTTP endpoint (and headers) become a metrics reader / logs processor / traces processor, so the
/// collector ships its own telemetry to where the Server asked. Own-telemetry settings win over whatever
/// the config carried for that signal. A config that is not a YAML mapping, or an empty offer, is
/// returned unchanged.
fn merge_own_telemetry(config: &[u8], own: &OwnTelemetry) -> Vec<u8> {
    if own.is_empty() {
        return config.to_vec();
    }
    let mut doc: serde_yaml::Value = match serde_yaml::from_slice(config) {
        Ok(doc) => doc,
        Err(_) => return config.to_vec(),
    };
    let Some(root) = doc.as_mapping_mut() else {
        return config.to_vec();
    };
    let service = root
        .entry("service".into())
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let Some(service) = service.as_mapping_mut() else {
        return config.to_vec();
    };
    let telemetry = service
        .entry("telemetry".into())
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let Some(telemetry) = telemetry.as_mapping_mut() else {
        return config.to_vec();
    };

    // The collector's `service.telemetry` uses the OpenTelemetry configuration schema: metrics take a
    // list of `readers` (each a `periodic` reader), logs and traces a list of `processors` (each a
    // `batch` processor). Each wraps an OTLP/HTTP exporter pointed at the offered destination.
    if let Some(dest) = &own.metrics {
        set_signal_exporter(telemetry, "metrics", "readers", "periodic", dest);
    }
    if let Some(dest) = &own.logs {
        set_signal_exporter(telemetry, "logs", "processors", "batch", dest);
    }
    if let Some(dest) = &own.traces {
        set_signal_exporter(telemetry, "traces", "processors", "batch", dest);
    }

    serde_yaml::to_string(&doc)
        .map(String::into_bytes)
        .unwrap_or_else(|_| config.to_vec())
}

/// Sets one telemetry signal's exporter list under `service.telemetry`: `telemetry.<signal>.<list_key>`
/// becomes a single `<wrapper>` entry carrying an OTLP/HTTP exporter for `dest`, replacing any existing
/// list for that signal (own telemetry wins) while leaving the signal's other keys in place.
fn set_signal_exporter(
    telemetry: &mut serde_yaml::Mapping,
    signal: &str,
    list_key: &str,
    wrapper: &str,
    dest: &TelemetryDestination,
) {
    let sig = telemetry
        .entry(signal.into())
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));
    let Some(sig) = sig.as_mapping_mut() else {
        return;
    };
    let mut exporter = serde_yaml::Mapping::new();
    exporter.insert("exporter".into(), single("otlp", otlp_exporter(dest)));
    sig.insert(
        list_key.into(),
        serde_yaml::Value::Sequence(vec![single(wrapper, serde_yaml::Value::Mapping(exporter))]),
    );
}

/// The OTLP/HTTP exporter mapping for a destination: protocol, endpoint, and any offered headers (as the
/// OpenTelemetry configuration schema's list of `{name, value}`).
fn otlp_exporter(dest: &TelemetryDestination) -> serde_yaml::Value {
    let mut otlp = serde_yaml::Mapping::new();
    otlp.insert("protocol".into(), "http/protobuf".into());
    otlp.insert("endpoint".into(), dest.endpoint.as_str().into());
    if !dest.headers.is_empty() {
        let headers = dest
            .headers
            .iter()
            .map(|(name, value)| {
                let mut h = serde_yaml::Mapping::new();
                h.insert("name".into(), name.as_str().into());
                h.insert("value".into(), value.as_str().into());
                serde_yaml::Value::Mapping(h)
            })
            .collect();
        otlp.insert("headers".into(), serde_yaml::Value::Sequence(headers));
    }
    serde_yaml::Value::Mapping(otlp)
}

/// A single-entry YAML mapping `{key: value}`.
fn single(key: &str, value: serde_yaml::Value) -> serde_yaml::Value {
    let mut map = serde_yaml::Mapping::new();
    map.insert(key.into(), value);
    serde_yaml::Value::Mapping(map)
}

/// The canonical UUID string for a 16-byte Instance UID, but only when it is a UUIDv7 — the identifier
/// form the collector's `opamp` extension accepts. Returns `None` otherwise, so a non-v7 UID is never
/// injected (the extension would reject it and fail to start), leaving it to generate its own.
fn uuidv7_string(uid: &[u8]) -> Option<String> {
    let bytes: &[u8; 16] = uid.try_into().ok()?;
    let version = bytes[6] >> 4;
    (version == 7).then(|| crate::uid::format(bytes))
}

/// Deep-merges the YAML mapping documents in `files` (in the given order — later files override earlier
/// ones), matching how the Go supervisor merges a multi-file remote config. Files that are not YAML
/// mappings are skipped; if nothing merges, the first file is returned unchanged.
fn deep_merge_yaml(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut acc = serde_yaml::Value::Mapping(Default::default());
    let mut merged_any = false;
    for (_, body) in files {
        match serde_yaml::from_slice::<serde_yaml::Value>(body) {
            Ok(value @ serde_yaml::Value::Mapping(_)) => {
                merge_value(&mut acc, value);
                merged_any = true;
            }
            _ => continue,
        }
    }
    if !merged_any {
        return files[0].1.clone();
    }
    serde_yaml::to_string(&acc)
        .map(String::into_bytes)
        .unwrap_or_else(|_| files[0].1.clone())
}

/// Recursively merges `overlay` into `base`: mappings are merged key by key; any other value (scalar or
/// sequence) replaces what is in `base`.
fn merge_value(base: &mut serde_yaml::Value, overlay: serde_yaml::Value) {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(base), serde_yaml::Value::Mapping(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_injects_the_opamp_extension_and_enables_it() {
        let config = b"exporters:\n  debug: {}\nservice:\n  pipelines:\n    logs:\n      exporters: [debug]\n";
        let merged = merge_opamp_extension(config, "ws://127.0.0.1:9999/v1/opamp", None);
        let doc: serde_yaml::Value = serde_yaml::from_slice(&merged).unwrap();

        let endpoint = doc["extensions"]["opamp"]["server"]["ws"]["endpoint"]
            .as_str()
            .unwrap();
        assert_eq!(endpoint, "ws://127.0.0.1:9999/v1/opamp");
        assert!(!doc["extensions"]["health_check"].is_null());
        // No instance_uid was requested, so none is injected.
        assert!(doc["extensions"]["opamp"]["instance_uid"].is_null());
        let enabled: Vec<&str> = doc["service"]["extensions"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(enabled.contains(&"opamp"));
        assert!(enabled.contains(&"health_check"));
        assert!(doc["service"]["pipelines"]["logs"].is_mapping());
    }

    #[test]
    fn merge_preserves_existing_extensions() {
        let config = b"extensions:\n  health_check: {}\nservice:\n  extensions: [health_check]\n";
        let merged = merge_opamp_extension(config, "ws://x/v1/opamp", None);
        let doc: serde_yaml::Value = serde_yaml::from_slice(&merged).unwrap();
        let enabled: Vec<&str> = doc["service"]["extensions"]
            .as_sequence()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(enabled.contains(&"health_check"));
        assert!(enabled.contains(&"opamp"));
    }

    #[test]
    fn merge_leaves_a_non_mapping_config_unchanged() {
        let config = b"- just\n- a\n- list\n";
        assert_eq!(
            merge_opamp_extension(config, "ws://x/v1/opamp", None),
            config
        );
    }

    #[test]
    fn merge_injects_the_instance_uid_when_requested() {
        let merged = merge_opamp_extension(
            b"{}\n",
            "ws://x/v1/opamp",
            Some("0192abcd-1234-7abc-8def-0123456789ab"),
        );
        let doc: serde_yaml::Value = serde_yaml::from_slice(&merged).unwrap();
        assert_eq!(
            doc["extensions"]["opamp"]["instance_uid"].as_str(),
            Some("0192abcd-1234-7abc-8def-0123456789ab")
        );
    }

    #[test]
    fn uuidv7_string_accepts_v7_and_rejects_others() {
        // Version nibble (high nibble of byte 6) is 7 → accepted, formatted canonically.
        let mut v7 = [0u8; 16];
        v7[6] = 0x71;
        assert_eq!(uuidv7_string(&v7), Some(crate::uid::format(&v7)));
        // A v4 UID (version nibble 4) is rejected, so it is never injected.
        let mut v4 = [0u8; 16];
        v4[6] = 0x41;
        assert_eq!(uuidv7_string(&v4), None);
        // Wrong length is rejected.
        assert_eq!(uuidv7_string(&[0u8; 8]), None);
    }

    #[test]
    fn deep_merge_combines_distinct_and_overlapping_keys() {
        let files = vec![
            (
                "00-base.yaml".to_string(),
                b"receivers:\n  otlp: {}\nservice:\n  pipelines:\n    logs:\n      receivers: [otlp]\n"
                    .to_vec(),
            ),
            (
                "10-override.yaml".to_string(),
                b"exporters:\n  debug: {}\nservice:\n  pipelines:\n    logs:\n      exporters: [debug]\n"
                    .to_vec(),
            ),
        ];
        let merged = deep_merge_yaml(&files);
        let doc: serde_yaml::Value = serde_yaml::from_slice(&merged).unwrap();
        // Distinct top-level keys from both files survive.
        assert!(doc["receivers"]["otlp"].is_mapping());
        assert!(doc["exporters"]["debug"].is_mapping());
        // Nested mappings merged, not replaced: both receivers and exporters under the same pipeline.
        let logs = &doc["service"]["pipelines"]["logs"];
        assert_eq!(logs["receivers"][0].as_str(), Some("otlp"));
        assert_eq!(logs["exporters"][0].as_str(), Some("debug"));
    }

    #[test]
    fn deep_merge_later_file_wins_on_scalar_conflict() {
        let files = vec![
            (
                "00.yaml".to_string(),
                b"service:\n  telemetry:\n    logs:\n      level: info\n".to_vec(),
            ),
            (
                "10.yaml".to_string(),
                b"service:\n  telemetry:\n    logs:\n      level: debug\n".to_vec(),
            ),
        ];
        let doc: serde_yaml::Value = serde_yaml::from_slice(&deep_merge_yaml(&files)).unwrap();
        assert_eq!(
            doc["service"]["telemetry"]["logs"]["level"].as_str(),
            Some("debug")
        );
    }

    async fn agent_with_base(base: Option<Vec<u8>>) -> CollectorAgent {
        let (link, _addr) = crate::local_server::start("127.0.0.1:0")
            .await
            .expect("bind local server");
        CollectorAgent::new(
            Collector::new("/bin/true", "/tmp/opamp-merge-test.yaml"),
            link,
            "ws://127.0.0.1:0/v1/opamp".to_string(),
            &[0u8; 16],
            base,
        )
    }

    #[tokio::test]
    async fn merge_config_passes_a_single_file_through_without_a_base() {
        let agent = agent_with_base(None).await;
        let files = vec![("".to_string(), b"receivers: {}\n".to_vec())];
        assert_eq!(
            agent.merge_config(&files),
            Some(b"receivers: {}\n".to_vec())
        );
        assert_eq!(agent.merge_config(&[]), None);
    }

    #[test]
    fn merge_own_telemetry_injects_signal_exporters() {
        let own = OwnTelemetry {
            metrics: Some(TelemetryDestination {
                endpoint: "https://otlp.example/v1/metrics".to_string(),
                headers: [("Authorization".to_string(), "Bearer x".to_string())]
                    .into_iter()
                    .collect(),
            }),
            logs: Some(TelemetryDestination {
                endpoint: "https://otlp.example/v1/logs".to_string(),
                headers: Default::default(),
            }),
            traces: None,
        };
        let merged = merge_own_telemetry(b"service:\n  pipelines: {}\n", &own);
        let doc: serde_yaml::Value = serde_yaml::from_slice(&merged).unwrap();

        let metrics_otlp =
            &doc["service"]["telemetry"]["metrics"]["readers"][0]["periodic"]["exporter"]["otlp"];
        assert_eq!(
            metrics_otlp["endpoint"].as_str(),
            Some("https://otlp.example/v1/metrics")
        );
        assert_eq!(metrics_otlp["protocol"].as_str(), Some("http/protobuf"));
        assert_eq!(
            metrics_otlp["headers"][0]["name"].as_str(),
            Some("Authorization")
        );
        assert_eq!(
            metrics_otlp["headers"][0]["value"].as_str(),
            Some("Bearer x")
        );

        let logs_otlp =
            &doc["service"]["telemetry"]["logs"]["processors"][0]["batch"]["exporter"]["otlp"];
        assert_eq!(
            logs_otlp["endpoint"].as_str(),
            Some("https://otlp.example/v1/logs")
        );
        // No headers were offered for logs, so none are written.
        assert!(logs_otlp["headers"].is_null());
        // Traces were not offered, so no traces telemetry is written.
        assert!(doc["service"]["telemetry"]["traces"].is_null());
        // Unrelated config is preserved.
        assert!(doc["service"]["pipelines"].is_mapping());
    }

    #[test]
    fn merge_own_telemetry_leaves_config_unchanged_when_empty() {
        let config = b"service:\n  pipelines: {}\n";
        assert_eq!(
            merge_own_telemetry(config, &OwnTelemetry::default()),
            config
        );
    }

    #[tokio::test]
    async fn set_own_telemetry_reports_only_real_changes() {
        let mut agent = agent_with_base(None).await;
        let settings = OwnTelemetry {
            metrics: Some(TelemetryDestination {
                endpoint: "https://otlp.example/v1/metrics".to_string(),
                headers: Default::default(),
            }),
            ..Default::default()
        };
        assert!(
            agent.set_own_telemetry(settings.clone()),
            "first set is a change"
        );
        assert!(
            !agent.set_own_telemetry(settings),
            "same settings are not a change"
        );
        assert!(
            agent.set_own_telemetry(OwnTelemetry::default()),
            "clearing is a change"
        );
    }

    #[tokio::test]
    async fn merge_config_layers_the_base_under_the_remote_config() {
        let base =
            b"processors:\n  batch: {}\nservice:\n  telemetry:\n    logs:\n      level: warn\n";
        let agent = agent_with_base(Some(base.to_vec())).await;
        // Remote config shares a key with the base (the log level) and adds its own.
        let remote = vec![(
            "".to_string(),
            b"exporters:\n  debug: {}\nservice:\n  telemetry:\n    logs:\n      level: debug\n"
                .to_vec(),
        )];
        let doc: serde_yaml::Value =
            serde_yaml::from_slice(&agent.merge_config(&remote).unwrap()).unwrap();
        // Base-only key survives, remote-only key is present.
        assert!(doc["processors"]["batch"].is_mapping());
        assert!(doc["exporters"]["debug"].is_mapping());
        // On the shared key the remote wins over the base.
        assert_eq!(
            doc["service"]["telemetry"]["logs"]["level"].as_str(),
            Some("debug")
        );
        // With no remote config offered there is nothing to apply — the base alone is not applied here
        // (the startup fallback covers the pre-Server case).
        assert_eq!(agent.merge_config(&[]), None);
    }
}
