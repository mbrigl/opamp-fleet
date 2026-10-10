//! The fleet's certificates, made by the Server binary itself (ADR-0029).
//!
//! `server pki` makes the three certificate authorities — the server CA, the client CA and the
//! bootstrap CA — the Server's or a Gateway's certificate, and the bootstrap certificate a new host
//! enrols with, and says when each of them ends. It never makes a host certificate: a host obtains
//! that through enrolment alone (ADR-0022). The running Server reads the same ends and warns before
//! a certificate it depends on ends.

use std::path::{Path, PathBuf};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use time::{Duration, OffsetDateTime};
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::config::ServerConfig;

/// How long a CA lives unless told otherwise (ADR-0029 clause 9).
pub const CA_DAYS: u32 = 3650;
/// How long a server certificate lives unless told otherwise: within every browser's limit.
pub const SERVER_DAYS: u32 = 397;
/// How long a bootstrap certificate lives unless told otherwise: short, it is shared by every new
/// host.
pub const BOOTSTRAP_DAYS: u32 = 30;
/// How far ahead an end is announced (ADR-0029 clauses 11 and 12) — or, for a certificate whose
/// whole life is shorter than three times this, the last third of its life.
pub const WARN_DAYS: i64 = 30;
/// The fleet's name in the CAs' subjects unless told otherwise.
pub const DEFAULT_FLEET: &str = "opamp-fleet";

/// How far a certificate's life starts before the moment it is signed, as the client CA's do.
const CLOCK_SKEW: Duration = Duration::minutes(5);

/// What `server pki init` is asked for.
pub struct InitOptions {
    pub server_dir: PathBuf,
    pub offline_dir: PathBuf,
    /// Where the Server host's files will live; the fragment names them there.
    pub server_path: Option<PathBuf>,
    /// Where a host's files will live; the fragment names them there.
    pub host_path: Option<PathBuf>,
    pub fleet: String,
    pub names: Vec<String>,
    pub ca_days: u32,
    pub server_days: u32,
    pub bootstrap_days: u32,
}

/// One file a command writes: its name in the target directory, its contents, and whether it is a
/// private key.
struct File {
    name: &'static str,
    contents: String,
    key: bool,
}

/// A directory and the files a command writes into it.
struct Target {
    dir: PathBuf,
    files: Vec<File>,
}

/// A CA ready to sign: what signs, and when it ends.
struct Authority {
    issuer: Issuer<'static, KeyPair>,
    end: OffsetDateTime,
    common_name: String,
}

/// A certificate a command signed.
struct Made {
    cert_pem: String,
    key_pem: String,
    serial: String,
    /// The life asked for reached past the CA's end and was cut to it.
    cut: bool,
}

/// `server pki init` (ADR-0029 clause 3): the three CAs, the server certificate and the bootstrap
/// certificate, split into what the Server host needs and what stays offline. Returns the lines
/// to print.
///
/// # Errors
/// Returns the message to print when an option is invalid, a file exists, or writing fails.
pub fn init(options: &InitOptions) -> Result<Vec<String>, String> {
    check_fleet(&options.fleet)?;
    let names = names(&options.names)?;
    for days in [options.ca_days, options.server_days, options.bootstrap_days] {
        check_days(days)?;
    }
    let fleet = &options.fleet;
    let (server_ca_pem, server_ca_key, server_ca) =
        authority(&format!("{fleet} server CA"), options.ca_days)?;
    let (client_ca_pem, client_ca_key, _) =
        authority(&format!("{fleet} client CA"), options.ca_days)?;
    let (bootstrap_ca_pem, bootstrap_ca_key, bootstrap_ca) =
        authority(&format!("{fleet} bootstrap CA"), options.ca_days)?;
    let server = server_certificate(&server_ca, &names, options.server_days)?;
    let bootstrap = bootstrap_certificate(&bootstrap_ca, options.bootstrap_days)?;

    let server_path = destination(&options.server_dir, options.server_path.as_deref())?;
    let host_path = destination(&options.offline_dir, options.host_path.as_deref())?;
    let server_toml = format!(
        "[tls]\ncert_file = {}\nkey_file = {}\nclient_ca_file = {}\n\n\
         [client_ca]\ncert_file = {}\nkey_file = {}\n\n\
         [enrolment]\nbootstrap_ca_file = {}\n",
        quoted(&server_path, "server.pem"),
        quoted(&server_path, "server-key.pem"),
        quoted(&server_path, "client-ca.pem"),
        quoted(&server_path, "client-ca.pem"),
        quoted(&server_path, "client-ca-key.pem"),
        quoted(&server_path, "bootstrap-ca.pem"),
    );
    let supervisor_toml = format!(
        "[tls]\nca_file = {}\ncert_file = {}\nkey_file = {}\n",
        quoted(&host_path, "server-ca.pem"),
        quoted(&host_path, "bootstrap.pem"),
        quoted(&host_path, "bootstrap-key.pem"),
    );

    let targets = [
        Target {
            dir: options.server_dir.clone(),
            files: vec![
                cert("server.pem", &server.cert_pem),
                key("server-key.pem", &server.key_pem),
                cert("client-ca.pem", &client_ca_pem),
                key("client-ca-key.pem", &client_ca_key),
                cert("bootstrap-ca.pem", &bootstrap_ca_pem),
                cert("server.toml.fragment", &server_toml),
            ],
        },
        Target {
            dir: options.offline_dir.clone(),
            files: vec![
                cert("server-ca.pem", &server_ca_pem),
                key("server-ca-key.pem", &server_ca_key),
                cert("bootstrap-ca.pem", &bootstrap_ca_pem),
                key("bootstrap-ca-key.pem", &bootstrap_ca_key),
                cert("bootstrap.pem", &bootstrap.cert_pem),
                key("bootstrap-key.pem", &bootstrap.key_pem),
                cert("supervisor.toml.fragment", &supervisor_toml),
            ],
        },
    ];
    write_all(&targets)?;

    let mut lines = vec![
        format!(
            "{}: for the Server host — merge server.toml.fragment into its server.toml",
            options.server_dir.display()
        ),
        format!(
            "{}: keep offline — the server CA and bootstrap CA keys; give every new host \
             server-ca.pem, bootstrap.pem and bootstrap-key.pem, as supervisor.toml.fragment names them",
            options.offline_dir.display()
        ),
        format!("bootstrap certificate serial: {}", bootstrap.serial),
    ];
    if same_file_system(&options.server_dir, &options.offline_dir) {
        lines.push(format!(
            "notice: both directories are on one file system; move {} off the Server host",
            options.offline_dir.display()
        ));
    }
    Ok(lines)
}

/// `server pki server-cert` (ADR-0029 clause 7): a server certificate from the offline server CA,
/// for the Server or for a Gateway.
///
/// # Errors
/// Returns the message to print when an option is invalid, the CA cannot be read, a file exists,
/// or writing fails.
pub fn server_cert(
    offline_dir: &Path,
    names: &[String],
    days: u32,
    out: &Path,
) -> Result<Vec<String>, String> {
    let names = self::names(names)?;
    check_days(days)?;
    let ca = load_authority(offline_dir, "server-ca")?;
    let made = server_certificate(&ca, &names, days)?;
    write_all(&[Target {
        dir: out.to_path_buf(),
        files: vec![
            cert("server.pem", &made.cert_pem),
            key("server-key.pem", &made.key_pem),
        ],
    }])?;
    let mut lines = vec![format!(
        "{}: server.pem and server-key.pem, serial {}",
        out.display(),
        made.serial
    )];
    lines.extend(cut_notice(&made, &ca));
    Ok(lines)
}

/// `server pki bootstrap-cert` (ADR-0029 clause 8): a bootstrap certificate from the offline
/// bootstrap CA.
///
/// # Errors
/// Returns the message to print when an option is invalid, the CA cannot be read, a file exists,
/// or writing fails.
pub fn bootstrap_cert(offline_dir: &Path, days: u32, out: &Path) -> Result<Vec<String>, String> {
    check_days(days)?;
    let ca = load_authority(offline_dir, "bootstrap-ca")?;
    let made = bootstrap_certificate(&ca, days)?;
    write_all(&[Target {
        dir: out.to_path_buf(),
        files: vec![
            cert("bootstrap.pem", &made.cert_pem),
            key("bootstrap-key.pem", &made.key_pem),
        ],
    }])?;
    let mut lines = vec![
        format!("{}: bootstrap.pem and bootstrap-key.pem", out.display()),
        format!("bootstrap certificate serial: {}", made.serial),
    ];
    lines.extend(cut_notice(&made, &ca));
    Ok(lines)
}

/// When one certificate ends, as `server pki status` lists it and the running Server watches it.
#[derive(Debug, Clone)]
pub struct Ending {
    pub file: PathBuf,
    pub subject: String,
    pub serial: String,
    pub not_before: OffsetDateTime,
    pub not_after: OffsetDateTime,
}

/// Where an end stands against a moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// More left than the warning reaches.
    Fine,
    /// Ends within [`WARN_DAYS`], or within the last third of a shorter life.
    Ending,
    /// Has ended.
    Ended,
}

impl Ending {
    /// Where this end stands at `now`.
    #[must_use]
    pub fn standing(&self, now: OffsetDateTime) -> Standing {
        let warning = Duration::days(WARN_DAYS).min((self.not_after - self.not_before) / 3);
        if self.not_after <= now {
            Standing::Ended
        } else if self.not_after - now <= warning {
            Standing::Ending
        } else {
            Standing::Fine
        }
    }

    /// Whole days left at `now`, negative once ended.
    #[must_use]
    pub fn days_left(&self, now: OffsetDateTime) -> i64 {
        (self.not_after - now).whole_days()
    }
}

/// Reads the first certificate of a PEM file.
///
/// # Errors
/// Returns the message to print when the file cannot be read or holds no certificate.
pub fn read_ending(file: &Path) -> Result<Ending, String> {
    let pem = std::fs::read(file).map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    let der = opamp::tls::certificates(&pem)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?
        .into_iter()
        .next()
        .ok_or_else(|| format!("{} holds no certificate", file.display()))?;
    let (_, cert) = X509Certificate::from_der(der.as_ref())
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    Ok(Ending {
        file: file.to_path_buf(),
        subject: cert.subject().to_string(),
        serial: hex::encode(cert.raw_serial()),
        not_before: cert.validity().not_before.to_datetime(),
        not_after: cert.validity().not_after.to_datetime(),
    })
}

/// The certificates a running Server depends on (ADR-0029 clause 11): its own, the client CA as
/// both `[tls]` and `[client_ca]` name it, and the bootstrap CA. Each file once.
#[must_use]
pub fn depended_on(config: &ServerConfig) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(tls) = &config.tls {
        files.push(tls.cert_file.clone());
        files.extend(tls.client_ca_file.clone());
    }
    if let Some(ca) = &config.client_ca {
        files.push(ca.cert_file.clone());
    }
    if let Some(enrolment) = &config.enrolment {
        files.push(enrolment.bootstrap_ca_file.clone());
    }
    let mut seen = std::collections::HashSet::new();
    files.retain(|file| seen.insert(file.clone()));
    files
}

/// `server pki status` (ADR-0029 clause 12): every certificate the configuration depends on and
/// every certificate in the offline directory, with its end.
///
/// # Errors
/// Returns the message to print when a named file cannot be read.
pub fn status(
    config: Option<&ServerConfig>,
    offline_dir: Option<&Path>,
) -> Result<Vec<Ending>, String> {
    let mut files = config.map(depended_on).unwrap_or_default();
    if let Some(dir) = offline_dir {
        for name in ["server-ca.pem", "bootstrap-ca.pem", "bootstrap.pem"] {
            let file = dir.join(name);
            if file.exists() && !files.contains(&file) {
                files.push(file);
            }
        }
    }
    files.iter().map(|file| read_ending(file)).collect()
}

/// The exit code of `server pki status`: `0` when nothing is ending, `1` when something is, `2`
/// when something has ended.
#[must_use]
pub fn exit_code(endings: &[Ending], now: OffsetDateTime) -> i32 {
    endings
        .iter()
        .map(|ending| match ending.standing(now) {
            Standing::Fine => 0,
            Standing::Ending => 1,
            Standing::Ended => 2,
        })
        .max()
        .unwrap_or(0)
}

/// The running Server's look at what it depends on (ADR-0029 clause 11): a certificate that is
/// ending is a warning and `pki.expiring`, one that has ended an error and `pki.expired`.
/// Nothing stops: the hosts decide for themselves.
pub fn warn_endings(
    files: &[PathBuf],
    audit: Option<&dyn crate::audit::Audit>,
    now: OffsetDateTime,
) {
    for file in files {
        let ending = match read_ending(file) {
            Ok(ending) => ending,
            Err(e) => {
                tracing::warn!("cannot read when a certificate ends: {e}");
                continue;
            }
        };
        let (event, outcome) = match ending.standing(now) {
            Standing::Fine => continue,
            Standing::Ending => {
                tracing::warn!(
                    file = %ending.file.display(),
                    subject = %ending.subject,
                    not_after = %ending.not_after.date(),
                    days_left = ending.days_left(now),
                    "a certificate the Server depends on ends soon; renew it"
                );
                ("pki.expiring", "expiring")
            }
            Standing::Ended => {
                tracing::error!(
                    file = %ending.file.display(),
                    subject = %ending.subject,
                    not_after = %ending.not_after.date(),
                    "a certificate the Server depends on has ended; renew it"
                );
                ("pki.expired", "expired")
            }
        };
        if let Some(audit) = audit {
            let not_after_ms = u64::try_from(ending.not_after.unix_timestamp())
                .unwrap_or(0)
                .saturating_mul(1000);
            let _ = audit.record(
                crate::audit::Entry::new(event, outcome)
                    .with("file", ending.file.display().to_string())
                    .with("subject", ending.subject.clone())
                    .with("serial", ending.serial.clone())
                    .with("not_after_ms", not_after_ms),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Making certificates
// ---------------------------------------------------------------------------------------------

/// A self-signed CA (ADR-0029 clause 5): `CA:TRUE` with a path length of 0, `keyCertSign` and
/// `cRLSign`. Returns its certificate and key as PEM, and the authority that signs with it.
fn authority(common_name: &str, days: u32) -> Result<(String, String, Authority), String> {
    let key = KeyPair::generate().map_err(|e| format!("cannot make a key: {e}"))?;
    let (not_before, not_after, _) = life(days, None)?;
    let mut params = CertificateParams::default();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params.serial_number = Some(crate::ca::random_serial()?);
    params.not_before = not_before;
    params.not_after = not_after;
    let cert = params
        .self_signed(&key)
        .map_err(|e| format!("cannot make the {common_name}: {e}"))?;
    let key_pem = key.serialize_pem();
    Ok((
        cert.pem(),
        key_pem,
        Authority {
            issuer: Issuer::new(params, key),
            end: not_after,
            common_name: common_name.to_string(),
        },
    ))
}

/// A server certificate (ADR-0029 clause 6): every name, and both loopback addresses.
fn server_certificate(ca: &Authority, names: &[SanType], days: u32) -> Result<Made, String> {
    let mut params = CertificateParams::default();
    let subject = match &names[0] {
        SanType::DnsName(name) => name.to_string(),
        SanType::IpAddress(ip) => ip.to_string(),
        _ => unreachable!("names() yields DNS names and IP addresses"),
    };
    params.distinguished_name.push(DnType::CommonName, subject);
    params.subject_alt_names = names.to_vec();
    for loopback in [
        std::net::IpAddr::from([127, 0, 0, 1]),
        std::net::IpAddr::from(std::net::Ipv6Addr::LOCALHOST),
    ] {
        let san = SanType::IpAddress(loopback);
        if !params.subject_alt_names.contains(&san) {
            params.subject_alt_names.push(san);
        }
    }
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf(params, ca, days)
}

/// A bootstrap certificate (ADR-0029 clause 8): a client certificate and nothing more, its
/// subject the bootstrap CA's with `CA` dropped.
fn bootstrap_certificate(ca: &Authority, days: u32) -> Result<Made, String> {
    let mut params = CertificateParams::default();
    let subject = ca
        .common_name
        .strip_suffix(" CA")
        .unwrap_or(&ca.common_name)
        .to_string();
    params.distinguished_name.push(DnType::CommonName, subject);
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    leaf(params, ca, days)
}

/// Signs a leaf: `CA:FALSE`, `digitalSignature`, a random serial, and a life that never reaches
/// past the CA's end (ADR-0029 clause 9).
fn leaf(mut params: CertificateParams, ca: &Authority, days: u32) -> Result<Made, String> {
    let key = KeyPair::generate().map_err(|e| format!("cannot make a key: {e}"))?;
    let (not_before, not_after, cut) = life(days, Some(ca.end))?;
    let serial = crate::ca::random_serial()?;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.serial_number = Some(serial.clone());
    params.not_before = not_before;
    params.not_after = not_after;
    let cert = params
        .signed_by(&key, &ca.issuer)
        .map_err(|e| format!("cannot sign with the {}: {e}", ca.common_name))?;
    Ok(Made {
        cert_pem: cert.pem(),
        key_pem: key.serialize_pem(),
        serial: hex::encode(serial.to_bytes()),
        cut,
    })
}

/// From a moment ago to `days` from now, cut to `ceiling` when it would reach past it.
fn life(
    days: u32,
    ceiling: Option<OffsetDateTime>,
) -> Result<(OffsetDateTime, OffsetDateTime, bool), String> {
    let now = OffsetDateTime::now_utc();
    let asked = now
        .checked_add(Duration::days(i64::from(days)))
        .ok_or_else(|| format!("{days} days is out of range"))?;
    match ceiling {
        Some(end) if end <= now => Err(format!("the CA ended on {}", end.date())),
        Some(end) if asked > end => Ok((now - CLOCK_SKEW, end, true)),
        _ => Ok((now - CLOCK_SKEW, asked, false)),
    }
}

/// The notice a cut life leaves.
fn cut_notice(made: &Made, ca: &Authority) -> Option<String> {
    made.cut.then(|| {
        format!(
            "notice: the certificate ends with the {} on {}, before the days asked for",
            ca.common_name,
            ca.end.date()
        )
    })
}

/// Loads `<stem>.pem` and `<stem>-key.pem` from the offline directory as an authority.
fn load_authority(dir: &Path, stem: &str) -> Result<Authority, String> {
    let cert_file = dir.join(format!("{stem}.pem"));
    let key_file = dir.join(format!("{stem}-key.pem"));
    let cert_pem = std::fs::read_to_string(&cert_file)
        .map_err(|e| format!("cannot read {}: {e}", cert_file.display()))?;
    let key_pem = std::fs::read_to_string(&key_file)
        .map_err(|e| format!("cannot read {}: {e}", key_file.display()))?;
    let key = KeyPair::from_pem(&key_pem)
        .map_err(|e| format!("cannot read {}: {e}", key_file.display()))?;
    let ending = read_ending(&cert_file)?;
    let common_name = {
        let der = opamp::tls::certificates(cert_pem.as_bytes())
            .map_err(|e| format!("cannot read {}: {e}", cert_file.display()))?
            .into_iter()
            .next()
            .ok_or_else(|| format!("{} holds no certificate", cert_file.display()))?;
        let (_, cert) = X509Certificate::from_der(der.as_ref())
            .map_err(|e| format!("cannot read {}: {e}", cert_file.display()))?;
        let common_name = cert
            .subject()
            .iter_common_name()
            .next()
            .and_then(|cn| cn.as_str().ok())
            .map(str::to_string);
        common_name.ok_or_else(|| format!("{} names no common name", cert_file.display()))?
    };
    let issuer = Issuer::from_ca_cert_pem(&cert_pem, key)
        .map_err(|e| format!("cannot use {} as a CA: {e}", cert_file.display()))?;
    Ok(Authority {
        issuer,
        end: ending.not_after,
        common_name,
    })
}

// ---------------------------------------------------------------------------------------------
// Checking what is asked for
// ---------------------------------------------------------------------------------------------

/// A fleet name (ADR-0029 clause 5): letters, digits, space, `.` and `-`, at most 40 characters.
fn check_fleet(fleet: &str) -> Result<(), String> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-');
    if fleet.trim().is_empty() || fleet.len() > 40 || !fleet.chars().all(allowed) {
        return Err(format!(
            "--fleet {fleet:?}: letters, digits, space, '.' and '-', at most 40 characters"
        ));
    }
    Ok(())
}

/// A life of at least a day.
fn check_days(days: u32) -> Result<(), String> {
    if days == 0 {
        return Err("a certificate needs at least one day".to_string());
    }
    Ok(())
}

/// The `--name`s of a server certificate (ADR-0029 clause 6): at least one, each a DNS name or an
/// IP address, no wildcard.
fn names(names: &[String]) -> Result<Vec<SanType>, String> {
    if names.is_empty() {
        return Err(
            "--name is required: the DNS name or address the certificate is dialled by".into(),
        );
    }
    names
        .iter()
        .map(|name| {
            if let Ok(ip) = name.parse::<std::net::IpAddr>() {
                return Ok(SanType::IpAddress(ip));
            }
            if !is_dns_name(name) {
                return Err(format!(
                    "--name {name:?} is neither a DNS name nor an IP address (no wildcards)"
                ));
            }
            name.as_str()
                .try_into()
                .map(SanType::DnsName)
                .map_err(|e| format!("--name {name:?}: {e}"))
        })
        .collect()
}

/// A DNS name: labels of letters, digits and `-`, none empty, none starting or ending with `-`,
/// at most 63 characters each and 253 in all.
fn is_dns_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

// ---------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------

fn cert(name: &'static str, contents: &str) -> File {
    File {
        name,
        contents: contents.to_string(),
        key: false,
    }
}

fn key(name: &'static str, contents: &str) -> File {
    File {
        name,
        contents: contents.to_string(),
        key: true,
    }
}

/// The path a fragment names a file by: where the directory is meant to live, absolute.
fn destination(dir: &Path, meant: Option<&Path>) -> Result<PathBuf, String> {
    let path = match meant {
        Some(path) => path.to_path_buf(),
        None if dir.is_absolute() => dir.to_path_buf(),
        None => std::env::current_dir()
            .map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?
            .join(dir),
    };
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    Ok(path)
}

/// A TOML basic string naming `file` in `dir`.
fn quoted(dir: &Path, file: &str) -> String {
    toml::Value::String(dir.join(file).display().to_string()).to_string()
}

/// Writes every target or none (ADR-0029 clause 10): refuses when any file exists, writes each
/// directory's files into a temporary directory inside it, and renames them into place only once
/// all are written. Directories it creates are `0700`, keys `0600`.
fn write_all(targets: &[Target]) -> Result<(), String> {
    for target in targets {
        for file in &target.files {
            let path = target.dir.join(file.name);
            if path.exists() {
                return Err(format!("{} exists; nothing was written", path.display()));
            }
        }
    }
    let mut staged = Vec::new();
    for target in targets {
        create_private_dir(&target.dir)?;
        let temp = target.dir.join(format!(".pki-{}", hex::encode(nonce()?)));
        create_private_dir(&temp)?;
        for file in &target.files {
            write_file(&temp.join(file.name), &file.contents, file.key)?;
        }
        staged.push((temp, target));
    }
    for (temp, target) in &staged {
        for file in &target.files {
            std::fs::rename(temp.join(file.name), target.dir.join(file.name)).map_err(|e| {
                format!("cannot place {}: {e}", target.dir.join(file.name).display())
            })?;
        }
        std::fs::remove_dir(temp).map_err(|e| format!("cannot remove {}: {e}", temp.display()))?;
    }
    Ok(())
}

fn nonce() -> Result<[u8; 8], String> {
    use ring::rand::SecureRandom as _;
    let mut bytes = [0u8; 8];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "no secure random source".to_string())?;
    Ok(bytes)
}

/// Creates `dir` and its missing parents; the ones it creates are `0700`.
fn create_private_dir(dir: &Path) -> Result<(), String> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        create_private_dir(parent)?;
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))
}

/// Writes a new file; a key is `0600` from the moment it exists.
fn write_file(path: &Path, contents: &str, key: bool) -> Result<(), String> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(if key { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = key;
    let mut file = options
        .open(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Whether two directories sit on one file system (ADR-0029 clause 4).
fn same_file_system(a: &Path, b: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        matches!(
            (std::fs::metadata(a), std::fs::metadata(b)),
            (Ok(a), Ok(b)) if a.dev() == b.dev()
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (a, b);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(root: &Path) -> InitOptions {
        InitOptions {
            server_dir: root.join("server"),
            offline_dir: root.join("offline"),
            server_path: None,
            host_path: None,
            fleet: DEFAULT_FLEET.to_string(),
            names: vec!["fleet.example.com".to_string(), "10.0.0.5".to_string()],
            ca_days: CA_DAYS,
            server_days: SERVER_DAYS,
            bootstrap_days: BOOTSTRAP_DAYS,
        }
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn parsed(file: &Path) -> Vec<u8> {
        let pem = std::fs::read(file).expect("read");
        opamp::tls::certificates(&pem).expect("pem")[0]
            .as_ref()
            .to_vec()
    }

    // Verifies: ADR-0029
    #[test]
    fn init_splits_what_the_server_needs_from_what_stays_offline() {
        let root = tempfile::tempdir().expect("tempdir");
        let lines = init(&options(root.path())).expect("init");
        assert_eq!(
            files(&root.path().join("server")),
            [
                "bootstrap-ca.pem",
                "client-ca-key.pem",
                "client-ca.pem",
                "server-key.pem",
                "server.pem",
                "server.toml.fragment"
            ]
        );
        assert_eq!(
            files(&root.path().join("offline")),
            [
                "bootstrap-ca-key.pem",
                "bootstrap-ca.pem",
                "bootstrap-key.pem",
                "bootstrap.pem",
                "server-ca-key.pem",
                "server-ca.pem",
                "supervisor.toml.fragment"
            ]
        );
        assert!(lines
            .iter()
            .any(|l| l.starts_with("bootstrap certificate serial: ")));
        assert!(lines
            .iter()
            .any(|l| l.starts_with("notice: both directories")));
    }

    // Verifies: ADR-0029
    #[cfg(unix)]
    #[test]
    fn keys_are_owner_only_and_new_directories_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().expect("tempdir");
        init(&options(root.path())).expect("init");
        for dir in ["server", "offline"] {
            let dir = root.path().join(dir);
            let mode = std::fs::metadata(&dir).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o700, "{}", dir.display());
            for name in files(&dir) {
                let mode = std::fs::metadata(dir.join(&name))
                    .expect("meta")
                    .permissions()
                    .mode()
                    & 0o777;
                let expected = if name.ends_with("-key.pem") {
                    0o600
                } else {
                    0o644
                };
                assert_eq!(mode, expected, "{name}");
            }
        }
    }

    // Verifies: ADR-0029
    #[test]
    fn a_second_init_refuses_and_writes_nothing() {
        let root = tempfile::tempdir().expect("tempdir");
        init(&options(root.path())).expect("init");
        let before = std::fs::read(root.path().join("server/server.pem")).expect("read");
        let error = init(&options(root.path())).expect_err("refused");
        assert!(error.contains("exists"), "{error}");
        assert_eq!(
            std::fs::read(root.path().join("server/server.pem")).expect("read"),
            before
        );
        assert!(
            files(&root.path().join("server"))
                .iter()
                .all(|n| !n.starts_with(".pki-")),
            "no temporary directory is left"
        );
    }

    // Verifies: ADR-0029
    #[test]
    fn the_cas_have_their_own_subjects_and_sign_no_ca() {
        let root = tempfile::tempdir().expect("tempdir");
        init(&options(root.path())).expect("init");
        let mut subjects = Vec::new();
        for file in [
            "offline/server-ca.pem",
            "server/client-ca.pem",
            "offline/bootstrap-ca.pem",
        ] {
            let der = parsed(&root.path().join(file));
            let (_, cert) = X509Certificate::from_der(&der).expect("parse");
            let constraints = cert
                .basic_constraints()
                .expect("ext")
                .expect("present")
                .value;
            assert!(constraints.ca, "{file}");
            assert_eq!(constraints.path_len_constraint, Some(0), "{file}");
            subjects.push(cert.subject().to_string());
        }
        assert_eq!(
            subjects,
            [
                "CN=opamp-fleet server CA",
                "CN=opamp-fleet client CA",
                "CN=opamp-fleet bootstrap CA"
            ]
        );
    }

    // Verifies: ADR-0029
    #[test]
    fn a_server_certificate_names_what_is_dialled_and_the_loopback() {
        use x509_parser::extensions::GeneralName;
        let root = tempfile::tempdir().expect("tempdir");
        init(&options(root.path())).expect("init");
        let der = parsed(&root.path().join("server/server.pem"));
        let (_, cert) = X509Certificate::from_der(&der).expect("parse");
        let san = cert
            .subject_alternative_name()
            .expect("ext")
            .expect("present")
            .value;
        let names: Vec<String> = san
            .general_names
            .iter()
            .map(|name| match name {
                GeneralName::DNSName(dns) => (*dns).to_string(),
                GeneralName::IPAddress(ip) => match ip.len() {
                    4 => std::net::IpAddr::from(<[u8; 4]>::try_from(*ip).expect("v4")).to_string(),
                    _ => std::net::IpAddr::from(<[u8; 16]>::try_from(*ip).expect("v6")).to_string(),
                },
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(names, ["fleet.example.com", "10.0.0.5", "127.0.0.1", "::1"]);
        assert_eq!(cert.subject().to_string(), "CN=fleet.example.com");
    }

    // Verifies: ADR-0029
    #[test]
    fn a_wildcard_an_empty_name_and_a_bad_fleet_are_refused() {
        let root = tempfile::tempdir().expect("tempdir");
        for name in ["*.example.com", "", "under_score.example"] {
            let mut o = options(root.path());
            o.names = vec![name.to_string()];
            assert!(init(&o).is_err(), "{name:?} was accepted");
        }
        let mut o = options(root.path());
        o.names.clear();
        assert!(init(&o).is_err(), "no --name was accepted");
        let mut o = options(root.path());
        o.fleet = "a/b".to_string();
        assert!(init(&o).is_err(), "a fleet with '/' was accepted");
        assert!(!root.path().join("server").exists(), "nothing was written");
    }

    // Verifies: ADR-0029
    #[test]
    fn a_certificate_never_outlives_its_ca() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut o = options(root.path());
        o.ca_days = 10;
        init(&o).expect("init");
        let ca = read_ending(&root.path().join("offline/server-ca.pem")).expect("ca");
        let server = read_ending(&root.path().join("server/server.pem")).expect("server");
        assert_eq!(server.not_after, ca.not_after);
        let lines = server_cert(
            &root.path().join("offline"),
            &["fleet.example.com".to_string()],
            SERVER_DAYS,
            &root.path().join("next"),
        )
        .expect("server-cert");
        assert!(lines
            .iter()
            .any(|l| l.starts_with("notice: the certificate ends with")));
    }

    // Verifies: ADR-0029
    #[test]
    fn server_cert_and_bootstrap_cert_sign_with_the_offline_cas() {
        let root = tempfile::tempdir().expect("tempdir");
        init(&options(root.path())).expect("init");
        let offline = root.path().join("offline");
        server_cert(
            &offline,
            &["gw.example.com".to_string()],
            90,
            &root.path().join("gw"),
        )
        .expect("server-cert");
        let lines = bootstrap_cert(&offline, 7, &root.path().join("next")).expect("bootstrap");
        let serial = read_ending(&root.path().join("next/bootstrap.pem"))
            .expect("read")
            .serial;
        assert!(lines.contains(&format!("bootstrap certificate serial: {serial}")));
        for (leaf, ca) in [
            ("gw/server.pem", "offline/server-ca.pem"),
            ("next/bootstrap.pem", "offline/bootstrap-ca.pem"),
        ] {
            let leaf_der = parsed(&root.path().join(leaf));
            let ca_der = parsed(&root.path().join(ca));
            let (_, leaf_cert) = X509Certificate::from_der(&leaf_der).expect("leaf");
            let (_, ca_cert) = X509Certificate::from_der(&ca_der).expect("ca");
            leaf_cert
                .verify_signature(Some(ca_cert.public_key()))
                .unwrap_or_else(|e| panic!("{leaf} is not signed by {ca}: {e}"));
        }
        let bootstrap = read_ending(&root.path().join("next/bootstrap.pem")).expect("read");
        assert_eq!(bootstrap.subject, "CN=opamp-fleet bootstrap");
    }

    /// A self-signed CA that lived `lived` days and has `left` days left, written to `file`.
    fn aged_ca(file: &Path, lived: i64, left: i64) {
        let key = KeyPair::generate().expect("key");
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, "aged CA");
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        let now = OffsetDateTime::now_utc();
        params.not_before = now - Duration::days(lived);
        params.not_after = now + Duration::days(left);
        std::fs::write(file, params.self_signed(&key).expect("ca").pem()).expect("write");
    }

    #[derive(Default)]
    struct Recorded(std::sync::Mutex<Vec<crate::audit::Entry>>);

    impl crate::audit::Audit for Recorded {
        fn record(&self, entry: crate::audit::Entry) -> Result<(), crate::audit::Unavailable> {
            self.0.lock().expect("lock").push(entry);
            Ok(())
        }
    }

    // Verifies: ADR-0029
    #[test]
    fn the_server_records_what_is_ending_and_what_has_ended() {
        let root = tempfile::tempdir().expect("tempdir");
        let fine = root.path().join("fine.pem");
        let ending = root.path().join("ending.pem");
        let ended = root.path().join("ended.pem");
        aged_ca(&fine, 10, 3000);
        aged_ca(&ending, 3600, 10);
        aged_ca(&ended, 3650, -1);
        let audit = Recorded::default();
        warn_endings(
            &[fine, ending.clone(), ended.clone()],
            Some(&audit),
            OffsetDateTime::now_utc(),
        );
        let recorded = audit.0.lock().expect("lock");
        let seen: Vec<(String, String)> = recorded
            .iter()
            .map(|entry| (entry.event.clone(), entry.outcome.clone()))
            .collect();
        assert_eq!(
            seen,
            [
                ("pki.expiring".to_string(), "expiring".to_string()),
                ("pki.expired".to_string(), "expired".to_string())
            ]
        );
        assert_eq!(
            recorded[0].fields.get("file"),
            Some(&crate::audit::Field::Text(ending.display().to_string()))
        );
    }

    // Verifies: ADR-0029
    #[test]
    fn status_exits_by_the_worst_end() {
        let now = OffsetDateTime::now_utc();
        let ending = |lived: i64, left: i64| Ending {
            file: PathBuf::from("x.pem"),
            subject: String::new(),
            serial: String::new(),
            not_before: now - Duration::days(lived),
            not_after: now + Duration::days(left),
        };
        assert_eq!(exit_code(&[ending(0, 400)], now), 0);
        assert_eq!(exit_code(&[ending(0, 400), ending(380, 20)], now), 1);
        assert_eq!(exit_code(&[ending(380, 20), ending(30, -1)], now), 2);
        assert_eq!(exit_code(&[], now), 0);
        // A fresh 30-day bootstrap certificate is not ending; its last third is.
        assert_eq!(exit_code(&[ending(0, 30)], now), 0);
        assert_eq!(exit_code(&[ending(21, 9)], now), 1);
    }
}
