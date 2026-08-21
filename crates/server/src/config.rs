//! The Server's own configuration file — TOML (ADR-0008).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::Deserialize;

/// The default OpAMP endpoint port, from the Baseline.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:4320";

/// The default Operator-plane address (ADR-0066): the port above the protocol's, on loopback.
/// Loopback because that plane is open until `[rest.auth]` guards it (ADR-0067) — until then its
/// reachability *is* its protection, so publishing it is a line an operator writes deliberately.
pub const DEFAULT_REST_LISTEN: &str = "127.0.0.1:4321";

/// `server.toml`. Every setting has a default; unknown keys are rejected so a typo fails loudly at
/// startup instead of silently applying a default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address and port the **Agent plane** binds: the OpAMP endpoint and the package download
    /// route the offers point at (ADR-0066, superseding ADR-0005 on this point).
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// The **Operator plane** — REST API, API docs, and the bundled UI — on its own listener
    /// (ADR-0066). Absent means the default, which is loopback.
    #[serde(default)]
    pub rest: RestConfig,
    /// Where Configurations are persisted — one JSON file each (ADR-0012) — so a Server restart
    /// does not lose what the fleet should be running. An empty or missing directory means: no
    /// Configuration to offer yet.
    #[serde(default = "default_config_dir")]
    pub config_dir: PathBuf,
    /// Optional TLS; when present **both** listeners serve HTTPS/WSS, with one certificate and
    /// key (ADR-0007, ADR-0066).
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
    /// that the Client resolves against its own OpAMP endpoint — the Agent plane, which is where
    /// the download is served (ADR-0066); set it when downloads must go through a different host.
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
    /// The total size of all stored package artifacts the REST API keeps before it refuses a new
    /// upload (ADR-0015). Where `max_package_size_bytes` bounds one artifact, this bounds the whole
    /// store — so a caller cannot fill the disk by uploading many artifacts under distinct names.
    /// `0` is refused at load.
    #[serde(default = "default_max_total_package_size")]
    pub max_total_package_bytes: u64,
    /// How long an Agent that declares `ReportsHeartbeat` may be silent before the fleet view calls
    /// it stale (ADR-0038). Ignored when `[connection_offer]` names a heartbeat interval — the
    /// period this Server asked for is a better answer than a default.
    #[serde(default = "default_stale_after_secs")]
    pub stale_after_secs: u64,
    /// The most Agent records the fleet holds at once. A report bearing a new `instance_uid` past
    /// this ceiling is refused `Unavailable`, so a peer minting fresh self-asserted UIDs (ADR-0047)
    /// cannot exhaust memory or disk; existing Agents keep reporting. The real defence against an
    /// anonymous flood is `[auth]` (ADR-0013) — this is the backstop while it is off. `0` is refused
    /// at load: a fleet that can hold no Agent is a misconfiguration, not a limit.
    #[serde(default = "default_max_agents")]
    pub max_agents: usize,
}

/// The `[rest]` section (ADR-0066): the Operator plane's own listener. It is a section rather than
/// a bare key because the plane is what grows next — an authentication decision belongs inside it,
/// not beside it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestConfig {
    /// Address and port the REST API, the API docs, and the bundled UI bind.
    #[serde(default = "default_rest_listen")]
    pub listen: SocketAddr,
    /// Optional Basic authentication over the whole plane (ADR-0067); absent means open, which is
    /// what the loopback default above is there to make tolerable.
    pub auth: Option<RestAuthConfig>,
}

impl Default for RestConfig {
    fn default() -> Self {
        RestConfig {
            listen: default_rest_listen(),
            auth: None,
        }
    }
}

/// The `[rest.auth]` section (ADR-0067): who may reach the Operator plane. Basic only — the
/// audience is a browser and `curl`, and Basic is the one scheme both speak without a login page.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestAuthConfig {
    /// Accepted Basic credentials, `user = "password"`. Several allow a rotation, or an individual
    /// operator's credential to be withdrawn on its own.
    #[serde(default)]
    pub basic_users: BTreeMap<String, String>,
}

impl RestAuthConfig {
    /// The exact `Authorization` header values that authenticate, precomputed so the request path
    /// is one constant-time comparison per candidate.
    pub fn accepted_headers(&self) -> Vec<String> {
        self.basic_users
            .iter()
            .map(|(user, password)| {
                let encoded =
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
                format!("Basic {encoded}")
            })
            .collect()
    }

    /// The `WWW-Authenticate` challenge — what makes a browser ask for the password rather than
    /// show the operator a bare `401` (RFC 7617).
    pub fn challenge(&self) -> String {
        r#"Basic realm="opamp""#.to_string()
    }

    /// A section that authenticates nobody would lock the operator out of their own Server, and an
    /// empty user or password is a half-written credential rather than an intent (ADR-0008).
    fn check(&self) -> Result<(), String> {
        if self.basic_users.is_empty() {
            return Err(
                "a [rest.auth] section needs at least one entry in [rest.auth.basic_users]"
                    .to_string(),
            );
        }
        for (user, password) in &self.basic_users {
            if user.is_empty() || password.is_empty() {
                return Err(format!(
                    "the [rest.auth.basic_users] entry {user:?} needs a name and a password"
                ));
            }
        }
        Ok(())
    }
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

/// The `[telemetry_offer]` section (ADR-0036): where Agents send their own telemetry.
///
/// The endpoints are full OTLP/HTTP URLs *with path*, which is what the Baseline requires of them;
/// this Server does not append `/v1/metrics` for you, because guessing a receiver's routing is how
/// telemetry disappears into a 404 nobody looks at.
///
/// **What this section says, it says about all three signals** (ADR-0089). A signal left out is
/// offered no destination and is *stopped* on an Agent that was reporting it, and an endpoint set
/// to the empty string is an explicit withdrawal — the one way to say "stop all three", since a
/// Server that offers nothing at all is a Server that says nothing at all. Removing the section
/// keeps that second meaning: it withdraws nothing, so a Server without telemetry of its own does
/// not tear down a fleet another Server pointed at a collector.
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
    ///
    /// An endpoint set to the empty string passes both tests deliberately — it is a withdrawal
    /// (ADR-0089), which is a thing to be said rather than a URL to be checked.
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
            if let Some(endpoint) = value.as_ref().filter(|endpoint| !endpoint.is_empty()) {
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
    /// Client authentication stays *optional at the TLS layer* — the same listener also serves the
    /// package download, which a Client fetches presenting no certificate (ADR-0066) — so the
    /// requirement is enforced on the OpAMP route rather than on the socket. A certificate that **is** presented
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

fn default_rest_listen() -> SocketAddr {
    DEFAULT_REST_LISTEN
        .parse()
        .expect("default REST listen address")
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

/// Roomy for a real package set — a handful of packages across a few platforms, each with a
/// rollback copy — while still bounding the store a caller can grow.
fn default_max_total_package_size() -> u64 {
    crate::fleet::DEFAULT_MAX_TOTAL_PACKAGE_SIZE
}

/// Far above any real fleet, so an authenticated deployment never meets it, yet low enough that the
/// in-memory map and its per-Agent disk mirror stay bounded under a flood of self-asserted UIDs.
fn default_max_agents() -> usize {
    crate::fleet::DEFAULT_MAX_AGENTS
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: default_listen(),
            rest: RestConfig::default(),
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
            max_total_package_bytes: default_max_total_package_size(),
            stale_after_secs: default_stale_after_secs(),
            max_agents: default_max_agents(),
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
        if let Some(auth) = &config.rest.auth {
            auth.check()
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        // The two planes are two listeners (ADR-0066). Addresses that collide would surface as the
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
        if config.max_total_package_bytes == 0 {
            return Err(format!(
                "{}: max_total_package_bytes must be greater than zero — it bounds the store, not a \
                 switch",
                path.display()
            ));
        }
        if config.max_agents == 0 {
            return Err(format!(
                "{}: max_agents must be greater than zero — a fleet that can hold no Agent is a \
                 misconfiguration, not a limit",
                path.display()
            ));
        }
        Ok(config)
    }

    /// The configured offers that hand a credential to any Agent that asks, while `[auth]` is unset
    /// so the OpAMP endpoint admits anyone (ADR-0013). The connection-settings offer carries an
    /// `Authorization` value (ADR-0014) and the telemetry offer carries headers that are "typically
    /// an access token" (ADR-0036); with no admission in front of them, a report declaring the
    /// matching capability is answered with those secrets. Names the sections so the operator can
    /// act. Empty when `[auth]` is set or no offer carries a secret — nothing to warn about.
    ///
    /// This is a warning, not a refusal: ADR-0013 keeps the endpoint open by default so a lab runs
    /// with zero configuration, and gating the offers on admission would break that. Surfacing the
    /// exposure is the middle ground.
    pub fn unauthenticated_secret_offers(&self) -> Vec<&'static str> {
        if self.auth.is_some() {
            return Vec::new();
        }
        let mut offers = Vec::new();
        if self.connection_offer.as_ref().is_some_and(|offer| {
            offer.bearer_token.is_some() || offer.username.is_some() || offer.password.is_some()
        }) {
            offers.push("[connection_offer]");
        }
        if self
            .telemetry_offer
            .as_ref()
            .is_some_and(|offer| !offer.headers.is_empty())
        {
            offers.push("[telemetry_offer]");
        }
        offers
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

    /// ADR-0066: the Operator plane is a second listener, and by default it is on loopback — the
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
    /// and it is named as one rather than surfacing as "address already in use" (ADR-0066).
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

    #[test]
    fn the_total_package_store_ceiling_defaults_is_configurable_and_rejects_zero() {
        let cfg: ServerConfig = toml::from_str("").expect("parse");
        assert_eq!(cfg.max_total_package_bytes, 16 * 1024 * 1024 * 1024);
        let tightened: ServerConfig =
            toml::from_str("max_total_package_bytes = 1048576").expect("parse");
        assert_eq!(tightened.max_total_package_bytes, 1_048_576);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        std::fs::write(&path, "max_total_package_bytes = 0\n").expect("write");
        let err = ServerConfig::load(&path).expect_err("zero must fail startup");
        assert!(err.contains("max_total_package_bytes"), "{err}");
    }

    #[test]
    fn the_agent_ceiling_defaults_is_configurable_and_rejects_zero() {
        let cfg: ServerConfig = toml::from_str("").expect("parse");
        assert_eq!(cfg.max_agents, crate::fleet::DEFAULT_MAX_AGENTS);
        let tightened: ServerConfig = toml::from_str("max_agents = 500").expect("parse");
        assert_eq!(tightened.max_agents, 500);

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        std::fs::write(&path, "max_agents = 0\n").expect("write");
        let err = ServerConfig::load(&path).expect_err("zero must fail startup");
        assert!(err.contains("max_agents"), "{err}");
    }

    /// The three shapes `[telemetry_offer]` admits: a destination, a withdrawal, and a mistake.
    /// The withdrawal is the one ADR-0089 adds — an empty endpoint is a thing to say, not a URL to
    /// check — and it must not be waved through for a value that is merely wrong.
    #[test]
    fn an_empty_endpoint_is_a_withdrawal_and_a_wrong_one_is_still_an_error() {
        let section = |body: &str| {
            toml::from_str::<TelemetryOfferConfig>(body)
                .expect("parse")
                .check()
        };

        assert!(section("metrics_endpoint = \"https://otlp.example/v1/metrics\"").is_ok());
        assert!(
            section("metrics_endpoint = \"\"").is_ok(),
            "an empty endpoint withdraws the signal"
        );

        let err = section("metrics_endpoint = \"collector:4318\"")
            .expect_err("a bare host is not an OTLP/HTTP URL");
        assert!(err.contains("metrics_endpoint"), "{err}");

        let err = section("").expect_err("an empty section offers nothing");
        assert!(err.contains("at least one"), "{err}");
    }

    #[test]
    fn a_secret_bearing_offer_without_auth_is_flagged() {
        // A connection offer carrying a credential, no [auth]: flagged.
        let cfg: ServerConfig =
            toml::from_str("[connection_offer]\nbearer_token = \"a-backend-token\"\n")
                .expect("parse");
        assert_eq!(
            cfg.unauthenticated_secret_offers(),
            vec!["[connection_offer]"]
        );

        // Telemetry headers (an access token) with no [auth]: flagged too.
        let cfg: ServerConfig = toml::from_str(
            "[telemetry_offer]\nmetrics_endpoint = \"https://otlp.example/v1/metrics\"\n\
             [telemetry_offer.headers]\nAuthorization = \"Bearer t\"\n",
        )
        .expect("parse");
        assert_eq!(
            cfg.unauthenticated_secret_offers(),
            vec!["[telemetry_offer]"]
        );

        // The same offer with [auth] in front of it: nothing to warn about.
        let cfg: ServerConfig = toml::from_str(
            "[auth]\nbearer_tokens = [\"admit\"]\n\
             [connection_offer]\nbearer_token = \"a-backend-token\"\n",
        )
        .expect("parse");
        assert!(cfg.unauthenticated_secret_offers().is_empty());

        // An offer that carries no secret (just an endpoint move) is not flagged.
        let cfg: ServerConfig =
            toml::from_str("[connection_offer]\nendpoint = \"wss://moved.example/v1/opamp\"\n")
                .expect("parse");
        assert!(cfg.unauthenticated_secret_offers().is_empty());
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

    /// ADR-0067: the Operator plane's own credentials, precomputed into the header values that
    /// authenticate, with the challenge that makes a browser ask rather than give up.
    #[test]
    fn rest_auth_precomputes_the_accepted_headers_and_the_basic_challenge() {
        let cfg: ServerConfig = toml::from_str(
            r#"
            [rest]
            listen = "127.0.0.1:4321"
            [rest.auth.basic_users]
            fleet = "secret"
            "#,
        )
        .expect("parse");
        let auth = cfg.rest.auth.expect("rest auth");
        // base64("fleet:secret")
        assert_eq!(auth.accepted_headers(), vec!["Basic ZmxlZXQ6c2VjcmV0"]);
        assert_eq!(auth.challenge(), r#"Basic realm="opamp""#);
        assert!(auth.check().is_ok());

        // Absent means open — the zero-configuration default this plane still has.
        let open: ServerConfig = toml::from_str("").expect("parse");
        assert!(open.rest.auth.is_none());
    }

    /// A section that authenticates nobody locks the operator out of their own Server, and a
    /// half-written credential is a mistake rather than an intent — both fail at startup.
    #[test]
    fn an_unusable_rest_auth_section_is_rejected() {
        let empty: RestAuthConfig = toml::from_str("").expect("parses; emptiness is semantic");
        assert!(empty.check().is_err());
        let blank: RestAuthConfig = toml::from_str("[basic_users]\nfleet = \"\"").expect("parse");
        assert!(blank.check().is_err());

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        std::fs::write(&path, "[rest.auth]\n").expect("write");
        let err = ServerConfig::load(&path).expect_err("an empty section must fail startup");
        assert!(err.contains("[rest.auth.basic_users]"), "{err}");

        // Bearer is not a scheme this plane has, and a typo fails loudly (ADR-0008, ADR-0067).
        assert!(toml::from_str::<ServerConfig>("[rest.auth]\nbearer_tokens = [\"tok\"]").is_err());
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
