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
    /// How large the audit record grows and how much of it is kept (ADR-0052).
    #[serde(default)]
    pub audit: AuditConfig,
}

/// The `[audit]` section (ADR-0052 clause 4).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuditConfig {
    /// A file reaching this size is closed and a new one started.
    pub max_file_bytes: u64,
    /// How many files are kept; the oldest past this is deleted.
    pub keep_files: usize,
}

impl Default for AuditConfig {
    fn default() -> Self {
        let limits = crate::audit_log::Limits::default();
        AuditConfig {
            max_file_bytes: limits.max_file_bytes,
            keep_files: limits.keep_files,
        }
    }
}

impl AuditConfig {
    #[must_use]
    pub fn limits(&self) -> crate::audit_log::Limits {
        crate::audit_log::Limits {
            max_file_bytes: self.max_file_bytes,
            keep_files: self.keep_files,
        }
    }
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
    /// Accepted Basic credentials, `user = "<Argon2id hash>"` (ADR-0039 clause 26). Several allow
    /// a rotation, or an individual operator's credential to be withdrawn on its own.
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
    /// The check the Operator plane runs on every request (ADR-0039 clause 2).
    ///
    /// # Errors
    /// Returns an error naming an entry that is not a hash this Server keeps.
    pub fn credentials(&self) -> Result<crate::credentials::Credentials, String> {
        crate::credentials::Credentials::new(&[], &self.basic_users, self.challenge())
            .map_err(|e| format!("[rest.auth.basic_users] {e}"))
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
        self.credentials().map(|_| ())
    }
}

/// The `[connection_offer]` section (ADR-0041): what every Agent declaring
/// `AcceptsOpAMPConnectionSettings` is offered — a canonical credential (`bearer_token_file`, or
/// `username` and `password_file`, exactly one scheme), a heartbeat interval, an endpoint. Any
/// subset, but never none of them. The credential is read from its file, never from this one.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionOfferConfig {
    /// A file holding the offered Bearer token, readable by its owner alone.
    pub bearer_token_file: Option<PathBuf>,
    pub username: Option<String>,
    /// A file holding the offered Basic password, readable by its owner alone.
    pub password_file: Option<PathBuf>,
    /// Refused: the credential is never kept inline (ADR-0041 clause 1).
    pub bearer_token: Option<String>,
    /// Refused, as `bearer_token`.
    pub password: Option<String>,
    /// The offered `Authorization` value, read once: what is checked against `[auth]` is what is
    /// offered.
    #[serde(skip)]
    resolved: std::sync::OnceLock<Result<Option<String>, String>>,
    /// Offered heartbeat interval — on plain HTTP the polling interval (the Baseline's MUST).
    pub heartbeat_interval_secs: Option<u64>,
    /// Offered OpAMP endpoint, e.g. for a Server move; `ws(s)://` or `http(s)://`.
    pub endpoint: Option<String>,
}

impl std::fmt::Debug for ConnectionOfferConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionOfferConfig")
            .field("bearer_token_file", &self.bearer_token_file)
            .field("username", &self.username)
            .field("password_file", &self.password_file)
            .field("bearer_token", &redacted(&self.bearer_token))
            .field("password", &redacted(&self.password))
            .field("heartbeat_interval_secs", &self.heartbeat_interval_secs)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

/// A credential kept in a file of its own (ADR-0041 clause 1): present, not empty, and readable by
/// its owner alone on Unix. Trailing whitespace — the newline an editor adds — is dropped.
fn secret_file(key: &str, path: &Path) -> Result<String, String> {
    use std::io::Read as _;
    let named = |e: std::io::Error| format!("[connection_offer] {key} {}: {e}", path.display());
    // Opened once, and checked and read through that one handle, so the file read is the file
    // checked — not one swapped in between.
    let mut file = std::fs::File::open(path).map_err(named)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let meta = file.metadata().map_err(named)?;
        if meta.mode() & 0o077 != 0 {
            return Err(format!(
                "[connection_offer] {key} {} is readable by others (mode {:o}) — make it 0600",
                path.display(),
                meta.mode() & 0o777
            ));
        }
        // SAFETY: geteuid has no preconditions and touches no memory.
        let me = unsafe { libc::geteuid() };
        if meta.uid() != me {
            return Err(format!(
                "[connection_offer] {key} {} belongs to another account (uid {}) — it must be \
                 this Server's own",
                path.display(),
                meta.uid()
            ));
        }
    }
    let mut text = String::new();
    file.read_to_string(&mut text).map_err(named)?;
    let secret = text.trim_end().to_string();
    if secret.is_empty() {
        return Err(format!(
            "[connection_offer] {key} {} is empty",
            path.display()
        ));
    }
    Ok(secret)
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
    /// The offered `Authorization` header value, read from its file; `None` for a
    /// credential-less offer.
    ///
    /// # Errors
    /// Refuses an inline credential, a scheme half given or both given, and a file that is
    /// missing, empty or readable by anyone but its owner.
    pub fn authorization(&self) -> Result<Option<String>, String> {
        self.resolved
            .get_or_init(|| self.read_authorization())
            .clone()
    }

    fn read_authorization(&self) -> Result<Option<String>, String> {
        for (key, inline) in [
            ("bearer_token", &self.bearer_token),
            ("password", &self.password),
        ] {
            if inline.is_some() {
                return Err(format!(
                    "[connection_offer] {key} is never kept in server.toml — write it to a file \
                     only this Server's account can read and name it with {key}_file"
                ));
            }
        }
        match (&self.bearer_token_file, &self.username, &self.password_file) {
            (None, None, None) => Ok(None),
            (Some(file), None, None) => Ok(Some(format!(
                "Bearer {}",
                secret_file("bearer_token_file", file)?
            ))),
            (None, Some(user), Some(file)) => {
                let password = secret_file("password_file", file)?;
                let encoded =
                    base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
                Ok(Some(format!("Basic {encoded}")))
            }
            (Some(_), _, _) => Err(
                "[connection_offer] must set either bearer_token_file or username/password_file, \
                 not both"
                    .to_string(),
            ),
            _ => Err("[connection_offer] needs username and password_file together".to_string()),
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
            if !auth.credentials()?.verify(offered) {
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
    /// Accepted Bearer tokens, each as `sha256:<hex>` (ADR-0039 clause 26).
    #[serde(default)]
    pub bearer_tokens: Vec<String>,
    /// Accepted Basic credentials, `user = "<Argon2id hash>"`.
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
    /// The check the Agent plane runs on every request (ADR-0039 clause 2).
    ///
    /// # Errors
    /// Returns an error naming an entry that is not a hash this Server keeps.
    pub fn credentials(&self) -> Result<crate::credentials::Credentials, String> {
        crate::credentials::Credentials::new(
            &self.bearer_tokens,
            &self.basic_users,
            self.challenge(),
        )
        .map_err(|e| format!("[auth] {e}"))
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
        self.credentials().map(|_| ())
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

/// A month: short enough that a certificate stolen unnoticed is good for weeks, not a quarter;
/// long enough that a host offline for a fortnight still renews on its own (ADR-0039 clause 9).
fn default_validity_days() -> u32 {
    30
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
            audit: AuditConfig::default(),
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
        if self.audit.max_file_bytes == 0 || self.audit.keep_files == 0 {
            return Err(
                "[audit] max_file_bytes and keep_files must be greater than 0 — the record is \
                 never switched off"
                    .to_string(),
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

    /// The entry `server.toml` keeps for a Bearer token, for any token a test names.
    fn bearer(token: &str) -> String {
        use sha2::Digest as _;
        format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(token.as_bytes()))
        )
    }

    /// The `[auth]` line accepting the token `t`.
    fn auth_t() -> String {
        format!("[auth]\nbearer_tokens = [{:?}]\n", bearer("t"))
    }

    /// A credential file, owner-only, as ADR-0041 asks.
    fn secret_in(dir: &Path, name: &str, value: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("{value}\n")).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        }
        path
    }

    fn offer_with(body: &str) -> ConnectionOfferConfig {
        toml::from_str(body).expect("parse")
    }

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
    fn auth_keeps_hashes_and_advertises_both_schemes() {
        let cfg: ServerConfig = toml::from_str(&format!(
            "[auth]\nbearer_tokens = [{:?}]\n[auth.basic_users]\nfleet = {:?}\n",
            bearer("tok"),
            crate::credentials::hash_basic("secret").expect("hash"),
        ))
        .expect("parse");
        let auth = cfg.auth.expect("auth");
        assert!(auth.check().is_ok());
        let credentials = auth.credentials().expect("credentials");
        assert!(credentials.verify("Bearer tok"));
        // base64("fleet:secret")
        assert!(credentials.verify("Basic ZmxlZXQ6c2VjcmV0"));
        assert!(!credentials.verify("Bearer secret"));
        assert_eq!(auth.challenge(), r#"Basic realm="opamp", Bearer"#);
    }

    /// No credential in `server.toml` authenticates on its own: a token or a password in clear is
    /// refused at startup, the entry named and the value never echoed (ADR-0039 clause 26).
    /// Verifies: ADR-0039
    #[test]
    fn a_plaintext_credential_is_refused_naming_its_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        let tls =
            "[tls]\ncert_file = \"c.pem\"\nkey_file = \"k.pem\"\nclient_ca_file = \"ca.pem\"\n";
        for (body, names) in [
            (
                format!("[auth]\nbearer_tokens = [{:?}, \"plain-token-value\"]\n{tls}", bearer("t")),
                "entry 1",
            ),
            (
                format!("{}[auth.basic_users]\nfleet = \"plain-password\"\n{tls}", auth_t()),
                "user \"fleet\"",
            ),
            (
                format!(
                    "{}[rest]\nlisten = \"0.0.0.0:4321\"\n[rest.auth.basic_users]\nops = \"plain-password\"\n{tls}",
                    auth_t()
                ),
                "[rest.auth.basic_users] user \"ops\"",
            ),
        ] {
            std::fs::write(&path, body).expect("write");
            let err = ServerConfig::load(&path).expect_err("a credential in clear");
            assert!(err.contains(names), "names the entry: {err}");
            assert!(err.contains("hash-credential"), "says how to fix it: {err}");
            assert!(!err.contains("plain-"), "never echoes the value: {err}");
        }
    }

    /// Verifies: ADR-0039
    #[test]
    fn a_weak_argon2id_hash_is_refused() {
        use argon2::password_hash::{PasswordHasher as _, SaltString};
        let cheap = argon2::Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(4096, 1, 1, None).expect("params"),
        )
        .hash_password(
            b"secret",
            &SaltString::encode_b64(b"0123456789abcdef").expect("salt"),
        )
        .expect("hash")
        .to_string();
        let auth: RestAuthConfig =
            toml::from_str(&format!("[basic_users]\nops = {cheap:?}\n")).expect("parse");
        let err = auth.check().expect_err("cheaper than the minimum");
        assert!(err.contains("cheaper"), "{err}");
    }

    #[test]
    fn the_challenge_advertises_only_the_configured_scheme() {
        let bearer_only: AuthConfig =
            toml::from_str(&format!("bearer_tokens = [{:?}]", bearer("tok"))).expect("parse");
        assert_eq!(bearer_only.challenge(), "Bearer");
        assert!(bearer_only.check().is_ok());
    }

    /// The Operator plane's own credentials (ADR-0039), precomputed into the header values that
    /// authenticate, with the challenge that makes a browser ask rather than give up.
    /// Verifies: ADR-0039
    #[test]
    fn rest_auth_verifies_its_hashes_and_carries_the_basic_challenge() {
        let cfg: ServerConfig = toml::from_str(&format!(
            "[rest]\nlisten = \"127.0.0.1:4321\"\n[rest.auth.basic_users]\nfleet = {:?}\n",
            crate::credentials::hash_basic("secret").expect("hash"),
        ))
        .expect("parse");
        let auth = cfg.rest.auth.expect("rest auth");
        // base64("fleet:secret")
        assert!(auth
            .credentials()
            .expect("credentials")
            .verify("Basic ZmxlZXQ6c2VjcmV0"));
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
    fn an_offered_credential_is_read_from_its_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let token = secret_in(dir.path(), "token", "tok");
        let bearer = offer_with(&format!(
            "bearer_token_file = {:?}",
            token.display().to_string()
        ));
        assert_eq!(
            bearer.authorization().expect("value"),
            Some("Bearer tok".to_string()),
            "the trailing newline is dropped"
        );

        let password = secret_in(dir.path(), "password", "secret");
        let basic = offer_with(&format!(
            "username = \"fleet\"\npassword_file = {:?}",
            password.display().to_string()
        ));
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
    fn an_inline_offered_credential_is_refused() {
        for body in [
            "bearer_token = \"tok\"",
            "username = \"fleet\"\npassword = \"secret\"",
        ] {
            let err = offer_with(body).authorization().expect_err(body);
            assert!(err.contains("never kept in server.toml"), "{err}");
            assert!(!err.contains("tok\"") && !err.contains("secret\""), "{err}");
        }
    }

    /// Verifies: ADR-0041
    #[cfg(unix)]
    #[test]
    fn an_offered_credential_file_readable_by_others_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let token = secret_in(dir.path(), "token", "tok");
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        let offer = offer_with(&format!(
            "bearer_token_file = {:?}",
            token.display().to_string()
        ));
        let err = offer.authorization().expect_err("readable by others");
        assert!(
            err.contains("bearer_token_file") && err.contains("0600"),
            "{err}"
        );
        let empty = secret_in(dir.path(), "empty", "");
        let offer = offer_with(&format!(
            "bearer_token_file = {:?}",
            empty.display().to_string()
        ));
        assert!(offer.authorization().expect_err("empty").contains("empty"));
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
        std::fs::write(&path, format!("{}{tls}", auth_t())).expect("write");
        let err = ServerConfig::load(&path).expect_err("no client CA");
        assert!(err.contains("client_ca_file is required"), "{err}");

        let tls = format!("{tls}client_ca_file = \"ca.pem\"\n");
        std::fs::write(&path, &tls).expect("write");
        let err = ServerConfig::load(&path).expect_err("no credential");
        assert!(err.contains("[auth] is required"), "{err}");

        let complete = format!("{}{tls}", auth_t());
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
        let tls = format!(
            "{}[tls]\ncert_file = \"c.pem\"\nkey_file = \"k.pem\"\nclient_ca_file = \"ca.pem\"\n",
            auth_t()
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("server.toml");
        std::fs::write(&path, format!("[rest]\nlisten = \"0.0.0.0:4321\"\n{tls}")).expect("write");
        let err = ServerConfig::load(&path).expect_err("open plane without auth");
        assert!(err.contains("[rest.auth] is required"), "{err}");

        let guarded = format!(
            "[rest]\nlisten = \"0.0.0.0:4321\"\n[rest.auth.basic_users]\nops = {:?}\n",
            crate::credentials::hash_basic("s3cret").expect("hash")
        );
        std::fs::write(&path, format!("{guarded}{tls}")).expect("write");
        ServerConfig::load(&path).expect("guarded plane loads");

        std::fs::write(&path, &tls).expect("write");
        ServerConfig::load(&path).expect("a loopback plane needs no authentication");
    }

    /// Verifies: ADR-0041
    #[test]
    fn a_credential_offer_must_be_accepted_by_auth_unless_the_endpoint_moves() {
        let auth: AuthConfig =
            toml::from_str(&format!("bearer_tokens = [{:?}]", bearer("new"))).expect("parse");
        let dir = tempfile::tempdir().expect("tempdir");
        let new = secret_in(dir.path(), "new", "new").display().to_string();
        let other = secret_in(dir.path(), "other", "other")
            .display()
            .to_string();

        // Offering a credential [auth] does not accept would lock the fleet out.
        let stranger = offer_with(&format!("bearer_token_file = {other:?}"));
        assert!(stranger.check(Some(&auth)).is_err());

        // Offering the accepted credential is fine.
        let matching = offer_with(&format!("bearer_token_file = {new:?}"));
        assert!(matching.check(Some(&auth)).is_ok());

        // A move to another Server is exempt — the destination validates its own credential.
        let moved = offer_with(&format!(
            "bearer_token_file = {other:?}\nendpoint = \"wss://elsewhere/v1/opamp\""
        ));
        assert!(moved.check(Some(&auth)).is_ok());
    }
}
