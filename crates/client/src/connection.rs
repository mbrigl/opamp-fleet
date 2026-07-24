//! Server-offered connection settings (ADR-0014): persistence, their precedence over
//! `client.toml`, and the verify-by-actually-connecting the Baseline requires.
//!
//! The persisted file is the Baseline's own `ConnectionSettingsOffers` protobuf — the merged
//! settings currently in force plus the hash that reports them `APPLIED`. It lives at the
//! `state_dir` root because the settings belong to the Client's one upstream connection, not to
//! any single Agent. Deleting the file reverts to `client.toml`.

use std::path::Path;

use opamp::proto::{AgentToServer, ConnectionSettingsOffers, OpAmpConnectionSettings};
use prost::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tracing::warn;

use crate::config::ClientConfig;

const SETTINGS_FILE: &str = "connection-settings.pb";

/// The persisted settings in force, or `None` on a fresh state dir (an unreadable file is
/// dropped with a warning — `client.toml` then applies, never a half-read override).
pub fn load(state_dir: &Path) -> Option<ConnectionSettingsOffers> {
    let path = state_dir.join(SETTINGS_FILE);
    let bytes = std::fs::read(&path).ok()?;
    match ConnectionSettingsOffers::decode(bytes.as_slice()) {
        Ok(stored) => Some(stored),
        Err(e) => {
            warn!(file = %path.display(), error = %e, "unreadable connection settings; ignoring");
            None
        }
    }
}

/// Persists the settings now in force, losslessly as the received protobuf.
pub fn store(state_dir: &Path, settings: &ConnectionSettingsOffers) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    std::fs::write(state_dir.join(SETTINGS_FILE), settings.encode_to_vec())
}

/// Folds a verified offer over what was already in force. An offer carries only what changes —
/// a headers-only rotation must not erase a previously offered endpoint, and vice versa.
pub fn merge(
    stored: Option<&ConnectionSettingsOffers>,
    offer: &ConnectionSettingsOffers,
) -> ConnectionSettingsOffers {
    let previous = stored.and_then(|s| s.opamp.as_ref());
    let offered = offer.opamp.as_ref();
    let pick = |field: fn(&OpAmpConnectionSettings) -> bool| -> Option<OpAmpConnectionSettings> {
        offered.filter(|s| field(s)).or(previous).cloned()
    };
    ConnectionSettingsOffers {
        hash: offer.hash.clone(),
        opamp: Some(OpAmpConnectionSettings {
            destination_endpoint: pick(|s| !s.destination_endpoint.is_empty())
                .map(|s| s.destination_endpoint)
                .unwrap_or_default(),
            headers: pick(|s| s.headers.is_some()).and_then(|s| s.headers),
            heartbeat_interval_seconds: pick(|s| s.heartbeat_interval_seconds != 0)
                .map(|s| s.heartbeat_interval_seconds)
                .unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The `Authorization` value an offer carries, if any.
pub fn offered_authorization(settings: &OpAmpConnectionSettings) -> Option<&str> {
    settings.headers.as_ref()?.headers.iter().find_map(|h| {
        h.key
            .eq_ignore_ascii_case("authorization")
            .then_some(h.value.as_str())
    })
}

/// Applies persisted settings over the loaded `client.toml` (ADR-0014): the Server's word wins
/// where it spoke — endpoint, credential, heartbeat (on plain HTTP the same value is the polling
/// interval, the Baseline's MUST) — and the file's word stays everywhere else.
pub fn apply(config: &mut ClientConfig, stored: &ConnectionSettingsOffers) {
    let Some(settings) = &stored.opamp else {
        return;
    };
    if !settings.destination_endpoint.is_empty() {
        config.endpoint = settings.destination_endpoint.clone();
    }
    if let Some(authorization) = offered_authorization(settings) {
        config.authorization_override = Some(authorization.to_string());
    }
    if settings.heartbeat_interval_seconds != 0 {
        config.heartbeat_interval_secs = settings.heartbeat_interval_seconds;
        config.poll_interval_secs = settings.heartbeat_interval_seconds;
    }
}

/// Verifies an offer by actually connecting (the Baseline's MUST) with the candidate settings:
/// offered fields, falling back to the current ones. A WebSocket candidate must complete its
/// handshake; a plain-HTTP candidate must complete a real exchange, fed by `probe_report`. The
/// current TLS trust override applies to the candidate too.
pub async fn verify(
    settings: &OpAmpConnectionSettings,
    config: &ClientConfig,
    probe_report: impl FnOnce() -> Option<AgentToServer>,
) -> Result<(), String> {
    let endpoint = if settings.destination_endpoint.is_empty() {
        config.endpoint.clone()
    } else {
        settings.destination_endpoint.clone()
    };
    let authorization = match offered_authorization(settings) {
        Some(offered) => Some(offered.to_string()),
        None => config.authorization_value()?,
    };

    let scheme = endpoint.split("://").next().unwrap_or("");
    match scheme {
        "ws" | "wss" => {
            let mut request = endpoint
                .as_str()
                .into_client_request()
                .map_err(|e| format!("invalid offered endpoint {endpoint}: {e}"))?;
            if let Some(value) = &authorization {
                request.headers_mut().insert(
                    AUTHORIZATION,
                    value
                        .parse()
                        .map_err(|e| format!("offered credentials are not a valid header: {e}"))?,
                );
            }
            let connector = match &config.tls {
                Some(tls) => Some(tokio_tungstenite::Connector::Rustls(
                    crate::tls::rustls_config_with_ca(&tls.ca_file)?,
                )),
                None => None,
            };
            let (mut socket, _) =
                tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector)
                    .await
                    .map_err(|e| format!("cannot connect to {endpoint}: {e}"))?;
            let _ = futures_util::SinkExt::close(&mut socket).await;
            Ok(())
        }
        "http" | "https" => {
            let report = probe_report().ok_or("no agent to build a probe report from")?;
            let mut builder = reqwest::Client::builder()
                .use_rustls_tls()
                .timeout(std::time::Duration::from_secs(30));
            if let Some(tls) = &config.tls {
                let pem = std::fs::read(&tls.ca_file)
                    .map_err(|e| format!("cannot read {}: {e}", tls.ca_file.display()))?;
                let ca = reqwest::Certificate::from_pem(&pem)
                    .map_err(|e| format!("cannot parse {}: {e}", tls.ca_file.display()))?;
                builder = builder
                    .tls_built_in_root_certs(false)
                    .add_root_certificate(ca);
            }
            let client = builder
                .build()
                .map_err(|e| format!("cannot build the probe client: {e}"))?;
            let mut request = client
                .post(&endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/x-protobuf")
                .body(report.encode_to_vec());
            if let Some(value) = &authorization {
                request = request.header(reqwest::header::AUTHORIZATION, value);
            }
            let response = request
                .send()
                .await
                .map_err(|e| format!("cannot reach {endpoint}: {e}"))?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("{endpoint} answered {status}"));
            }
            Ok(())
        }
        _ => Err(format!(
            "offered endpoint {endpoint} must start with ws://, wss://, http:// or https://"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opamp::proto::{Header, Headers};

    fn offer_with(
        hash: &[u8],
        endpoint: &str,
        authorization: Option<&str>,
        heartbeat: u64,
    ) -> ConnectionSettingsOffers {
        ConnectionSettingsOffers {
            hash: hash.to_vec(),
            opamp: Some(OpAmpConnectionSettings {
                destination_endpoint: endpoint.to_string(),
                headers: authorization.map(|value| Headers {
                    headers: vec![Header {
                        key: "Authorization".to_string(),
                        value: value.to_string(),
                    }],
                }),
                heartbeat_interval_seconds: heartbeat,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn load_store_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(load(dir.path()).is_none(), "fresh state dir holds nothing");
        let settings = offer_with(b"h1", "wss://x/v1/opamp", Some("Bearer t"), 20);
        store(dir.path(), &settings).expect("store");
        let restored = load(dir.path()).expect("restored");
        assert_eq!(restored.hash, b"h1");
        assert_eq!(
            restored.opamp.unwrap().destination_endpoint,
            "wss://x/v1/opamp"
        );
    }

    #[test]
    fn merge_keeps_unchanged_fields_from_the_previous_settings() {
        let stored = offer_with(b"h1", "wss://old/v1/opamp", Some("Bearer old"), 30);
        // A headers-only rotation: new credential, no endpoint, no heartbeat.
        let offer = offer_with(b"h2", "", Some("Bearer new"), 0);
        let merged = merge(Some(&stored), &offer);
        let settings = merged.opamp.expect("opamp");
        assert_eq!(merged.hash, b"h2", "the merged hash is the new offer's");
        assert_eq!(
            settings.destination_endpoint, "wss://old/v1/opamp",
            "the endpoint carries over"
        );
        assert_eq!(offered_authorization(&settings), Some("Bearer new"));
        assert_eq!(
            settings.heartbeat_interval_seconds, 30,
            "the heartbeat carries over"
        );
    }

    #[test]
    fn apply_overrides_client_toml_where_the_server_spoke() {
        let mut config = ClientConfig {
            endpoint: "ws://file/v1/opamp".to_string(),
            heartbeat_interval_secs: 30,
            poll_interval_secs: 30,
            ..ClientConfig::default()
        };
        let stored = offer_with(b"h1", "wss://server/v1/opamp", Some("Bearer rotated"), 12);
        apply(&mut config, &stored);
        assert_eq!(config.endpoint, "wss://server/v1/opamp");
        assert_eq!(
            config.authorization_override,
            Some("Bearer rotated".to_string())
        );
        // On plain HTTP the offered interval is the polling interval too (the Baseline's MUST).
        assert_eq!(config.heartbeat_interval_secs, 12);
        assert_eq!(config.poll_interval_secs, 12);
        // The rotated credential wins over the file's [auth].
        assert_eq!(
            config.authorization_value().expect("value"),
            Some("Bearer rotated".to_string())
        );
    }

    #[test]
    fn apply_leaves_untouched_what_the_offer_omits() {
        let mut config = ClientConfig {
            endpoint: "ws://file/v1/opamp".to_string(),
            heartbeat_interval_secs: 30,
            ..ClientConfig::default()
        };
        // Endpoint-only offer: heartbeat and credential stay whatever the file said.
        let stored = offer_with(b"h1", "wss://server/v1/opamp", None, 0);
        apply(&mut config, &stored);
        assert_eq!(config.endpoint, "wss://server/v1/opamp");
        assert_eq!(config.heartbeat_interval_secs, 30);
        assert_eq!(config.authorization_override, None);
    }
}
