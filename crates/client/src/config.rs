//! The Client's own configuration file — TOML (ADR-0008).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// `client.toml`. Every setting has a default; unknown keys are rejected so a typo fails loudly at
/// startup instead of silently applying a default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// The Server's OpAMP endpoint. The URL scheme selects the transport (ADR-0007):
    /// `ws://` / `wss://` is the WebSocket transport, `http://` / `https://` the polling one.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// The Agent's `service.name`, its human identity in the fleet.
    #[serde(default = "default_name")]
    pub name: String,
    /// How often the plain-HTTP transport polls. The Baseline's default is 30 seconds; ignored on
    /// WebSocket, where the Server pushes.
    #[serde(default = "default_poll_interval_secs")]
    pub poll_interval_secs: u64,
    /// Where the Client persists its identity and the received remote configuration.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
}

/// The transport the endpoint's scheme selects (ADR-0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    WebSocket,
    Http,
}

fn default_endpoint() -> String {
    // The Baseline's default port and path.
    "ws://127.0.0.1:4320/v1/opamp".to_string()
}

fn default_name() -> String {
    "opamp-fleet-client".to_string()
}

fn default_poll_interval_secs() -> u64 {
    30
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("client-state")
}

impl Default for ClientConfig {
    fn default() -> Self {
        ClientConfig {
            endpoint: default_endpoint(),
            name: default_name(),
            poll_interval_secs: default_poll_interval_secs(),
            state_dir: default_state_dir(),
        }
    }
}

impl ClientConfig {
    /// Loads the file, or the defaults when it does not exist. A file that exists but does not
    /// parse is an error — never silently ignored.
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(ClientConfig::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))
    }

    pub fn transport(&self) -> Result<TransportKind, String> {
        match self.endpoint.split("://").next() {
            Some("ws") | Some("wss") => Ok(TransportKind::WebSocket),
            Some("http") | Some("https") => Ok(TransportKind::Http),
            _ => Err(format!(
                "endpoint {} must start with ws://, wss://, http:// or https://",
                self.endpoint
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_select_websocket_on_port_4320() {
        let cfg = ClientConfig::default();
        assert_eq!(
            cfg.transport().expect("transport"),
            TransportKind::WebSocket
        );
        assert!(cfg.endpoint.contains(":4320/v1/opamp"));
        assert_eq!(cfg.poll_interval_secs, 30);
    }

    #[test]
    fn scheme_selects_the_transport() {
        for (endpoint, kind) in [
            ("ws://x/v1/opamp", TransportKind::WebSocket),
            ("wss://x/v1/opamp", TransportKind::WebSocket),
            ("http://x/v1/opamp", TransportKind::Http),
            ("https://x/v1/opamp", TransportKind::Http),
        ] {
            let cfg = ClientConfig {
                endpoint: endpoint.to_string(),
                ..ClientConfig::default()
            };
            assert_eq!(cfg.transport().expect("transport"), kind);
        }
    }

    #[test]
    fn rejects_an_unknown_scheme_and_unknown_keys() {
        let cfg = ClientConfig {
            endpoint: "ftp://x".to_string(),
            ..ClientConfig::default()
        };
        assert!(cfg.transport().is_err());
        assert!(toml::from_str::<ClientConfig>("endpont = \"ws://x\"").is_err());
    }
}
