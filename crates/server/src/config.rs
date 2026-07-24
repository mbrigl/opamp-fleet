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
}
