//! Server-offered connection settings (ADR-0018): persistence, their precedence over
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
/// The file holds the Server-rotated `Authorization` value (ADR-0018), which outranks the one in
/// `supervisor.toml` — so it is written no wider than its owner, and the state directory holding it no
/// wider than `0700`. On a multi-user host the default umask would otherwise leave the live fleet
/// credential world-readable. On Windows the directory ACL under `%ProgramData%` protects it
/// (ADR-0014); there is no mode to set.
pub fn store(state_dir: &Path, settings: &ConnectionSettingsOffers) -> std::io::Result<()> {
    crate::storage::create_private_dir(state_dir)?;
    crate::storage::write_private(&state_dir.join(SETTINGS_FILE), &settings.encode_to_vec())
}

/// Folds a verified offer over what was already in force.
///
/// The **OpAMP** settings carry only what changes — a headers-only rotation must not erase a
/// previously offered endpoint, and vice versa. The **own-telemetry** destinations do not: an offer
/// that names any of them states all three (ADR-0025). The two rules live in one function because
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
    // The own-telemetry destinations do not fold per signal (ADR-0025). An offer that names any of
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
        // Built only when one of the two sides actually has OpAMP settings (ADR-0018 clause 9).
        // Emitting a block unconditionally would have a telemetry-only offer persist the claim that
        // the Server offered OpAMP settings it never offered — a lie in the one file an operator is
        // told to inspect and delete, and one that makes the honest assertion untestable.
        opamp: (offered.is_some() || previous.is_some()).then(|| OpAmpConnectionSettings {
            destination_endpoint: pick(|s| !s.destination_endpoint.is_empty())
                .map(|s| s.destination_endpoint)
                .unwrap_or_default(),
            headers: pick(|s| s.headers.is_some()).and_then(|s| s.headers),
            // The issued client identity (ADR-0017). Folded like every other field: a later offer
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

/// What to report for an offer that has been verified and applied: `Ok` when the Client honoured
/// all of it, `Err` naming the fields it dropped (ADR-0017).
///
/// The Client applies what it understands and then says so. Reporting `APPLIED` for an offer whose
/// `tls` or `proxy` it silently discarded — which is what it used to do — tells the Server the
/// settings are in force when they are not, and the Server has no way to find out. `FAILED` with
/// the field names is the honest answer; the hash is echoed either way, so this does not put the
/// Server into a re-offer loop.
///
/// Neither field is honoured on purpose. `TLSConnectionSettings` is mostly a way to weaken
/// verification — `insecure_skip_verify` would let a Server switch off the check that proves it is
/// the Server — and trust here is an operator's file (ADR-0012). `ProxyConnectionSettings` has
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

/// Applies persisted settings over the loaded `supervisor.toml` (ADR-0018): the Server's word wins
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
    // A move to another TLS endpoint is taken only where this Client's own CA file can vouch for
    // it: under the public roots alone, a Server could move the fleet to any host a public CA
    // ever certified, and keep it there (ADR-0041 clause 5).
    let moves = endpoint != config.endpoint;
    let over_tls = endpoint.starts_with("wss://") || endpoint.starts_with("https://");
    if moves && over_tls && config.ca_file().is_none() {
        return Err(format!(
            "refusing the offered endpoint {endpoint}: a move to another endpoint needs [tls] \
             ca_file, so that only a server certificate from the fleet's own CA is trusted"
        ));
    }
    let authorization = match offered_authorization(settings) {
        Some(offered) => Some(offered.to_string()),
        None => config.authorization_value()?,
    };
    // An offered client certificate is proved the same way the endpoint and the credential are:
    // by connecting with it (ADR-0017). Until that succeeds the one in force stays in force, so a
    // certificate that cannot authenticate costs nothing.
    let candidate_cert = settings
        .certificate
        .as_ref()
        .map(|certificate| certificate.cert.as_slice())
        .filter(|cert| !cert.is_empty());

    let tls = crate::tls::client_tls_for(config, candidate_cert)?;
    let mut candidate = crate::transport::connection_with(config, tls)?;
    candidate.endpoint = endpoint;
    candidate.authorization = authorization;
    opamp::client::connection::probe(&candidate, probe_report).await
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

    /// Clause 6: what is persisted says only what was offered. A telemetry-only offer against a
    /// fresh state directory must not leave behind an empty `opamp` block claiming the Server
    /// offered settings it never sent.
    /// Verifies: ADR-0041
    #[test]
    fn merge_leaves_opamp_absent_when_neither_side_has_one() {
        let merged = merge(None, &telemetry_only(b"t1", "https://x/v1/metrics"));
        assert!(merged.opamp.is_none());
        assert!(merged.own_metrics.is_some());
        assert_eq!(merged.hash, b"t1");
    }

    /// ADR-0025 rule 17: an offer that names any telemetry destination states all three. The
    /// traces endpoint in force is *stopped* by a metrics-only offer, not carried forward — which
    /// is the whole difference between a fleet that can turn a signal off and one that cannot.
    /// Verifies: ADR-0048
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
    /// ADR-0018 independent, and it is the schema's own "not set means unchanged", held at the
    /// level it still holds at.
    /// Verifies: ADR-0048
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
    /// Verifies: ADR-0048
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
    /// Verifies: ADR-0041
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

    /// Verifies: ADR-0041
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
    /// Verifies: ADR-0041
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

    /// Verifies: ADR-0041
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

    /// Verifies: ADR-0041
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

    /// Verifies: ADR-0041
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

    /// An offer's `tls` and `proxy` are not taken: what is stored carries neither, so the
    /// connection keeps the operator's trust, and the report names both rather than claiming the
    /// offer was applied whole.
    /// Verifies: ADR-0041
    #[test]
    fn offered_tls_and_proxy_are_neither_stored_nor_claimed() {
        use opamp::proto::{ProxyConnectionSettings, TlsConnectionSettings};
        let mut offer = offer_with(b"h1", "wss://server.example/v1/opamp", None, 0);
        if let Some(settings) = offer.opamp.as_mut() {
            settings.tls = Some(TlsConnectionSettings {
                insecure_skip_verify: true,
                ca_pem_contents: "-----BEGIN CERTIFICATE-----".to_string(),
                ..Default::default()
            });
            settings.proxy = Some(ProxyConnectionSettings {
                url: "http://proxy.example:3128".to_string(),
                ..Default::default()
            });
        }
        let stored = merge(None, &offer);
        let settings = stored.opamp.as_ref().expect("the OpAMP half");
        assert_eq!(
            settings.destination_endpoint,
            "wss://server.example/v1/opamp"
        );
        assert!(settings.tls.is_none(), "an offered tls was stored");
        assert!(settings.proxy.is_none(), "an offered proxy was stored");

        let mut config = ClientConfig::default();
        apply(&mut config, &stored);
        assert_eq!(config.endpoint, "wss://server.example/v1/opamp");

        let reported = unhonoured(offer.opamp.as_ref().expect("offer")).expect_err("named");
        assert!(reported.contains("tls and proxy"), "{reported}");
    }

    fn endpoint_only(endpoint: &str) -> OpAmpConnectionSettings {
        OpAmpConnectionSettings {
            destination_endpoint: endpoint.to_string(),
            ..Default::default()
        }
    }

    fn never_reported() -> Option<AgentToServer> {
        panic!("a refused endpoint must not get as far as a probe report")
    }

    /// A move to another TLS endpoint is refused without the Client's own CA file: the public
    /// roots would let any publicly certified host take the fleet.
    /// Verifies: ADR-0041
    #[tokio::test]
    async fn an_offered_move_needs_the_clients_own_ca() {
        let config: ClientConfig =
            toml::from_str("endpoint = \"wss://fleet.example/v1/opamp\"").expect("config");
        let error = verify(
            &endpoint_only("wss://elsewhere.example/v1/opamp"),
            &config,
            never_reported,
        )
        .await
        .expect_err("no ca_file");
        assert!(error.contains("ca_file"), "{error}");
    }

    /// An offered plaintext endpoint off the loopback is refused before anything is dialled, on
    /// either transport: the probe report a plain-HTTP exchange would send is never built.
    /// Verifies: ADR-0041
    #[tokio::test]
    async fn verify_refuses_a_plaintext_endpoint_off_loopback_without_connecting() {
        let config = ClientConfig::default();
        for endpoint in ["ws://192.0.2.1:9/v1/opamp", "http://192.0.2.1:9/v1/opamp"] {
            let error = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                verify(&endpoint_only(endpoint), &config, never_reported),
            )
            .await
            .expect("refused without waiting on a connect")
            .expect_err(endpoint);
            assert!(error.starts_with("refusing"), "{endpoint}: {error}");
            assert!(error.contains(endpoint), "{error} names the endpoint");
        }
    }

    /// A host name never counts as the loopback, not even `localhost`: an offered plaintext
    /// endpoint naming one is refused, and the listener behind it is never dialled.
    /// Verifies: ADR-0041
    #[tokio::test]
    async fn verify_refuses_a_plaintext_endpoint_on_a_host_name() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let config = ClientConfig::default();
        for endpoint in [
            format!("ws://localhost:{port}/v1/opamp"),
            format!("http://localhost:{port}/v1/opamp"),
        ] {
            let error = verify(&endpoint_only(&endpoint), &config, never_reported)
                .await
                .expect_err("a plaintext host name was accepted");
            assert!(error.starts_with("refusing"), "{endpoint}: {error}");
            assert!(
                error.contains(endpoint.as_str()),
                "{error} names the endpoint"
            );
        }
        let dialled =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(dialled.is_err(), "the refused endpoint was dialled");
    }

    /// The verification connect presents the client certificate in force: a listener that
    /// requires one admits the probe with it and refuses the same probe without it.
    /// Verifies: ADR-0041
    #[tokio::test]
    async fn verify_presents_the_client_certificate_in_force() {
        use opamp::server::listen::{ClientAuth, Handle, Listener, ServerTls};
        use opamp::tls::Identity;
        use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};

        opamp::tls::install_ring_provider();
        let dir = tempfile::tempdir().expect("tempdir");
        let ca_key = KeyPair::generate().expect("ca key");
        let mut params = CertificateParams::new(vec!["test-ca".to_string()]).expect("params");
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = params.self_signed(&ca_key).expect("ca");
        let issuer = Issuer::from_ca_cert_pem(&ca.pem(), ca_key).expect("issuer");
        let issue = |name: &str| {
            let key = KeyPair::generate().expect("key");
            let cert = CertificateParams::new(vec![name.to_string()])
                .expect("params")
                .signed_by(&key, &issuer)
                .expect("signed");
            (cert.pem(), key.serialize_pem())
        };

        let (server_cert, server_key) = issue("localhost");
        let tls = ServerTls {
            identity: Identity {
                cert_pem: server_cert.into_bytes(),
                key_pem: server_key.into_bytes(),
            },
            client_auth: ClientAuth::Required {
                ca_pem: ca.pem().into_bytes(),
            },
        }
        .rustls_config()
        .expect("server config");
        let router = axum::Router::new().route("/v1/opamp", axum::routing::post(|| async { "" }));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(
            Listener::new(listener, Handle::new())
                .with_tls(tls)
                .serve(router),
        );

        let (client_cert, client_key) = issue("agent");
        let ca_file = dir.path().join("ca.pem");
        let cert_file = dir.path().join("cert.pem");
        let key_file = dir.path().join("key.pem");
        std::fs::write(&ca_file, ca.pem()).expect("ca");
        std::fs::write(&cert_file, client_cert).expect("cert");
        std::fs::write(&key_file, client_key).expect("key");
        let with_identity = |identity: bool| ClientConfig {
            endpoint: format!("https://localhost:{port}/v1/opamp"),
            state_dir: dir.path().join("state"),
            tls: Some(crate::config::TlsConfig {
                ca_file: Some(ca_file.clone()),
                cert_file: identity.then(|| cert_file.clone()),
                key_file: identity.then(|| key_file.clone()),
            }),
            ..ClientConfig::default()
        };
        let report = || Some(AgentToServer::default());
        let keep = OpAmpConnectionSettings::default();

        verify(&keep, &with_identity(true), report)
            .await
            .expect("the certificate in force is presented");
        assert!(
            verify(&keep, &with_identity(false), report).await.is_err(),
            "a listener requiring a certificate admitted a probe without one"
        );
    }
}
