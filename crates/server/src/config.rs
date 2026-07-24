//! The Server's own configuration file — TOML (ADR-0011).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The default OpAMP endpoint port, from the Baseline.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:4320";

/// The default Operator-plane address (ADR-0012): the port above the protocol's, on loopback.
pub const DEFAULT_REST_LISTEN: &str = "127.0.0.1:4321";

/// `server.toml`. Every setting has a default; unknown keys are rejected so a typo fails loudly at
/// startup instead of silently applying a default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address and port the **Agent plane** binds: the OpAMP endpoint and the package download
    /// route the offers point at (ADR-0012, superseding ADR-0011 on this point).
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// The **Operator plane** — REST API, API docs, and the bundled UI — on its own listener
    /// (ADR-0012). Absent means the default, which is loopback.
    #[serde(default)]
    pub rest: RestConfig,
    /// Optional TLS; when present **both** listeners serve HTTPS/WSS, with one certificate and
    /// key (ADR-0012).
    pub tls: Option<TlsConfig>,
    /// that the Client resolves against its own OpAMP endpoint — the Agent plane, which is where
    /// the download is served (ADR-0012); set it when downloads must go through a different host.
    /// The largest OpAMP message the Server accepts or sends, on either transport and in either
    /// direction. The Baseline requires the limit, recommends this default, and asks that it be
    /// configurable — a fleet of small status reports can be served with far less.
    #[serde(default = "default_max_message_size")]
    pub max_message_size_bytes: usize,
/// The `[rest]` section (ADR-0012): the Operator plane's own listener. It is a section rather than
/// a bare key because the plane is what grows next — an authentication decision belongs inside it,
/// not beside it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestConfig {
    /// Address and port the REST API, the API docs, and the bundled UI bind.
    #[serde(default = "default_rest_listen")]
    pub listen: SocketAddr,
}

impl Default for RestConfig {
    fn default() -> Self {
        RestConfig {
            listen: default_rest_listen(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsConfig {
    /// PEM certificate chain.
    pub cert_file: PathBuf,
    /// PEM private key.
    pub key_file: PathBuf,
    /// Client authentication stays *optional at the TLS layer* — the same listener also serves the
    /// package download, which a Client fetches presenting no certificate (ADR-0012) — so the
    /// requirement is enforced on the OpAMP route rather than on the socket. A certificate that **is** presented
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
}

fn default_listen() -> SocketAddr {
    DEFAULT_LISTEN.parse().expect("default listen address")
}

fn default_rest_listen() -> SocketAddr {
    DEFAULT_REST_LISTEN
        .parse()
        .expect("default REST listen address")
}

}

fn default_max_message_size() -> usize {
    opamp::frame::DEFAULT_MAX_MESSAGE_SIZE
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: default_listen(),
            rest: RestConfig::default(),
            tls: None,
            max_message_size_bytes: default_max_message_size(),
        }
    }
}

/// Whether two listener addresses cannot both be bound: the same port on the same address, or on
/// an address that covers every interface — `0.0.0.0:4320` and `127.0.0.1:4320` are two spellings
/// of one socket as far as the second `bind` is concerned.
fn listeners_collide(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() == b.port() && (a.ip() == b.ip() || a.ip().is_unspecified() || b.ip().is_unspecified())
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
            toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?;
        // The two planes are two listeners (ADR-0012). Addresses that collide would surface as the
        // second bind failing with "address already in use" — a message about sockets for what is
        // really a configuration mistake, so it is refused here, by name.
        if listeners_collide(config.listen, config.rest.listen) {
            return Err(format!(
                "{}: listen ({}) and [rest] listen ({}) must be different addresses — the Agent \
                 plane and the Operator plane are separate listeners",
                path.display(),
                config.listen,
                config.rest.listen
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

    /// ADR-0012: the Operator plane is a second listener, and by default it is on loopback — the
    /// only protection it has while nothing authenticates it.
    #[test]
    fn the_operator_plane_defaults_to_loopback_and_is_configurable() {
        let cfg: ServerConfig = toml::from_str("").expect("parse");
        assert_eq!(cfg.rest.listen.port(), 4321);
        assert!(
            cfg.rest.listen.ip().is_loopback(),
            "the REST API is not published to the network by default"
        );
        let opened: ServerConfig =
            toml::from_str("[rest]\nlisten = \"0.0.0.0:8080\"").expect("parse");
        assert_eq!(opened.rest.listen.to_string(), "0.0.0.0:8080");
    }

    /// Two planes, two sockets: an address that cannot be bound twice is a configuration mistake,
    /// and it is named as one rather than surfacing as "address already in use" (ADR-0012).
    #[test]
    fn two_planes_on_one_address_are_refused() {
        assert!(listeners_collide(
            "0.0.0.0:4320".parse().unwrap(),
            "0.0.0.0:4320".parse().unwrap()
        ));
        assert!(
            listeners_collide(
                "0.0.0.0:4320".parse().unwrap(),
                "127.0.0.1:4320".parse().unwrap()
            ),
            "a listener on every interface covers the loopback one"
        );
        assert!(!listeners_collide(
            "0.0.0.0:4320".parse().unwrap(),
            "127.0.0.1:4321".parse().unwrap()
        ));
        assert!(!listeners_collide(
            "127.0.0.1:4320".parse().unwrap(),
            "192.168.0.1:4320".parse().unwrap()
        ));
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

        let cfg: ServerConfig = toml::from_str("").expect("parse");
        let tightened: ServerConfig =

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        let err = ServerConfig::load(&path).expect_err("zero must fail startup");
        let cfg: ServerConfig = toml::from_str("").expect("parse");

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        let err = ServerConfig::load(&path).expect_err("zero must fail startup");
        let cfg: ServerConfig = toml::from_str(
        let cfg: ServerConfig = toml::from_str(
    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<ServerConfig>("listne = \"0.0.0.0:1\"").is_err());
    }
        let cfg: ServerConfig = toml::from_str(
            r#"
        let cfg: ServerConfig = toml::from_str(
            r#"

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
}
