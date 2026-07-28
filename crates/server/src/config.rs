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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain.
    pub cert_file: PathBuf,
    /// PEM private key.
    pub key_file: PathBuf,
}

fn default_listen() -> SocketAddr {
    DEFAULT_LISTEN.parse().expect("default listen address")
}

fn default_config_dir() -> PathBuf {
    PathBuf::from("fleet-configs")
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: default_listen(),
            config_dir: default_config_dir(),
            tls: None,
            auth: None,
            connection_offer: None,
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
