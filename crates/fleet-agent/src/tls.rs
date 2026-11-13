//! Which TLS material the Client uses (ADR-0012, ADR-0040), read from disk and handed to `opamp` as
//! [`ClientTls`] (ADR-0008).
//!
//! The decisions are the Client's: an optional CA file that *replaces* the built-in roots, and the
//! identity in force — the one the Server issued, else the one the operator configured. Building a
//! rustls configuration or an HTTP client from that material is `opamp`'s.

use std::path::Path;

use opamp::client::ClientTls;
use opamp::tls::Identity;
use zeroize::Zeroizing;

use crate::config::ClientConfig;

/// The Server-issued client certificate, in the state directory beside the connection settings —
/// it belongs to the Client's one upstream connection, not to any single Agent (ADR-0040).
pub const ISSUED_CERT_FILE: &str = "client-cert.pem";
/// The private key of [`ISSUED_CERT_FILE`]. Generated on this host and never sent anywhere: what
/// leaves is a CSR over its public half.
pub const ISSUED_KEY_FILE: &str = "client-key.pem";
/// The key a request in flight asks to be certified, kept apart from [`ISSUED_KEY_FILE`] so the
/// certificate in force keeps its own key until the new one is proved (ADR-0040 clause 10).
pub const PENDING_KEY_FILE: &str = "client-key.pending.pem";
/// An issued certificate staged beside [`PENDING_KEY_FILE`] while the two are put in force, so a
/// swap cut short is completed at the next start rather than leaving a mismatched pair.
pub const PENDING_CERT_FILE: &str = "client-cert.pending.pem";
/// The request in flight, re-sent unchanged until it is answered — the enrolment queue knows a
/// request by its public key (ADR-0027 clause 9).
pub const PENDING_CSR_FILE: &str = "client-csr.pending.pem";

/// The trust and the identity in force.
///
/// # Errors
/// Returns an error naming the file that cannot be read.
pub fn client_tls(config: &ClientConfig) -> Result<ClientTls, String> {
    client_tls_for(config, None)
}

/// The same, for a **candidate** identity: an offered certificate is proved by connecting with it
/// before it is stored (ADR-0013's MUST, applied to the certificate in ADR-0040), so the
/// certificate under test comes from the offer while its key is the pending one on disk.
///
/// # Errors
/// Returns an error naming the file that cannot be read, or the missing key of a candidate.
pub fn client_tls_for(
    config: &ClientConfig,
    candidate_cert: Option<&[u8]>,
) -> Result<ClientTls, String> {
    let ca_pem = trust_pem(config)?;
    let identity = match candidate_cert {
        Some(cert) => {
            // An offered certificate belongs to the key this Client generated for its request;
            // without that key there is nothing to prove possession with.
            // The key of the request in flight, or — for a certificate offered over a key already in
            // force — that one.
            let pending = config.state_dir.join(PENDING_KEY_FILE);
            let key = if pending.exists() {
                pending
            } else {
                config.state_dir.join(ISSUED_KEY_FILE)
            };
            if !key.exists() {
                return Err(format!(
                    "an offered certificate has no key to go with it — {} is missing",
                    key.display()
                ));
            }
            // `opamp` takes the key as its own buffer; the copy is made here and the one read
            // is wiped (ADR-0014 clause 3).
            Some(Identity {
                cert_pem: cert.to_vec(),
                key_pem: key_in(&key, state_file(&key)?)?.to_vec(),
            })
        }
        // An identity opened from a `bootstrap.p12` stands in for the configured one until the
        // Server's certificate is stored (ADR-0027 clause 15); the issued one outranks both.
        None => match (&config.bootstrap_identity, config.is_enrolled()) {
            (Some(bootstrap), false) => Some(Identity {
                cert_pem: bootstrap.cert_pem.clone(),
                key_pem: bootstrap.key_pem.to_vec(),
            }),
            _ => match config.client_identity() {
                // The pair the Server issued lies in the state directory, read without following a
                // link; a pair the operator named in `[tls]` may be reached through one.
                Some((cert, key)) if cert.parent() == Some(config.state_dir.as_path()) => {
                    Some(Identity {
                        cert_pem: certificates_in(&cert, state_file(&cert)?)?,
                        key_pem: key_in(&key, state_file(&key)?)?.to_vec(),
                    })
                }
                Some((cert, key)) => Some(Identity {
                    cert_pem: certificates_file(&cert)?,
                    key_pem: key_file(&key)?.to_vec(),
                }),
                None => None,
            },
        },
    };
    Ok(ClientTls { ca_pem, identity })
}

/// The trust anchors alone — what a download from a host that is not the Server needs. Reads no
/// identity, so a client certificate that cannot be read does not stop a download.
///
/// # Errors
/// Returns an error naming the CA file that cannot be read or holds no certificate.
pub fn trust(config: &ClientConfig) -> Result<ClientTls, String> {
    Ok(ClientTls {
        ca_pem: trust_pem(config)?,
        identity: None,
    })
}

/// The certificates of `[tls] ca_file`: those an enrolment read into memory, else the file's.
fn trust_pem(config: &ClientConfig) -> Result<Option<Vec<u8>>, String> {
    match &config.ca_pem {
        Some(pem) => Ok(Some(pem.clone())),
        None => config.ca_file().map(certificates_file).transpose(),
    }
}

/// A PEM file of certificates, parsed here so that a bad one is named by its path.
pub(crate) fn certificates_file(path: &Path) -> Result<Vec<u8>, String> {
    certificates_in(path, read(path)?)
}

pub(crate) fn certificates_in(path: &Path, pem: Vec<u8>) -> Result<Vec<u8>, String> {
    opamp::tls::certificates(&pem).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(pem)
}

/// A PEM file holding a private key, parsed here so that a bad one is named by its path. The key
/// is held, and parsed, in buffers wiped when they are dropped (ADR-0014).
pub(crate) fn key_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    key_in(path, read(path)?)
}

pub(crate) fn key_in(path: &Path, pem: Vec<u8>) -> Result<Zeroizing<Vec<u8>>, String> {
    let pem = Zeroizing::new(pem);
    opamp::tls::private_key(&pem)
        .map(Zeroizing::new)
        .map_err(|_| format!("{} contains no private key", path.display()))?;
    Ok(pem)
}

/// The identity in `bytes`, the PKCS#12 file at `path`, under `password`, as PEM in memory
/// (ADR-0027 clause 15). The caller reads the file, through no link the service's account placed
/// and at most [`fleet_core::pkcs12::MAX_FILE_BYTES`] of it (ADR-0021 clause 11).
///
/// # Errors
/// [`OpenError::WrongPassword`](fleet_core::pkcs12::file::OpenError) when the password does not
/// open it, and `Invalid` naming the file for everything else.
pub fn pkcs12_identity(
    path: &Path,
    bytes: &[u8],
    password: &str,
) -> Result<crate::config::BootstrapIdentity, fleet_core::pkcs12::file::OpenError> {
    use fleet_core::pkcs12::file::OpenError;
    let invalid = |why: String| OpenError::Invalid(format!("{}: {why}", path.display()));
    let opened = fleet_core::pkcs12::file::open(bytes, password).map_err(|e| match e {
        OpenError::WrongPassword => OpenError::WrongPassword,
        OpenError::Invalid(why) => invalid(why),
    })?;
    let mut cert_pem = pem_block("CERTIFICATE", &opened.certificate_der);
    let mut key_pem = pem_block("PRIVATE KEY", &opened.private_key_der);
    Ok(crate::config::BootstrapIdentity {
        cert_pem: std::mem::take(&mut *cert_pem).into_bytes(),
        // The buffer moves; nothing is copied.
        key_pem: Zeroizing::new(std::mem::take(&mut *key_pem).into_bytes()),
    })
}

/// `der` as one PEM block, in buffers wiped when they are dropped and sized up front, so none is
/// grown and left behind (ADR-0014).
pub(crate) fn pem_block(label: &str, der: &[u8]) -> Zeroizing<String> {
    use base64::Engine as _;
    let body = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(der));
    let (begin, end) = (
        format!("-----BEGIN {label}-----\n"),
        format!("-----END {label}-----\n"),
    );
    let lines = body.len().div_ceil(64);
    let mut pem = Zeroizing::new(String::with_capacity(
        begin.len() + body.len() + lines + end.len(),
    ));
    pem.push_str(&begin);
    for line in body.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str(&end);
    pem
}

/// A file `[tls]` names, read by its path: the run-time Client's own files, which its operator
/// may reach through a link. A privileged enrolment reads them through
/// [`crate::nofollow::read_privileged`] instead, and never by this path (ADR-0021 clause 11).
fn read(path: &Path) -> Result<Vec<u8>, String> {
    read_bounded(path)
        .map(|mut bytes| std::mem::take(&mut *bytes))
        .map_err(|why| format!("cannot read {}: {why}", path.display()))
}

/// The most a PEM file of certificates or a key may hold: a bundle of many certificates is far
/// below it.
pub(crate) const MAX_PEM_BYTES: u64 = 1 << 20;

/// The bytes of the regular file at `path`, of at most [`MAX_PEM_BYTES`], in a buffer wiped when
/// it is dropped (ADR-0014). The error says why, and leaves naming the file to the caller.
///
/// Anything but a regular file is refused before it is opened, and again by what was opened, so
/// a FIFO or a device at the name — `/dev/zero` — is never read. On Unix the open does not block,
/// so a FIFO swapped in between the two looks is refused as well.
///
/// # Errors
/// Returns why the file was not read.
pub(crate) fn read_bounded(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let before = std::fs::metadata(path).map_err(|e| e.to_string())?;
    check_regular(&before)?;
    let mut open = std::fs::OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut open, libc::O_NONBLOCK);
    crate::nofollow::read_opened(open.open(path).map_err(|e| e.to_string())?, MAX_PEM_BYTES)
        .map_err(|e| e.to_string())
}

fn check_regular(meta: &std::fs::Metadata) -> Result<(), String> {
    if meta.is_file() && meta.len() <= MAX_PEM_BYTES {
        Ok(())
    } else {
        Err(not_regular())
    }
}

fn not_regular() -> String {
    format!("not a regular file of at most {MAX_PEM_BYTES} bytes")
}

/// A file of the state directory, never read through a link: an install reads it as root, and the
/// directory belongs to the service's account (ADR-0021).
fn state_file(path: &Path) -> Result<Vec<u8>, String> {
    crate::nofollow::read(path, MAX_PEM_BYTES)
        .map(|mut bytes| std::mem::take(&mut *bytes))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The run-time Client reads a `[tls]` file only as a regular file within the bound: a file
    /// over it is refused, naming it, and so is a FIFO, without waiting for a writer.
    #[test]
    fn a_tls_file_is_read_only_as_a_regular_file_within_the_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let large = dir.path().join("large.pem");
        std::fs::File::create(&large)
            .expect("large")
            .set_len(MAX_PEM_BYTES + 1)
            .expect("size");
        let refused = certificates_file(&large).expect_err("over the bound");
        assert!(refused.contains("large.pem"), "{refused}");
        assert!(refused.contains("at most"), "{refused}");

        #[cfg(unix)]
        {
            let fifo = dir.path().join("fifo.pem");
            let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("name");
            // SAFETY: `name` is a valid NUL-terminated path.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0, "mkfifo");
            let (done, wait) = std::sync::mpsc::channel();
            std::thread::spawn(move || done.send(key_file(&fifo).err()));
            let refused = wait
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("the FIFO held the read up")
                .expect("refused");
            assert!(refused.contains("not a regular file"), "{refused}");
        }
    }

    /// A `bootstrap.p12` opens to PEM in memory and stands in for the configured identity until
    /// the Server's certificate is stored (ADR-0027 clause 15). The bound its read is held to is
    /// `config_init`'s, tested there.
    /// Verifies: ADR-0027
    #[test]
    fn an_opened_bootstrap_p12_is_presented_until_the_client_is_enrolled() {
        use fleet_core::pkcs12::file::OpenError;
        let dir = tempfile::tempdir().expect("tempdir");
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["bootstrap".to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("cert");
        let p12 = dir.path().join("bootstrap.p12");
        std::fs::write(
            &p12,
            fleet_core::pkcs12::file::seal(cert.der(), &key.serialize_der(), "twelve chars")
                .expect("sealed"),
        )
        .expect("write");
        let bytes = std::fs::read(&p12).expect("read");
        assert_eq!(
            pkcs12_identity(&p12, &bytes, "not the password"),
            Err(OpenError::WrongPassword)
        );
        let identity = pkcs12_identity(&p12, &bytes, "twelve chars").expect("opens");
        assert_eq!(
            opamp::tls::certificates(&identity.cert_pem).expect("pem")[0].as_ref(),
            cert.der().as_ref()
        );

        let config = ClientConfig {
            state_dir: dir.path().join("state"),
            bootstrap_identity: Some(identity.clone()),
            ..ClientConfig::default()
        };
        std::fs::create_dir_all(&config.state_dir).expect("state");
        assert_eq!(
            client_tls(&config).expect("tls").identity,
            Some(Identity {
                cert_pem: identity.cert_pem.clone(),
                key_pem: identity.key_pem.to_vec(),
            })
        );

        let issued = rcgen::KeyPair::generate().expect("key");
        let issued_cert = rcgen::CertificateParams::new(vec!["issued".to_string()])
            .expect("params")
            .self_signed(&issued)
            .expect("cert");
        std::fs::write(config.state_dir.join(ISSUED_CERT_FILE), issued_cert.pem()).expect("w");
        std::fs::write(
            config.state_dir.join(ISSUED_KEY_FILE),
            issued.serialize_pem(),
        )
        .expect("w");
        let presented = client_tls(&config).expect("tls").identity.expect("one");
        assert_eq!(
            presented.cert_pem,
            issued_cert.pem().into_bytes(),
            "the issued certificate outranks the bootstrap identity"
        );
    }

    /// The Client's secrets are held in buffers wiped when they are dropped: the key of a
    /// bootstrap identity, a key file, a bootstrap password, the Supervisor Endpoint token and
    /// `[packages] archive_key`.
    /// Were any of them a plain buffer again, this would not compile.
    /// Verifies: ADR-0014
    #[test]
    fn the_clients_secrets_are_held_in_zeroizing_buffers() {
        use crate::config::BootstrapIdentity;
        fn bootstrap_key(identity: BootstrapIdentity) -> Zeroizing<Vec<u8>> {
            identity.key_pem
        }
        fn archive_key(packages: crate::config::PackagesConfig) -> Option<Zeroizing<String>> {
            packages.archive_key
        }
        let reads_key: fn(&Path) -> Result<Zeroizing<Vec<u8>>, String> = key_file;
        let reads_password = |path: &Path| -> Result<Zeroizing<String>, String> {
            crate::config_init::read_password_file(path, "--flag", &[])
        };
        let makes_token: fn() -> Result<Zeroizing<String>, String> =
            crate::supervisor::endpoint::new_token;

        let dir = tempfile::tempdir().expect("tempdir");
        let key = rcgen::KeyPair::generate().expect("key");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&key_path, key.serialize_pem()).expect("write");
        assert_eq!(
            *reads_key(&key_path).expect("a key"),
            key.serialize_pem().into_bytes()
        );
        let password_path = dir.path().join("password");
        std::fs::write(&password_path, "twelve chars\n").expect("write");
        assert_eq!(
            reads_password(&password_path).expect("a password").as_str(),
            "twelve chars"
        );
        assert_eq!(makes_token().expect("a token").len(), 64);
        let identity = BootstrapIdentity {
            cert_pem: Vec::new(),
            key_pem: Zeroizing::new(b"key".to_vec()),
        };
        assert_eq!(*bootstrap_key(identity), b"key");
        let packages: crate::config::PackagesConfig =
            toml::from_str("archive_key = \"a-long-secret\"\n").expect("parses");
        assert_eq!(
            archive_key(packages).expect("a key").as_str(),
            "a-long-secret"
        );
    }
}
