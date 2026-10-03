//! The Server's own configuration file — TOML (ADR-0011).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use opamp::proto::{Header, Headers, OpAmpConnectionSettings, TelemetryConnectionSettings};
use serde::Deserialize;

use crate::fleet::{ConnectionOffer, TelemetryOffer};

/// The default Agent-plane address: the Baseline's port, on the loopback (ADR-0038). Serving the
/// estate is a line an operator writes deliberately, together with the TLS it needs.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:4320";

/// The default Operator-plane address (ADR-0012): the port above the protocol's, on loopback.
/// Loopback because that plane is open until `[rest.auth]` guards it (ADR-0017) — until then its
/// reachability *is* its protection, so publishing it is a line an operator writes deliberately.
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
    /// Where Configurations are persisted — one JSON file each (ADR-0016) — so a Server restart
    /// does not lose what the fleet should be running. An empty or missing directory means: no
    /// Configuration to offer yet.
    #[serde(default = "default_config_dir")]
    pub config_dir: PathBuf,
    /// TLS for both listeners, with one certificate and key (ADR-0038). Required: a Server without
    /// it is refused at startup. It is an `Option` only so that its absence can be named.
    pub tls: Option<TlsConfig>,
    /// The fleet credential every request to the OpAMP endpoint must carry (ADR-0039). Required: a
    /// Server without it is refused at startup. An `Option` only so its absence can be named.
    pub auth: Option<AuthConfig>,
    /// Optional connection settings offered to the fleet (ADR-0041); absent means none.
    pub connection_offer: Option<ConnectionOfferConfig>,
    /// Optional certificate authority for signing Agent CSRs (ADR-0039); absent means the Server
    /// issues nothing and does not declare `AcceptsConnectionSettingsRequest`.
    pub client_ca: Option<ClientCaConfig>,
    /// Optional destinations for the Agents' own telemetry (ADR-0048); absent means none is
    /// offered and no Agent reports any.
    pub telemetry_offer: Option<TelemetryOfferConfig>,
    /// Where software packages are persisted — artifact + metadata each (ADR-0019). An empty or
    /// missing directory means: no package to offer, and `OffersPackages` stays undeclared.
    #[serde(default = "default_packages_dir")]
    pub packages_dir: PathBuf,
    /// The absolute base URL the Server advertises for package downloads (ADR-0019), e.g.
    /// `https://fleet.example:4320`. When unset, the Server offers a path-only `download_url`
    /// that the Client resolves against its own OpAMP endpoint — the Agent plane, which is where
    /// the download is served (ADR-0012); set it when downloads must go through a different host.
    pub advertised_url: Option<String>,
    /// The largest OpAMP message the Server accepts or sends, on either transport and in either
    /// direction. The Baseline requires the limit, recommends this default, and asks that it be
    /// configurable — a fleet of small status reports can be served with far less.
    #[serde(default = "default_max_message_size")]
    pub max_message_size_bytes: usize,
    /// The largest package artifact the REST API accepts on upload (ADR-0019). Nothing to do with
    /// the OpAMP message limit above: a package is a *program*, routinely hundreds of megabytes,
    /// and it travels over the REST plane, never in an OpAMP message.
    #[serde(default = "default_max_package_size")]
    pub max_package_size_bytes: usize,
    /// The total size of all stored package artifacts the REST API keeps before it refuses a new
    /// upload (ADR-0019). Where `max_package_size_bytes` bounds one artifact, this bounds the whole
    /// store — so a caller cannot fill the disk by uploading many artifacts under distinct names.
    /// `0` is refused at load.
    #[serde(default = "default_max_total_package_size")]
    pub max_total_package_bytes: u64,
    /// How long an Agent that declares `ReportsHeartbeat` may be silent before the fleet view calls
    /// it stale (ADR-0026). Ignored when `[connection_offer]` names a heartbeat interval — the
    /// period this Server asked for is a better answer than a default.
    #[serde(default = "default_stale_after_secs")]
    pub stale_after_secs: u64,
    /// The most Agent records the fleet holds at once. A report bearing a new `instance_uid` past
    /// this ceiling is refused `Unavailable`, so a peer minting fresh self-asserted UIDs (ADR-0017)
    /// cannot exhaust memory or disk; existing Agents keep reporting. The real defence against an
    /// anonymous flood is `[auth]` (ADR-0017) — this is the backstop while it is off. `0` is refused
    /// at load: a fleet that can hold no Agent is a misconfiguration, not a limit.
    #[serde(default = "default_max_agents")]
    pub max_agents: usize,
    /// The connections the Agent plane holds at once (ADR-0038). Past it a connection is closed on
    /// accept; the ones held keep working. A fleet larger than the default raises this together
    /// with the process's file-descriptor limit. `0` is refused at load.
    #[serde(default = "default_agent_max_connections")]
    pub max_connections: usize,
    /// The bootstrap CA a host enrols with (ADR-0039 clause 19); absent means no host enrols, and
    /// an operator provisions every certificate.
    pub enrolment: Option<EnrolmentConfig>,
    /// How repeated admission failures from one peer address are throttled (ADR-0039 clause 24).
    #[serde(default)]
    pub admission_throttle: AdmissionThrottleConfig,
}

/// The `[enrolment]` section (ADR-0039 clause 19).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrolmentConfig {
    /// PEM certificate of the CA that issues bootstrap certificates. A certificate from it can
    /// only enrol: it needs the credential beside it, an open enrolment window and an operator's
    /// approval.
    pub bootstrap_ca_file: PathBuf,
}

/// The `[admission_throttle]` section (ADR-0039 clause 24).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdmissionThrottleConfig {
    /// Failures within `window_secs` that put a peer address in back-off.
    pub max_failures: u32,
    pub window_secs: u64,
    /// How long a peer address in back-off is answered `429`.
    pub backoff_secs: u64,
}

impl Default for AdmissionThrottleConfig {
    fn default() -> Self {
        let limits = crate::throttle::Limits::default();
        AdmissionThrottleConfig {
            max_failures: limits.max_failures,
            window_secs: limits.window_secs,
            backoff_secs: limits.backoff_secs,
        }
    }
}

impl AdmissionThrottleConfig {
    /// The limits the throttle counts by.
    #[must_use]
    pub fn limits(&self) -> crate::throttle::Limits {
        crate::throttle::Limits {
            max_failures: self.max_failures,
            window_secs: self.window_secs,
            backoff_secs: self.backoff_secs,
        }
    }
}

/// The `[rest]` section (ADR-0012): the Operator plane's own listener. It is a section rather than
/// a bare key because the plane is what grows next — an authentication decision belongs inside it,
/// not beside it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestConfig {
    /// Address and port the REST API, the API docs, and the bundled UI bind.
    #[serde(default = "default_rest_listen")]
    pub listen: SocketAddr,
    /// Optional Basic authentication over the whole plane (ADR-0039); required off the loopback.
    pub auth: Option<RestAuthConfig>,
    /// The connections the Operator plane holds at once (ADR-0038). `0` is refused at load.
    #[serde(default = "default_rest_max_connections")]
    pub max_connections: usize,
}

impl Default for RestConfig {
    fn default() -> Self {
        RestConfig {
            listen: default_rest_listen(),
            auth: None,
            max_connections: default_rest_max_connections(),
        }
    }
}

/// The `[rest.auth]` section (ADR-0017): who may reach the Operator plane. Basic only — the
/// audience is a browser and `curl`, and Basic is the one scheme both speak without a login page.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestAuthConfig {
    /// Accepted Basic credentials, `user = "password"`. Several allow a rotation, or an individual
    /// operator's credential to be withdrawn on its own.
    #[serde(default)]
    pub basic_users: BTreeMap<String, String>,
}

impl std::fmt::Debug for RestAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestAuthConfig")
            .field("basic_users", &self.basic_users.keys().collect::<Vec<_>>())
            .finish()
    }
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
    /// empty user or password is a half-written credential rather than an intent (ADR-0011).
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

/// The `[connection_offer]` section (ADR-0018): what every Agent declaring
/// `AcceptsOpAMPConnectionSettings` is offered — a canonical credential (`bearer_token`, or
/// `username`/`password`, exactly one scheme), a heartbeat interval, an endpoint. Any subset,
/// but never none of them.
#[derive(Deserialize)]
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

impl std::fmt::Debug for ConnectionOfferConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionOfferConfig")
            .field("bearer_token", &redacted(&self.bearer_token))
            .field("username", &self.username)
            .field("password", &redacted(&self.password))
            .field("heartbeat_interval_secs", &self.heartbeat_interval_secs)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// Names what is configured, never the secret itself.
fn redacted<T>(value: &Option<T>) -> &'static str {
    if value.is_some() {
        "<redacted>"
    } else {
        "None"
    }
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

    /// Loud validation (ADR-0011): a well-formed credential, at least one offered field, a sane
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
        // The Server never offers a fleet a plaintext path off the host (ADR-0041).
        if let Some(endpoint) = &self.endpoint {
            opamp::endpoint::check_url(endpoint)
                .map_err(|e| format!("[connection_offer] endpoint {e}"))?;
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

/// The `[auth]` section (ADR-0017): the credentials the OpAMP endpoint accepts. Any listed
/// credential passes — several valid at once is what makes overlapping rotation possible.
/// REST API and UI are not touched by this; operator-facing auth is a separate decision.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// Accepted `Authorization: Bearer <token>` values.
    #[serde(default)]
    pub bearer_tokens: Vec<String>,
    /// Accepted Basic credentials, `user = "password"`.
    #[serde(default)]
    pub basic_users: BTreeMap<String, String>,
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("bearer_tokens", &self.bearer_tokens.len())
            .field("basic_users", &self.basic_users.keys().collect::<Vec<_>>())
            .finish()
    }
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
    /// never what an operator meant, so it fails loudly (ADR-0011).
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

/// The `[telemetry_offer]` section (ADR-0025): where Agents send their own telemetry.
///
/// The endpoints are full OTLP/HTTP URLs *with path*, which is what the Baseline requires of them;
/// this Server does not append `/v1/metrics` for you, because guessing a receiver's routing is how
/// telemetry disappears into a 404 nobody looks at.
///
/// **What this section says, it says about all three signals** (ADR-0025). A signal left out is
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
    /// Loud validation (ADR-0011): an empty section offers nothing and is never what an operator
    /// meant, and an endpoint that is not an OTLP/HTTP URL would be refused by every Agent.
    ///
    /// An endpoint set to the empty string passes both tests deliberately — it is a withdrawal
    /// (ADR-0025), which is a thing to be said rather than a URL to be checked.
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
    /// PEM bundle of the certificate authorities a **client** certificate must chain to, required
    /// (ADR-0039). The Agent plane asks for the certificate in the TLS handshake, so a peer without
    /// one, or with one this bundle (or the bootstrap CA of `[enrolment]`) does not verify, never
    /// reaches a route — the package download included. An `Option` only so its absence can be
    /// named at startup.
    pub client_ca_file: Option<PathBuf>,
}

/// The `[client_ca]` section (ADR-0017): the certificate authority this Server signs Agent CSRs
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
    /// story, so validity plus renewal is what bounds a certificate's reach (ADR-0017).
    #[serde(default = "default_validity_days")]
    pub validity_days: u32,
}

impl ClientCaConfig {
    /// Loud validation (ADR-0011): a CA that cannot sign, or one whose certificates expire before
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

fn default_agent_max_connections() -> usize {
    opamp::server::listen::DEFAULT_MAX_CONNECTIONS
}

fn default_rest_max_connections() -> usize {
    256
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
/// enough that a certificate is not a permanent grant (ADR-0017).
/// Three times the Baseline's own default heartbeat of 30 seconds (ADR-0026): one missed beat is a
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
            max_connections: default_agent_max_connections(),
            enrolment: None,
            admission_throttle: AdmissionThrottleConfig::default(),
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
        // A missing file is the defaults, held to the same rules: they serve no TLS, so a Server
        // without a configuration is refused below, naming what it needs (ADR-0038).
        let config: ServerConfig = if path.exists() {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))?
        } else {
            ServerConfig::default()
        };
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
        // Mutual TLS needs a TLS listener to happen on: `client_ca_file` lives inside `[tls]`, so
        // this can only be a `[client_ca]` without one — issuing certificates for a channel that
        // will never ask for them (ADR-0017).
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
        if config.max_connections == 0 || config.rest.max_connections == 0 {
            return Err(format!(
                "{}: max_connections and [rest] max_connections must be greater than zero — a \
                 plane that holds no connection serves nobody",
                path.display()
            ));
        }
        config
            .check_secure()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(config)
    }

    /// The transport rules of the specification's Q-1 (ADR-0038): the Agent plane always serves
    /// TLS, and the Operator plane off the loopback requires authentication.
    fn check_secure(&self) -> Result<(), String> {
        let Some(tls) = &self.tls else {
            return Err(
                "[tls] is required — the Agent plane always serves TLS 1.3; set cert_file and \
                 key_file (scripts/dev-pki.sh makes a development set)"
                    .to_string(),
            );
        };
        // Both proofs, always (ADR-0039 clauses 1, 5, 6).
        if tls.client_ca_file.is_none() {
            return Err(
                "[tls] client_ca_file is required — every Agent presents a client certificate in \
                 the handshake"
                    .to_string(),
            );
        }
        if self.auth.is_none() {
            return Err(
                "[auth] is required — every request to the OpAMP endpoint carries the fleet \
                 credential"
                    .to_string(),
            );
        }
        if self.enrolment.is_some() && self.client_ca.is_none() {
            return Err(
                "[enrolment] needs [client_ca]: an approved request is signed by it".to_string(),
            );
        }
        let throttle = &self.admission_throttle;
        if throttle.max_failures == 0 || throttle.window_secs == 0 || throttle.backoff_secs == 0 {
            return Err(
                "[admission_throttle] max_failures, window_secs and backoff_secs must be greater \
                 than zero"
                    .to_string(),
            );
        }
        if !opamp::endpoint::is_loopback_literal(&self.rest.listen.ip().to_string())
            && self.rest.auth.is_none()
        {
            return Err(format!(
                "[rest] listen {} is not the loopback, so [rest.auth] is required — the Operator \
                 plane is the fleet's whole control surface",
                self.rest.listen
            ));
        }
        Ok(())
    }
}

/// The `[connection_offer]` section as the offer the fleet makes (ADR-0018).
impl ConnectionOffer {
    /// Compiles the section into the offer.
    ///
    /// # Errors
    /// Returns an error when the section's credential cannot be read.
    pub fn from_config(config: &ConnectionOfferConfig) -> Result<Self, String> {
        let settings = OpAmpConnectionSettings {
            destination_endpoint: config.endpoint.clone().unwrap_or_default(),
            headers: config.authorization()?.map(|value| Headers {
                headers: vec![Header {
                    key: "Authorization".to_string(),
                    value,
                }],
            }),
            heartbeat_interval_seconds: config.heartbeat_interval_secs.unwrap_or(0),
            ..Default::default()
        };
        Ok(ConnectionOffer::new(settings))
    }
}

/// The `[telemetry_offer]` section as the destinations the fleet offers (ADR-0025).
impl TelemetryOffer {
    pub fn from_config(config: &TelemetryOfferConfig) -> Self {
        let headers = (!config.headers.is_empty()).then(|| Headers {
            headers: config
                .headers
                .iter()
                .map(|(key, value)| Header {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
        });
        let destination = |endpoint: &Option<String>| {
            endpoint
                .as_ref()
                .map(|endpoint| TelemetryConnectionSettings {
                    destination_endpoint: endpoint.clone(),
                    // A withdrawal names nothing else (ADR-0025): an empty endpoint stops that
                    // signal, and the backend's credential travelling with it would be a token
                    // handed out for a connection nobody is going to open.
                    headers: headers.clone().filter(|_| !endpoint.is_empty()),
                    ..Default::default()
                })
        };
        TelemetryOffer {
            own_metrics: destination(&config.metrics_endpoint),
            own_traces: destination(&config.traces_endpoint),
            own_logs: destination(&config.logs_endpoint),
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

    /// ADR-0012: the Operator plane is a second listener, and by default it is on loopback — the
    /// only protection it has while nothing authenticates it.
    /// Verifies: ADR-0038
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
    /// Verifies: ADR-0038
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
    /// The withdrawal is the one ADR-0025 adds — an empty endpoint is a thing to say, not a URL to
    /// check — and it must not be waved through for a value that is merely wrong.
    /// Verifies: ADR-0048
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

    /// Verifies: ADR-0037
    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<ServerConfig>("listne = \"0.0.0.0:1\"").is_err());
    }

    /// Verifies: ADR-0039
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

    /// The Operator plane's own credentials (ADR-0039), precomputed into the header values that
    /// authenticate, with the challenge that makes a browser ask rather than give up.
    /// Verifies: ADR-0039
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
    /// Verifies: ADR-0039
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

        // Bearer is not a scheme this plane has, and a typo fails loudly (ADR-0011, ADR-0017).
        assert!(toml::from_str::<ServerConfig>("[rest.auth]\nbearer_tokens = [\"tok\"]").is_err());
    }

    /// Verifies: ADR-0039
    #[test]
    fn an_empty_auth_section_is_rejected() {
        let empty: AuthConfig = toml::from_str("").expect("parses; emptiness is semantic");
        assert!(empty.check().is_err());
        // Unknown keys fail loudly, as everywhere (ADR-0011).
        assert!(toml::from_str::<ServerConfig>("[auth]\nbearer_token = \"tok\"").is_err());
    }

    /// Verifies: ADR-0041
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

    /// Verifies: ADR-0041
    #[test]
    fn a_connection_offer_needs_at_least_one_field() {
        let empty: ConnectionOfferConfig =
            toml::from_str("").expect("parses; emptiness is semantic");
        assert!(empty.check(None).is_err());
    }

    /// Verifies: ADR-0041
    #[test]
    fn a_connection_offer_rejects_a_bad_endpoint_scheme() {
        let bad: ConnectionOfferConfig =
            toml::from_str("endpoint = \"ftp://x/v1/opamp\"").expect("parse");
        assert!(bad.check(None).is_err());
        let good: ConnectionOfferConfig =
            toml::from_str("endpoint = \"wss://x/v1/opamp\"").expect("parse");
        assert!(good.check(None).is_ok());
    }

    /// Verifies: ADR-0041
    #[test]
    fn a_connection_offer_refuses_a_plaintext_endpoint_off_loopback_naming_the_setting() {
        for endpoint in [
            "ws://fleet.example/v1/opamp",
            "http://10.0.0.1:4320/v1/opamp",
        ] {
            let offer: ConnectionOfferConfig =
                toml::from_str(&format!("endpoint = \"{endpoint}\"")).expect("parse");
            let err = offer.check(None).expect_err(endpoint);
            assert!(err.contains("[connection_offer] endpoint"), "{err}");
        }
    }

    /// Verifies: ADR-0041
    #[test]
    fn a_connection_offer_accepts_a_plaintext_endpoint_on_a_loopback_ip_literal() {
        for endpoint in ["ws://127.0.0.1:4320/v1/opamp", "http://[::1]:4320/v1/opamp"] {
            let offer: ConnectionOfferConfig =
                toml::from_str(&format!("endpoint = \"{endpoint}\"")).expect("parse");
            assert_eq!(offer.check(None), Ok(()), "{endpoint}");
        }
    }

    /// Verifies: ADR-0041
    #[test]
    fn a_connection_offer_refuses_a_plaintext_endpoint_on_localhost() {
        let offer: ConnectionOfferConfig =
            toml::from_str("endpoint = \"ws://localhost:4320/v1/opamp\"").expect("parse");
        assert!(offer.check(None).is_err());
    }

    /// Both proofs, always: a Server without the fleet credential or without the client CA does
    /// not start, and neither does an `[enrolment]` without a CA to sign what it approves.
    /// Verifies: ADR-0039, Q-1
    #[test]
    fn the_credential_and_the_client_ca_are_required_at_startup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        let tls = "[tls]\ncert_file = \"c.pem\"\nkey_file = \"k.pem\"\n";
        std::fs::write(&path, format!("[auth]\nbearer_tokens = [\"t\"]\n{tls}")).expect("write");
        let err = ServerConfig::load(&path).expect_err("no client CA");
        assert!(err.contains("client_ca_file is required"), "{err}");

        let tls = format!("{tls}client_ca_file = \"ca.pem\"\n");
        std::fs::write(&path, &tls).expect("write");
        let err = ServerConfig::load(&path).expect_err("no credential");
        assert!(err.contains("[auth] is required"), "{err}");

        let complete = format!("[auth]\nbearer_tokens = [\"t\"]\n{tls}");
        std::fs::write(&path, &complete).expect("write");
        ServerConfig::load(&path).expect("both proofs configured");

        std::fs::write(
            &path,
            format!("{complete}[enrolment]\nbootstrap_ca_file = \"boot.pem\"\n"),
        )
        .expect("write");
        let err = ServerConfig::load(&path).expect_err("enrolment without a CA");
        assert!(err.contains("[enrolment] needs [client_ca]"), "{err}");
    }

    /// Verifies: ADR-0038
    #[test]
    fn max_connections_defaults_per_plane_and_zero_is_refused() {
        let cfg: ServerConfig = toml::from_str("").expect("parse");
        assert_eq!(cfg.max_connections, 10_000);
        assert_eq!(cfg.rest.max_connections, 256);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        for body in ["max_connections = 0\n", "[rest]\nmax_connections = 0\n"] {
            std::fs::write(&path, body).expect("write");
            let err = ServerConfig::load(&path).expect_err("zero must fail startup");
            assert!(err.contains("max_connections"), "{err}");
        }
    }

    /// Verifies: ADR-0038
    #[test]
    fn the_agent_plane_defaults_to_the_loopback() {
        let cfg: ServerConfig = toml::from_str("").expect("parse");
        assert_eq!(cfg.listen, "127.0.0.1:4320".parse().expect("address"));
    }

    /// Verifies: ADR-0038, ADR-0039, Q-1
    #[test]
    fn a_server_without_tls_is_refused_at_startup() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No file at all is the defaults, and the defaults serve no TLS.
        let missing = ServerConfig::load(&dir.path().join("absent.toml")).expect_err("no tls");
        assert!(missing.contains("[tls] is required"), "{missing}");
        let path = dir.path().join("server.toml");
        std::fs::write(&path, "listen = \"127.0.0.1:4320\"\n").expect("write");
        let err = ServerConfig::load(&path).expect_err("no tls");
        assert!(err.contains("[tls] is required"), "{err}");
    }

    /// Verifies: ADR-0038, ADR-0039
    #[test]
    fn the_operator_plane_requires_authentication_off_the_loopback() {
        let tls = "[auth]\nbearer_tokens = [\"t\"]\n\
                   [tls]\ncert_file = \"c.pem\"\nkey_file = \"k.pem\"\nclient_ca_file = \"ca.pem\"\n";
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        std::fs::write(&path, format!("[rest]\nlisten = \"0.0.0.0:4321\"\n{tls}")).expect("write");
        let err = ServerConfig::load(&path).expect_err("open plane without auth");
        assert!(err.contains("[rest.auth] is required"), "{err}");

        let guarded =
            "[rest]\nlisten = \"0.0.0.0:4321\"\n[rest.auth.basic_users]\nops = \"s3cret\"\n";
        std::fs::write(&path, format!("{guarded}{tls}")).expect("write");
        ServerConfig::load(&path).expect("guarded plane loads");

        std::fs::write(&path, tls).expect("write");
        ServerConfig::load(&path).expect("a loopback plane needs no authentication");
    }

    /// Verifies: ADR-0041
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
