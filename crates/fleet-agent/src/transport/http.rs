//! The plain-HTTP(S) transport (ADR-0012): the polling is `opamp::client::http`'s; what is the
//! Client's is the HTTP client it polls with — trust, identity, the credential and the timeout —
//! and the poll interval and limits in `supervisor.toml`.

use std::time::Duration;

use opamp::client::http::Settings;
use opamp::client::Ended;
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
    let mut builder = reqwest::Client::builder()
        .use_rustls_tls()
        // The OpAMP endpoint is a fixed, operator-configured address; it never legitimately
        // redirects, so following one would only let a compromised or misconfigured Server bounce
        // the authenticated session elsewhere. Refuse them.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30));
    if let Some(value) = config.authorization_value()? {
        // The Authorization header (ADR-0017, rotated per ADR-0018) rides every request, the
        // disconnect included.
        let mut value = reqwest::header::HeaderValue::from_str(&value)
            .map_err(|e| format!("the [auth] credentials are not a valid header: {e}"))?;
        value.set_sensitive(true);
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::AUTHORIZATION, value);
        builder = builder.default_headers(headers);
        if config.sends_credentials_in_cleartext() {
            warn!(
                "sending credentials over unencrypted http:// beyond the loopback — use https://"
            );
        }
    }
    // Trust, plus this Client's own certificate when it has one — a Server on mutual TLS asks for
    // it on every request of this transport (ADR-0017).
    builder = crate::tls::trust_and_identity(builder, config)?;
    let settings = Settings {
        endpoint: config.endpoint.clone(),
        client: builder
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {e}"))?,
        poll: Duration::from_secs(config.poll_interval_secs.max(1)),
        max_message_size: config.max_message_size_bytes,
    };
    let mut session = Upstream {
        engine,
        config,
        shutdown: shutdown.clone(),
        telemetry,
    };
    Ok(
        match opamp::client::http::run(&settings, &mut session, shutdown).await? {
            Ended::Stopped => RunOutcome::Shutdown,
            Ended::Reconnect => RunOutcome::Reconfigured,
            // The only end the Client asks for is the self-update restart (ADR-0021).
            Ended::End => RunOutcome::RestartForUpdate,
        },
    )
}
