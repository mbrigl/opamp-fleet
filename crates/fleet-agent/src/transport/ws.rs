//! The WebSocket transport (ADR-0012): the connection is `opamp::client::ws`'s; what is the
//! Client's is the material it is built from — the TLS configuration, the credential, the limits
//! and the heartbeat in `supervisor.toml`.

use std::time::Duration;

use opamp::client::ws::{Connector, Settings};
use opamp::client::Ended;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderValue};
use tracing::warn;

use crate::config::ClientConfig;
use crate::engine::Engine;
use crate::shutdown::Shutdown;
use crate::transport::{RunOutcome, Upstream};

pub async fn run(
    engine: &mut Engine,
    config: &mut ClientConfig,
    shutdown: &mut Shutdown,
    telemetry: &crate::telemetry::Telemetry,
) -> Result<RunOutcome, String> {
    // Trust and identity in one configuration: a private CA when one is configured, and this
    // Client's client certificate when it has one (ADR-0012, ADR-0017).
    let connector = crate::tls::rustls_client_config(config)?.map(Connector::Rustls);

    // The Authorization header (ADR-0017, rotated per ADR-0018) rides the upgrade request — the
    // server checks it before the WebSocket comes up.
    let mut headers = HeaderMap::new();
    if let Some(value) = config.authorization_value()? {
        let mut value: HeaderValue = value
            .parse()
            .map_err(|e| format!("the [auth] credentials are not a valid header: {e}"))?;
        // Redact it from any `Debug` of the request headers, as the HTTP transport does: a
        // credential must not surface in a log line by accident.
        value.set_sensitive(true);
        if config.sends_credentials_in_cleartext() {
            warn!("sending credentials over unencrypted ws:// beyond the loopback — use wss://");
        }
        headers.insert(AUTHORIZATION, value);
    }

    let settings = Settings {
        endpoint: config.endpoint.clone(),
        headers,
        connector,
        max_message_size: config.max_message_size_bytes,
        // The heartbeat (ReportsHeartbeat, Baseline default 30 s; 0 disables).
        heartbeat: (config.heartbeat_interval_secs > 0)
            .then(|| Duration::from_secs(config.heartbeat_interval_secs)),
    };
    let mut session = Upstream {
        engine,
        config,
        shutdown: shutdown.clone(),
        telemetry,
    };
    Ok(
        match opamp::client::ws::run(&settings, &mut session, shutdown).await? {
            Ended::Stopped => RunOutcome::Shutdown,
            Ended::Reconnect => RunOutcome::Reconfigured,
            // The only end the Client asks for is the self-update restart (ADR-0021).
            Ended::End => RunOutcome::RestartForUpdate,
        },
    )
}
