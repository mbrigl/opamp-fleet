//! The Server's own configuration file — TOML (ADR-0008).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::Deserialize;

/// The default OpAMP endpoint port, from the Baseline.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:4320";

/// `server.toml`. Every setting has a default; unknown keys are rejected so a typo fails loudly at
/// startup instead of silently applying a default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address and port the single listener binds — OpAMP, REST API, and UI share it (ADR-0005).
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// Where Configurations are persisted — one JSON file each (ADR-0012) — so a Server restart
    /// does not lose what the fleet should be running. An empty or missing directory means: no
    /// Configuration to offer yet.
    #[serde(default = "default_config_dir")]
    pub config_dir: PathBuf,
    /// Optional TLS; when present the listener serves HTTPS/WSS (ADR-0007).
    pub tls: Option<TlsConfig>,
    /// Optional authentication on the OpAMP endpoint (ADR-0013); absent means open, as before.
    pub auth: Option<AuthConfig>,
    /// Optional connection settings offered to the fleet (ADR-0014); absent means none.
    pub connection_offer: Option<ConnectionOfferConfig>,
    /// Optional certificate authority for signing Agent CSRs (ADR-0035); absent means the Server
    /// issues nothing and does not declare `AcceptsConnectionSettingsRequest`.
    pub client_ca: Option<ClientCaConfig>,
    /// Optional destinations for the Agents' own telemetry (ADR-0036); absent means none is
    /// offered and no Agent reports any.
    pub telemetry_offer: Option<TelemetryOfferConfig>,
    /// Where software packages are persisted — artifact + metadata each (ADR-0015). An empty or
    /// missing directory means: no package to offer, and `OffersPackages` stays undeclared.
    #[serde(default = "default_packages_dir")]
    pub packages_dir: PathBuf,
    /// The absolute base URL the Server advertises for package downloads (ADR-0015), e.g.
    /// `https://fleet.example:4320`. When unset, the Server offers a path-only `download_url`
    /// that the Client resolves against its own OpAMP endpoint — right for the common
    /// single-listener deployment; set it when downloads must go through a different host.
    pub advertised_url: Option<String>,
    /// The largest OpAMP message the Server accepts or sends, on either transport and in either
    /// direction. The Baseline requires the limit, recommends this default, and asks that it be
    /// configurable — a fleet of small status reports can be served with far less.
    #[serde(default = "default_max_message_size")]
    pub max_message_size_bytes: usize,
    /// The largest package artifact the REST API accepts on upload (ADR-0015). Nothing to do with
    /// the OpAMP message limit above: a package is a *program*, routinely hundreds of megabytes,
    /// and it travels over the REST plane, never in an OpAMP message.
    #[serde(default = "default_max_package_size")]
    pub max_package_size_bytes: usize,
    /// How long an Agent that declares `ReportsHeartbeat` may be silent before the fleet view calls
    /// it stale (ADR-0038). Ignored when `[connection_offer]` names a heartbeat interval — the
    /// period this Server asked for is a better answer than a default.
    #[serde(default = "default_stale_after_secs")]
    pub stale_after_secs: u64,
}

/// The `[connection_offer]` section (ADR-0014): what every Agent declaring
/// `AcceptsOpAMPConnectionSettings` is offered — a canonical credential (`bearer_token`, or
/// `username`/`password`, exactly one scheme), a heartbeat interval, an endpoint. Any subset,
/// but never none of them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionOfferConfig {
    pub bearer_token: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Offered heartbeat interval — on plain HTTP the polling interval (the Baseline's MUST).
    pub heartbeat_interval_secs: Option<u64>,
    /// Offered OpAMP endpoint, e.g. for a Server move; `ws(s)://` or `http(s)://`.
    pub endpoint: Option<String>,
}

impl ConnectionOfferConfig {
    /// The offered `Authorization` header value, `None` for a credential-less offer.
    pub fn authorization(&self) -> Result<Option<String>, String> {
        match (&self.bearer_token, &self.username, &self.password) {
            (None, None, None) => Ok(None),
            (Some(token), None, None) => Ok(Some(format!("Bearer {token}"))),
            (None, Some(user), Some(password)) => {
                let encoded =
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
                Ok(Some(format!("Basic {encoded}")))
            }
            (Some(_), _, _) => Err(
                "[connection_offer] must set either bearer_token or username/password, not both"
                    .to_string(),
            ),
            _ => Err("[connection_offer] needs username and password together".to_string()),
        }
    }

    /// Loud validation (ADR-0008): a well-formed credential, at least one offered field, a sane
    /// endpoint — and, unless the offer points at another Server, a credential this Server's own
    /// `[auth]` accepts, so a rotation cannot lock the fleet out.
    fn check(&self, auth: Option<&AuthConfig>) -> Result<(), String> {
        let authorization = self.authorization()?;
        if authorization.is_none()
            && self.heartbeat_interval_secs.is_none()
            && self.endpoint.is_none()
        {
            return Err(
                "a [connection_offer] section needs a credential, heartbeat_interval_secs, or endpoint"
                    .to_string(),
            );
        }
        if let Some(endpoint) = &self.endpoint {
            let scheme = endpoint.split("://").next().unwrap_or("");
            if !matches!(scheme, "ws" | "wss" | "http" | "https") {
                return Err(format!(
                    "connection_offer endpoint {endpoint} must start with ws://, wss://, http:// or https://"
                ));
            }
        }
        if let (Some(offered), Some(auth), None) = (&authorization, auth, self.endpoint.as_ref()) {
            if !auth.accepted_headers().contains(offered) {
                return Err(
                    "the [connection_offer] credential is not in the [auth] accepted set — \
                     this rotation would lock the fleet out"
                        .to_string(),
                );
            }
        }
        Ok(())
    }
}

/// The `[auth]` section (ADR-0013): the credentials the OpAMP endpoint accepts. Any listed
/// credential passes — several valid at once is what makes overlapping rotation possible.
/// REST API and UI are not touched by this; operator-facing auth is a separate decision.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Accepted `Authorization: Bearer <token>` values.
    #[serde(default)]
    pub bearer_tokens: Vec<String>,
    /// Accepted Basic credentials, `user = "password"`.
    #[serde(default)]
    pub basic_users: BTreeMap<String, String>,
}

impl AuthConfig {
    /// The exact `Authorization` header values that authenticate, precomputed so the request
    /// path is one constant-time string comparison per candidate.
    pub fn accepted_headers(&self) -> Vec<String> {
        let bearer = self.bearer_tokens.iter().map(|t| format!("Bearer {t}"));
        let basic = self.basic_users.iter().map(|(user, password)| {
            let encoded =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
            format!("Basic {encoded}")
        });
        bearer.chain(basic).collect()
    }

    /// The `WWW-Authenticate` challenge advertising exactly the configured schemes (RFC 9110).
    pub fn challenge(&self) -> String {
        let mut schemes = Vec::new();
        if !self.basic_users.is_empty() {
            schemes.push(r#"Basic realm="opamp""#);
        }
        if !self.bearer_tokens.is_empty() {
            schemes.push("Bearer");
        }
        schemes.join(", ")
    }

    /// An `[auth]` section without a single credential would lock the endpoint for everyone —
    /// never what an operator meant, so it fails loudly (ADR-0008).
    fn check(&self) -> Result<(), String> {
        if self.bearer_tokens.is_empty() && self.basic_users.is_empty() {
            return Err(
                "an [auth] section needs at least one entry in bearer_tokens or [auth.basic_users]"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// The `[telemetry_offer]` section (ADR-0036): where Agents send their own telemetry. Each signal
/// is independent — offering only metrics leaves traces and logs unconfigured, and an Agent that
/// receives no destination for a signal reports none.
///
/// The endpoints are full OTLP/HTTP URLs *with path*, which is what the Baseline requires of them;
/// this Server does not append `/v1/metrics` for you, because guessing a receiver's routing is how
/// telemetry disappears into a 404 nobody looks at.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryOfferConfig {
    pub metrics_endpoint: Option<String>,
    pub traces_endpoint: Option<String>,
    pub logs_endpoint: Option<String>,
    /// Headers sent with every signal — an access token for the receiving backend, typically.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl TelemetryOfferConfig {
    /// Loud validation (ADR-0008): an empty section offers nothing and is never what an operator
    /// meant, and an endpoint that is not an OTLP/HTTP URL would be refused by every Agent.
    fn check(&self) -> Result<(), String> {
        let endpoints = [
            ("metrics_endpoint", &self.metrics_endpoint),
            ("traces_endpoint", &self.traces_endpoint),
            ("logs_endpoint", &self.logs_endpoint),
        ];
        if endpoints.iter().all(|(_, value)| value.is_none()) {
            return Err(
                "a [telemetry_offer] section needs at least one of metrics_endpoint,                  traces_endpoint, or logs_endpoint"
                    .to_string(),
            );
        }
        for (key, value) in endpoints {
            if let Some(endpoint) = value {
                if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
                    return Err(format!(
                        "[telemetry_offer] {key} must be a full OTLP/HTTP URL with path, e.g.                          https://collector.example:4318/v1/metrics"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain.
    pub cert_file: PathBuf,
    /// PEM private key.
    pub key_file: PathBuf,
    /// Optional PEM bundle of the certificate authorities a **client** certificate must chain to
    /// (ADR-0035). Present turns mutual TLS on for the OpAMP endpoint: every request to
    /// `/v1/opamp` must arrive over a connection bearing a certificate this bundle verifies.
    ///
    /// Client authentication stays *optional at the TLS layer* — the same listener serves the
    /// REST API and the UI (ADR-0005), and a browser presents nothing — so the requirement is
    /// enforced on the OpAMP route rather than on the socket. A certificate that **is** presented
    /// is always verified: rustls refuses a bad one before any route is reached.
    pub client_ca_file: Option<PathBuf>,
}

/// The `[client_ca]` section (ADR-0035): the certificate authority this Server signs Agent CSRs
/// with. Present is what arms the CSR flow — `AcceptsConnectionSettingsRequest` is declared only
/// while it is, the same "declare what is actually armed" rule `[connection_offer]` follows.
///
/// It is deliberately *not* the listener's own certificate and key. The Baseline's own schema warns
/// against storing a CA's private key where the server certificate lives, because compromising the
/// Server would then mint fleet members at will.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCaConfig {
    /// PEM certificate of the issuing CA.
    pub cert_file: PathBuf,
    /// PEM private key of the issuing CA.
    pub key_file: PathBuf,
    /// How long an issued certificate is valid. Short is the point: this project has no revocation
    /// story, so validity plus renewal is what bounds a certificate's reach (ADR-0035).
    #[serde(default = "default_validity_days")]
    pub validity_days: u32,
}

impl ClientCaConfig {
    /// Loud validation (ADR-0008): a CA that cannot sign, or one whose certificates expire before
    /// the Client would renew them, is a configuration error rather than a runtime surprise.
    fn check(&self) -> Result<(), String> {
        if self.validity_days == 0 {
            return Err("[client_ca] validity_days must be greater than zero".to_string());
        }
        for path in [&self.cert_file, &self.key_file] {
            if !path.exists() {
                return Err(format!("[client_ca] {} does not exist", path.display()));
            }
        }
        Ok(())
    }
}

fn default_listen() -> SocketAddr {
    DEFAULT_LISTEN.parse().expect("default listen address")
}

fn default_config_dir() -> PathBuf {
    PathBuf::from("fleet-configs")
}

fn default_packages_dir() -> PathBuf {
    PathBuf::from("fleet-packages")
}

fn default_max_message_size() -> usize {
    opamp::frame::DEFAULT_MAX_MESSAGE_SIZE
}

/// Long enough that a host offline over a holiday still comes back on a valid certificate, short
/// enough that a certificate is not a permanent grant (ADR-0035).
/// Three times the Baseline's own default heartbeat of 30 seconds (ADR-0038): one missed beat is a
/// lost packet, and a fleet view that flickers is one nobody trusts.
fn default_stale_after_secs() -> u64 {
    90
}

fn default_validity_days() -> u32 {
    90
}

/// Roomy enough for the real thing: an `otelcol-contrib` binary is a few hundred megabytes.
fn default_max_package_size() -> usize {
    crate::fleet::DEFAULT_MAX_PACKAGE_SIZE
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: default_listen(),
            config_dir: default_config_dir(),
            tls: None,
            auth: None,
            connection_offer: None,
            client_ca: None,
            telemetry_offer: None,
            packages_dir: default_packages_dir(),
            advertised_url: None,
            max_message_size_bytes: default_max_message_size(),
            max_package_size_bytes: default_max_package_size(),
            stale_after_secs: default_stale_after_secs(),
        }
    }
}

impl ServerConfig {
    /// Loads the file, or the defaults when it does not exist (a fresh checkout runs without any
    /// setup). A file that exists but does not parse is an error — never silently ignored.
    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(ServerConfig::default());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let config: ServerConfig =
            toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        if let Some(auth) = &config.auth {
            auth.check()
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if let Some(offer) = &config.connection_offer {
            offer
                .check(config.auth.as_ref())
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if let Some(client_ca) = &config.client_ca {
            client_ca
                .check()
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        if let Some(telemetry) = &config.telemetry_offer {
            telemetry
                .check()
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        // Mutual TLS needs a TLS listener to happen on: `client_ca_file` lives inside `[tls]`, so
        // this can only be a `[client_ca]` without one — issuing certificates for a channel that
        // will never ask for them (ADR-0035).
        if config.client_ca.is_some() && config.tls.is_none() {
            return Err(format!(
                "{}: [client_ca] issues client certificates, which only a TLS listener can ask \
                 for — add a [tls] section, or remove [client_ca]",
                path.display()
            ));
        }
        // A limit of zero would refuse every message, and the Baseline knows no "unlimited": the
        // limit is mandatory, so a value that cannot carry a message fails startup.
        if config.max_message_size_bytes == 0 {
            return Err(format!(
                "{}: max_message_size_bytes must be greater than zero",
                path.display()
            ));
        }
        if config.stale_after_secs == 0 {
            return Err(format!(
                "{}: stale_after_secs must be greater than zero — a budget of nothing would call \
                 every Agent stale the instant it reported",
                path.display()
            ));
        }
        if config.max_package_size_bytes == 0 {
            return Err(format!(
                "{}: max_package_size_bytes must be greater than zero",
                path.display()
            ));
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_config() {
        let cfg: ServerConfig = toml::from_str(
            r#"
            listen = "127.0.0.1:9999"
            config_dir = "configs"
            [tls]
            cert_file = "cert.pem"
            key_file = "key.pem"
            "#,
        )
        .expect("parse");
        assert_eq!(cfg.listen.port(), 9999);
        assert!(cfg.tls.is_some());
    }

    #[test]
    fn defaults_apply_to_an_empty_file() {
        let cfg: ServerConfig = toml::from_str("").expect("parse");
        assert_eq!(cfg.listen.port(), 4320);
        assert!(cfg.tls.is_none());
    }

    /// The Baseline requires a message size limit, recommends 64 MiB, and asks that it be
    /// configurable; zero is not "unlimited" but a limit that could carry nothing, so it fails.
    #[test]
    fn the_message_size_limit_defaults_to_the_recommended_value_and_is_configurable() {
        let cfg: ServerConfig = toml::from_str("").expect("parse");
        assert_eq!(cfg.max_message_size_bytes, 64 * 1024 * 1024);
        let tightened: ServerConfig =
            toml::from_str("max_message_size_bytes = 65536").expect("parse");
        assert_eq!(tightened.max_message_size_bytes, 65536);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        std::fs::write(&path, "max_message_size_bytes = 0\n").expect("write");
        let err = ServerConfig::load(&path).expect_err("zero must fail startup");
        assert!(err.contains("max_message_size_bytes"), "{err}");
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<ServerConfig>("listne = \"0.0.0.0:1\"").is_err());
    }

    #[test]
    fn auth_precomputes_the_accepted_headers_and_the_challenge() {
        let cfg: ServerConfig = toml::from_str(
            r#"
            [auth]
            bearer_tokens = ["tok"]
            [auth.basic_users]
            fleet = "secret"
            "#,
        )
        .expect("parse");
        let auth = cfg.auth.expect("auth");
        let headers = auth.accepted_headers();
        assert!(headers.contains(&"Bearer tok".to_string()));
        // base64("fleet:secret")
        assert!(headers.contains(&"Basic ZmxlZXQ6c2VjcmV0".to_string()));
        assert_eq!(auth.challenge(), r#"Basic realm="opamp", Bearer"#);
        assert!(auth.check().is_ok());
    }

    #[test]
    fn the_challenge_advertises_only_the_configured_scheme() {
        let bearer_only: AuthConfig = toml::from_str("bearer_tokens = [\"tok\"]").expect("parse");
        assert_eq!(bearer_only.challenge(), "Bearer");
        assert!(bearer_only.check().is_ok());
    }

    #[test]
    fn an_empty_auth_section_is_rejected() {
        let empty: AuthConfig = toml::from_str("").expect("parses; emptiness is semantic");
        assert!(empty.check().is_err());
        // Unknown keys fail loudly, as everywhere (ADR-0008).
        assert!(toml::from_str::<ServerConfig>("[auth]\nbearer_token = \"tok\"").is_err());
    }

    #[test]
    fn a_connection_offer_yields_the_expected_authorization() {
        let bearer: ConnectionOfferConfig =
            toml::from_str("bearer_token = \"tok\"").expect("parse");
        assert_eq!(
            bearer.authorization().expect("value"),
            Some("Bearer tok".to_string())
        );

        let basic: ConnectionOfferConfig =
            toml::from_str("username = \"fleet\"\npassword = \"secret\"").expect("parse");
        assert_eq!(
            basic.authorization().expect("value"),
            Some("Basic ZmxlZXQ6c2VjcmV0".to_string())
        );

        // Heartbeat-only: no credential, still valid.
        let heartbeat_only: ConnectionOfferConfig =
            toml::from_str("heartbeat_interval_secs = 15").expect("parse");
        assert_eq!(heartbeat_only.authorization().expect("value"), None);
    }

    #[test]
    fn a_connection_offer_needs_at_least_one_field() {
        let empty: ConnectionOfferConfig =
            toml::from_str("").expect("parses; emptiness is semantic");
        assert!(empty.check(None).is_err());
    }

    #[test]
    fn a_connection_offer_rejects_a_bad_endpoint_scheme() {
        let bad: ConnectionOfferConfig =
            toml::from_str("endpoint = \"ftp://x/v1/opamp\"").expect("parse");
        assert!(bad.check(None).is_err());
        let good: ConnectionOfferConfig =
            toml::from_str("endpoint = \"wss://x/v1/opamp\"").expect("parse");
        assert!(good.check(None).is_ok());
    }

    #[test]
    fn a_credential_offer_must_be_accepted_by_auth_unless_the_endpoint_moves() {
        let auth: AuthConfig = toml::from_str("bearer_tokens = [\"new\"]").expect("parse");

        // Offering a credential [auth] does not accept would lock the fleet out.
        let stranger: ConnectionOfferConfig =
            toml::from_str("bearer_token = \"other\"").expect("parse");
        assert!(stranger.check(Some(&auth)).is_err());

        // Offering the accepted credential is fine.
        let matching: ConnectionOfferConfig =
            toml::from_str("bearer_token = \"new\"").expect("parse");
        assert!(matching.check(Some(&auth)).is_ok());

        // A move to another Server is exempt — the destination validates its own credential.
        let moved: ConnectionOfferConfig =
            toml::from_str("bearer_token = \"other\"\nendpoint = \"wss://elsewhere/v1/opamp\"")
                .expect("parse");
        assert!(moved.check(Some(&auth)).is_ok());
    }
}
