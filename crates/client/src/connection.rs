//! Server-offered connection settings (ADR-0014): persistence, their precedence over
//! `supervisor.toml`, and the verify-by-actually-connecting the Baseline requires.
//!
//! The persisted file is the Baseline's own `ConnectionSettingsOffers` protobuf — the merged
//! settings currently in force plus the hash that reports them `APPLIED`. It lives at the
//! `state_dir` root because the settings belong to the Client's one upstream connection, not to
//! any single Agent. Deleting the file reverts to `supervisor.toml`.

use std::path::Path;

use opamp::proto::{
    AgentToServer, ConnectionSettingsOffers, OpAmpConnectionSettings, TelemetryConnectionSettings,
};
use prost::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tracing::warn;

use crate::config::ClientConfig;

const SETTINGS_FILE: &str = "connection-settings.pb";

/// The persisted settings in force, or `None` on a fresh state dir (an unreadable file is
/// dropped with a warning — `supervisor.toml` then applies, never a half-read override).
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
///
/// The file holds the Server-rotated `Authorization` value (ADR-0014), which outranks the one in
/// `supervisor.toml` — so it is written no wider than its owner, and the state directory holding it no
/// wider than `0700`. On a multi-user host the default umask would otherwise leave the live fleet
/// credential world-readable. On Windows the directory ACL under `%ProgramData%` protects it
/// (ADR-0010); there is no mode to set.
pub fn store(state_dir: &Path, settings: &ConnectionSettingsOffers) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
        // Narrow the directory too: create_dir_all leaves it at the umask default, and the file's
        // own mode is no protection if the directory it sits in is world-traversable and listable.
        std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700))?;
        let path = state_dir.join(SETTINGS_FILE);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        // The open sets the mode only when it creates the file; narrow a pre-existing one too.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&settings.encode_to_vec())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(state_dir.join(SETTINGS_FILE), settings.encode_to_vec())
    }
}

/// Folds a verified offer over what was already in force.
///
/// The **OpAMP** settings carry only what changes — a headers-only rotation must not erase a
/// previously offered endpoint, and vice versa. The **own-telemetry** destinations do not: an offer
/// that names any of them states all three (ADR-0089). The two rules live in one function because
/// one message carries both, and the difference between them is the whole of what this fold does.
pub fn merge(
    stored: Option<&ConnectionSettingsOffers>,
    offer: &ConnectionSettingsOffers,
) -> ConnectionSettingsOffers {
    let previous = stored.and_then(|s| s.opamp.as_ref());
    let offered = offer.opamp.as_ref();
    let pick = |field: fn(&OpAmpConnectionSettings) -> bool| -> Option<OpAmpConnectionSettings> {
        offered.filter(|s| field(s)).or(previous).cloned()
    };
    // The own-telemetry destinations do not fold per signal (ADR-0089). An offer that names any of
    // the three states all three: a signal it leaves out is *stopped*, and a signal whose endpoint
    // it offers empty is withdrawn. An offer that names none of them says nothing about telemetry
    // — an OpAMP endpoint move, a credential rotation, a certificate — and leaves all three alone.
    //
    // The line is between messages, not between fields, and that is what keeps it compatible with
    // the schema's per-field "if this field is not set … the settings are unchanged": unchanged
    // holds for an offer that is silent about telemetry. For one that speaks about it, the message
    // is the whole state — the reading the reference implementation has, and the only one in which
    // a destination can ever be taken away.
    let states_telemetry =
        offer.own_metrics.is_some() || offer.own_traces.is_some() || offer.own_logs.is_some();
    let telemetry = |offered: Option<&TelemetryConnectionSettings>,
                     previous: Option<&TelemetryConnectionSettings>| {
        if states_telemetry {
            offered
                .filter(|s| !s.destination_endpoint.is_empty())
                .cloned()
        } else {
            previous.cloned()
        }
    };
    ConnectionSettingsOffers {
        hash: offer.hash.clone(),
        own_metrics: telemetry(
            offer.own_metrics.as_ref(),
            stored.and_then(|s| s.own_metrics.as_ref()),
        ),
        own_traces: telemetry(
            offer.own_traces.as_ref(),
            stored.and_then(|s| s.own_traces.as_ref()),
        ),
        own_logs: telemetry(
            offer.own_logs.as_ref(),
            stored.and_then(|s| s.own_logs.as_ref()),
        ),
        // Built only when one of the two sides actually has OpAMP settings (ADR-0086 clause 6).
        // Emitting a block unconditionally would have a telemetry-only offer persist the claim that
        // the Server offered OpAMP settings it never offered — a lie in the one file an operator is
        // told to inspect and delete, and one that makes the honest assertion untestable.
        opamp: (offered.is_some() || previous.is_some()).then(|| OpAmpConnectionSettings {
            destination_endpoint: pick(|s| !s.destination_endpoint.is_empty())
                .map(|s| s.destination_endpoint)
                .unwrap_or_default(),
            headers: pick(|s| s.headers.is_some()).and_then(|s| s.headers),
            // The issued client identity (ADR-0035). Folded like every other field: a later offer
            // that says nothing about the certificate leaves the one in force alone, which is what
            // makes an endpoint or credential rotation safe for a fleet already on mutual TLS.
            certificate: pick(|s| s.certificate.is_some()).and_then(|s| s.certificate),
            heartbeat_interval_seconds: pick(|s| s.heartbeat_interval_seconds != 0)
                .map(|s| s.heartbeat_interval_seconds)
                .unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Whether an offer carries anything this Client can put in force (ADR-0086 clause 1): OpAMP
/// settings, or a destination for one of the three own-telemetry signals.
///
/// `other_connections` deliberately does not count. `AcceptsOtherConnectionSettings` is undeclared,
/// so a conforming Server never sends one — and acknowledging what cannot be applied is the lie this
/// whole path exists to prevent.
pub fn carries_settings(offers: &ConnectionSettingsOffers) -> bool {
    offers.opamp.is_some()
        || offers.own_metrics.is_some()
        || offers.own_traces.is_some()
        || offers.own_logs.is_some()
}

/// What to report for an offer that has been verified and applied: `Ok` when the Client honoured
/// all of it, `Err` naming the fields it dropped (ADR-0035).
///
/// The Client applies what it understands and then says so. Reporting `APPLIED` for an offer whose
/// `tls` or `proxy` it silently discarded — which is what it used to do — tells the Server the
/// settings are in force when they are not, and the Server has no way to find out. `FAILED` with
/// the field names is the honest answer; the hash is echoed either way, so this does not put the
/// Server into a re-offer loop.
///
/// Neither field is honoured on purpose. `TLSConnectionSettings` is mostly a way to weaken
/// verification — `insecure_skip_verify` would let a Server switch off the check that proves it is
/// the Server — and trust here is an operator's file (ADR-0007). `ProxyConnectionSettings` has
/// nothing on this Client to configure. Both are `[Development]` upstream.
pub fn unhonoured(settings: &OpAmpConnectionSettings) -> Result<(), String> {
    let mut dropped = Vec::new();
    if settings.tls.is_some() {
        dropped.push("tls");
    }
    if settings.proxy.is_some() {
        dropped.push("proxy");
    }
    if dropped.is_empty() {
        return Ok(());
    }
    Err(format!(
        "applied everything else, but this Client does not implement the offered {} \
         connection settings",
        dropped.join(" and ")
    ))
}

/// The `Authorization` value an offer carries, if any.
pub fn offered_authorization(settings: &OpAmpConnectionSettings) -> Option<&str> {
    settings.headers.as_ref()?.headers.iter().find_map(|h| {
        h.key
            .eq_ignore_ascii_case("authorization")
            .then_some(h.value.as_str())
    })
}

/// Applies persisted settings over the loaded `supervisor.toml` (ADR-0014): the Server's word wins
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
    // An offered client certificate is proved the same way the endpoint and the credential are:
    // by connecting with it (ADR-0035). Until that succeeds the one in force stays in force, so a
    // certificate that cannot authenticate costs nothing.
    let candidate_cert = settings
        .certificate
        .as_ref()
        .map(|certificate| certificate.cert.as_slice())
        .filter(|cert| !cert.is_empty());

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
            let connector = crate::tls::rustls_client_config_for(config, candidate_cert)?
                .map(tokio_tungstenite::Connector::Rustls);
            let (mut socket, _) =
                tokio_tungstenite::connect_async_tls_with_config(request, None, false, connector)
                    .await
                    .map_err(|e| format!("cannot connect to {endpoint}: {e}"))?;
            let _ = futures_util::SinkExt::close(&mut socket).await;
            Ok(())
        }
        "http" | "https" => {
            let report = probe_report().ok_or("no agent to build a probe report from")?;
            let builder = crate::tls::trust_and_identity_for(
                reqwest::Client::builder()
                    .use_rustls_tls()
                    // A candidate OpAMP endpoint (ADR-0014) is verified by connecting to exactly it;
                    // a redirect would defeat the point, so this probe never follows one.
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(std::time::Duration::from_secs(30)),
                config,
                candidate_cert,
            )?;
            let client = builder
                .build()
                .map_err(|e| format!("cannot build the probe client: {e}"))?;
            let mut request = client
                .post(&endpoint)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    opamp::endpoint::PROTOBUF_CONTENT_TYPE,
                )
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

    fn telemetry_only(hash: &[u8], endpoint: &str) -> ConnectionSettingsOffers {
        ConnectionSettingsOffers {
            hash: hash.to_vec(),
            own_metrics: Some(TelemetryConnectionSettings {
                destination_endpoint: endpoint.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// ADR-0086 clause 1: an offer that names a telemetry destination is actionable, whether or not
    /// it carries OpAMP settings — and one that carries nothing this Client applies is not.
    #[test]
    fn an_offer_carries_settings_when_it_names_anything_this_client_applies() {
        assert!(carries_settings(&telemetry_only(
            b"h",
            "https://x/v1/metrics"
        )));
        assert!(carries_settings(&offer_with(
            b"h",
            "wss://x/v1/opamp",
            None,
            0
        )));
        assert!(!carries_settings(&ConnectionSettingsOffers::default()));
    }

    /// Clause 6: what is persisted says only what was offered. A telemetry-only offer against a
    /// fresh state directory must not leave behind an empty `opamp` block claiming the Server
    /// offered settings it never sent.
    #[test]
    fn merge_leaves_opamp_absent_when_neither_side_has_one() {
        let merged = merge(None, &telemetry_only(b"t1", "https://x/v1/metrics"));
        assert!(merged.opamp.is_none());
        assert!(merged.own_metrics.is_some());
        assert_eq!(merged.hash, b"t1");
    }

    /// ADR-0089 rule 1: an offer that names any telemetry destination states all three. The
    /// traces endpoint in force is *stopped* by a metrics-only offer, not carried forward — which
    /// is the whole difference between a fleet that can turn a signal off and one that cannot.
    #[test]
    fn an_offer_naming_one_signal_stops_the_others() {
        let mut stored = telemetry_only(b"t1", "https://x/v1/metrics");
        stored.own_traces = Some(TelemetryConnectionSettings {
            destination_endpoint: "https://x/v1/traces".to_string(),
            ..Default::default()
        });

        let merged = merge(
            Some(&stored),
            &telemetry_only(b"t2", "https://y/v1/metrics"),
        );

        assert_eq!(
            merged.own_metrics.expect("metrics").destination_endpoint,
            "https://y/v1/metrics",
            "the offered destination replaces the one in force"
        );
        assert!(
            merged.own_traces.is_none(),
            "a signal the offer does not name is stopped"
        );
    }

    /// Rule 2: an offer that names none of the three says nothing about telemetry. A credential
    /// rotation must not take the exporters down with it — that is what keeps the classes of
    /// ADR-0086 independent, and it is the schema's own "not set means unchanged", held at the
    /// level it still holds at.
    #[test]
    fn an_offer_silent_about_telemetry_leaves_all_three_alone() {
        let mut stored = telemetry_only(b"t1", "https://x/v1/metrics");
        stored.own_logs = Some(TelemetryConnectionSettings {
            destination_endpoint: "https://x/v1/logs".to_string(),
            ..Default::default()
        });

        let merged = merge(Some(&stored), &offer_with(b"h2", "", Some("Bearer new"), 0));

        assert_eq!(
            merged.own_metrics.expect("metrics").destination_endpoint,
            "https://x/v1/metrics"
        );
        assert_eq!(
            merged.own_logs.expect("logs").destination_endpoint,
            "https://x/v1/logs"
        );
    }

    /// Rule 3: an endpoint offered empty withdraws that signal — the only way to say "all three
    /// off", since by rule 2 an offer that names nothing means "unchanged". The withdrawal leaves
    /// the persisted state, so a restart does not bring the destination back.
    #[test]
    fn an_empty_endpoint_withdraws_the_signal() {
        let stored = telemetry_only(b"t1", "https://x/v1/metrics");
        let merged = merge(Some(&stored), &telemetry_only(b"t2", ""));

        assert!(
            merged.own_metrics.is_none(),
            "an empty endpoint is a withdrawal, not a destination"
        );
        assert_eq!(merged.hash, b"t2", "and it is acknowledged like any offer");
    }

    /// And the fold still works the other way: a telemetry-only offer arriving over settings
    /// already in force leaves the OpAMP endpoint and credential exactly where they were.
    #[test]
    fn merge_of_a_telemetry_only_offer_carries_the_opamp_settings_in_force_forward() {
        let stored = offer_with(b"h1", "wss://server/v1/opamp", Some("Bearer t"), 20);
        let merged = merge(
            Some(&stored),
            &telemetry_only(b"t2", "https://x/v1/metrics"),
        );

        let opamp = merged.opamp.expect("the settings in force survive");
        assert_eq!(opamp.destination_endpoint, "wss://server/v1/opamp");
        assert_eq!(opamp.heartbeat_interval_seconds, 20);
        assert_eq!(offered_authorization(&opamp), Some("Bearer t"));
        assert!(merged.own_metrics.is_some());
        assert_eq!(merged.hash, b"t2", "the new offer's hash is acknowledged");
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

    /// The persisted file holds the live, Server-rotated credential, so it — and the directory it
    /// sits in — must not be readable by another user on the host.
    #[cfg(unix)]
    #[test]
    fn stored_settings_and_their_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let state_dir = dir.path().join("state");
        store(
            &state_dir,
            &offer_with(b"h1", "wss://x/v1/opamp", Some("Bearer secret"), 20),
        )
        .expect("store");

        let file_mode = state_dir
            .join(SETTINGS_FILE)
            .metadata()
            .expect("file metadata")
            .permissions()
            .mode();
        assert_eq!(
            file_mode & 0o777,
            0o600,
            "the credential file is owner-only"
        );
        let dir_mode = state_dir
            .metadata()
            .expect("dir metadata")
            .permissions()
            .mode();
        assert_eq!(dir_mode & 0o777, 0o700, "the state directory is owner-only");
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
