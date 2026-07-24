//! The Server's own configuration file — TOML (ADR-0008).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

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
        toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))
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
}
