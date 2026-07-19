//! The [`ManagedAgent`] adapter for an OpAMP-native OpenTelemetry Collector — the **Collector
//! Supervisor** (ADR-0008, ADR-0009).
//!
//! It owns an `otelcol` process ([`Collector`]), injects the `opamp`/`health_check` extensions into the
//! config so the collector reports back over a **local OpAMP server** ([`crate::local_server`]), and
//! surfaces the collector's *actual* health and effective config through the port. Behaviour matches the
//! upstream Go Supervisor oracle; only its seams sit behind [`ManagedAgent`].

use std::time::{SystemTime, UNIX_EPOCH};

use crate::agent::{liveness_health, AgentStatus, ChangeSignal, ManagedAgent};
use crate::collector::Collector;
use crate::local_server::CollectorLink;

/// A Collector under management: the process, the link to its local OpAMP reports, and the local
/// endpoint injected into its config.
pub struct CollectorAgent {
    collector: Collector,
    link: CollectorLink,
    local_opamp_endpoint: String,
    start_time_unix_nano: u64,
}

impl CollectorAgent {
    pub fn new(collector: Collector, link: CollectorLink, local_opamp_endpoint: String) -> Self {
        let start_time_unix_nano = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            collector,
            link,
            local_opamp_endpoint,
            start_time_unix_nano,
        }
    }
}

impl ManagedAgent for CollectorAgent {
    /// Inject the `opamp`/`health_check` extensions so the collector connects back to the local server.
    /// Re-injection is idempotent apart from the endpoint, so re-preparing an already-merged config on
    /// resume just re-points it at this run's local server.
    fn prepare_config(&self, config: Vec<u8>) -> Vec<u8> {
        merge_opamp_extension(&config, &self.local_opamp_endpoint)
    }

    async fn apply(&mut self, config: &[u8]) -> Result<(), String> {
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

    async fn supervise(&mut self) -> Option<String> {
        self.collector
            .check_exited()
            .map(|status| status.to_string())
    }
}

/// Injects the collector's `opamp` and `health_check` extensions into a collector configuration and
/// lists them under `service.extensions`. The `opamp` extension, pointed at the local OpAMP server, is
/// what makes the collector connect back and report its real health and effective config; `health_check`
/// exposes the collector's health for it to observe. Existing extensions and the rest of the config are
/// preserved. If the config is not a YAML mapping it is returned unchanged — validation will reject a
/// genuinely broken config, and we do not want to mask that.
fn merge_opamp_extension(config: &[u8], endpoint: &str) -> Vec<u8> {
    let mut doc: serde_yaml::Value = match serde_yaml::from_slice(config) {
        Ok(doc) => doc,
        Err(_) => return config.to_vec(),
    };
    let Some(root) = doc.as_mapping_mut() else {
        return config.to_vec();
    };

    let opamp = match serde_yaml::from_str::<serde_yaml::Value>(&format!(
        "server:\n  ws:\n    endpoint: \"{endpoint}\"\n    tls:\n      insecure: true\n"
    )) {
        Ok(value) => value,
        Err(_) => return config.to_vec(),
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_injects_the_opamp_extension_and_enables_it() {
        let config = b"exporters:\n  debug: {}\nservice:\n  pipelines:\n    logs:\n      exporters: [debug]\n";
        let merged = merge_opamp_extension(config, "ws://127.0.0.1:9999/v1/opamp");
        let doc: serde_yaml::Value = serde_yaml::from_slice(&merged).unwrap();

        let endpoint = doc["extensions"]["opamp"]["server"]["ws"]["endpoint"]
            .as_str()
            .unwrap();
        assert_eq!(endpoint, "ws://127.0.0.1:9999/v1/opamp");
        assert!(!doc["extensions"]["health_check"].is_null());
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
        let merged = merge_opamp_extension(config, "ws://x/v1/opamp");
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
        assert_eq!(merge_opamp_extension(config, "ws://x/v1/opamp"), config);
    }
}
