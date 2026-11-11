//! The questionnaire's enrolment (ADR-0027 clause 16): a host not yet enrolled is carried to its
//! first certificate before its service is registered, with the bootstrap pair where it lies, or
//! the identity a `bootstrap.p12` opened to, in memory (ADR-0027).
//!
//! Nothing here is a second enrolment. It is the Client's own upstream connection (ADR-0040) —
//! the same CSR, the same pending key, the same acceptance of the offered certificate into the
//! state directory — run with the Client's own Agent alone and ended the moment that certificate
//! is stored. The instance identity it creates in the state directory is the one the service then
//! runs with.

use std::path::{Path, PathBuf};

use tokio::sync::mpsc;

use crate::config::ClientConfig;
use crate::config_init::{Answered, Bootstrap, Written};
use crate::engine::{Engine, EngineAgent};
use crate::service::runtime;
use crate::shutdown::{shutdown_channel, Shutdown};
use crate::storage::Storage;
use crate::supervisor::agent::AgentState;
use crate::transport::{self, RunOutcome};

/// The files an enrolment writes into `state_dir`, the directory included — what `service config`
/// hands to the service's account afterwards, by name rather than by walking a directory that
/// account owns.
#[must_use]
pub fn written(state_dir: &Path) -> Vec<PathBuf> {
    let mut files = vec![state_dir.to_path_buf()];
    files.extend(
        [
            crate::storage::UID_FILE,
            crate::tls::ISSUED_CERT_FILE,
            crate::tls::ISSUED_KEY_FILE,
            crate::tls::PENDING_KEY_FILE,
            crate::tls::PENDING_CSR_FILE,
            crate::connection::SETTINGS_FILE,
        ]
        .map(|name| state_dir.join(name)),
    );
    files
}

/// Enrol on a runtime of its own, until the issued certificate is stored or Ctrl-C ends the wait.
///
/// `config` is the configuration the service will run with, its `state_dir` the service's.
/// `written` is what this run asked and wrote: an endpoint and trust it answered replace the
/// file's, and an endpoint named over a kept file holds the file to it
/// (ADR-draft-a-privileged-enrolment-connects-where-its-own-run-answered).
/// `roots` are the data root and the layout root. The CA file and the bootstrap pair are read
/// once, before anything connects; one in a root or the state directory, which the service's
/// account owns, is read through that directory, never through a link at its name.
///
/// # Errors
/// Returns an error when the CA file or the bootstrap pair cannot be read, when the state
/// directory cannot be prepared, or when the wait is interrupted before a certificate arrived.
pub fn run(
    config: &ClientConfig,
    bootstrap: &Bootstrap,
    written: &Written,
    roots: &[&Path],
) -> Result<(), String> {
    let config = prepare(config, bootstrap, written, roots)?;
    runtime::daemon_runtime()?.block_on(async move {
        let (stop, shutdown) = shutdown_channel();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = stop.send(true);
        });
        runtime::on_a_worker(connect(config, shutdown)).await
    })
}

/// Connect with `bootstrap` as the client identity and keep the connection until the Server's
/// certificate is in the state directory (ADR-0027 clauses 9 and 10).
///
/// The request's key fingerprint is logged as the request is generated, and printed here for a
/// request an earlier, interrupted run left in flight — what the operator matches in the
/// enrolment queue. A Server that refuses the request says so in the log, and the wait goes on
/// until `shutdown`: a closed window looks the same from here, and whether to try again is the
/// operator's to decide, not this loop's.
///
/// The CA file and the bootstrap pair are read once, before anything connects, with the state
/// directory as the account's; [`run`] names the roots too.
///
/// # Errors
/// Returns an error when the CA file or the bootstrap pair cannot be read, when the state
/// directory cannot be prepared, or when `shutdown` ends the wait before a certificate arrived.
pub async fn enrol(
    mut config: ClientConfig,
    bootstrap: Bootstrap,
    shutdown: Shutdown,
) -> Result<(), String> {
    read_once(&mut config, &bootstrap, &[])?;
    connect(config, shutdown).await
}

/// [`enrol`], with what it connects with already in `config`.
async fn connect(mut config: ClientConfig, mut shutdown: Shutdown) -> Result<(), String> {
    // A certificate swap a crash cut short is completed before anything reads the state.
    crate::csr::recover(&config.state_dir)?;
    // Read now, so a pair that is no certificate and key is named before anything connects.
    crate::tls::client_tls(&config)?;

    let storage = Storage::enrolling(config.state_dir.clone())
        .map_err(|e| format!("cannot prepare {}: {e}", config.state_dir.display()))?;
    // The Client's own Agent alone, taking nothing but the certificate: no Supervisor is started,
    // no remote configuration taken, and no package declared.
    let state = AgentState::new_without_remote_config(
        config.name.clone(),
        storage,
        crate::host::SystemHost,
    )
    .map_err(|e| format!("cannot restore the agent state: {e}"))?;
    let (event_tx, events) = mpsc::channel(1);
    let mut engine = Engine::with_processes(
        vec![EngineAgent {
            state,
            commands: None,
            stop: None,
            block_name: None,
        }],
        events,
        event_tx,
    );
    let telemetry = crate::telemetry::Telemetry::new();

    match crate::csr::pending_fingerprint(&config.state_dir) {
        Some(fingerprint) => println!(
            "enrolling with {}: resuming the request with key fingerprint {fingerprint}, which an \
             operator approves (Ctrl-C stops waiting)",
            config.endpoint
        ),
        None => println!(
            "enrolling with {}: an operator approves this host's request by the key fingerprint \
             logged below (Ctrl-C stops waiting)",
            config.endpoint
        ),
    }
    loop {
        let outcome =
            transport::run(&mut engine, &mut config, &mut shutdown, &telemetry, None).await;
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(e) => {
                telemetry.shutdown();
                return Err(e);
            }
        };
        if config.is_enrolled() {
            telemetry.shutdown();
            println!(
                "enrolled: the issued certificate is in {}",
                config.state_dir.display()
            );
            return Ok(());
        }
        match outcome {
            // Offered connection settings took effect; the request goes on over the new ones.
            RunOutcome::Reconfigured => {}
            RunOutcome::Shutdown | RunOutcome::RestartForUpdate => {
                telemetry.shutdown();
                return Err(
                    "enrolment stopped before the Server issued a certificate — run the \
                            questionnaire again to resume it"
                        .to_string(),
                );
            }
        }
    }
}

/// `config` as the enrolment connects with it: what the run answered in place of what the file
/// says (ADR-draft-a-privileged-enrolment-connects-where-its-own-run-answered),
/// and the CA file and the bootstrap pair read once, before anything connects, never again by a
/// path the account gave (ADR-0021 clause 11).
fn prepare(
    config: &ClientConfig,
    bootstrap: &Bootstrap,
    written: &Written,
    roots: &[&Path],
) -> Result<ClientConfig, String> {
    let mut config = config.clone();
    if let Some(answered) = &written.answered {
        take_answered(&mut config, answered);
    }
    holds_to_named(&config, written)?;
    read_once(&mut config, bootstrap, roots)?;
    Ok(config)
}

/// Refuses the enrolment where `--endpoint` named, over a file the install kept, another endpoint
/// than the file's: such a run answered nothing, and the file stands for the service
/// (ADR-draft-a-privileged-enrolment-connects-where-its-own-run-answered).
///
/// # Errors
/// Returns an error naming both endpoints.
pub fn holds_to_named(config: &ClientConfig, written: &Written) -> Result<(), String> {
    match &written.named_over_kept {
        Some(named) if *named != config.endpoint => Err(format!(
            "not enrolling: --endpoint names {named}, and the configuration kept names {}. \
             Correct the configuration or the flag, and install again",
            config.endpoint
        )),
        _ => Ok(()),
    }
}

/// Puts what the run answered in place of what `config` read from the file: the endpoint, and the
/// CA copy with the certificates written into it, or no CA where none was answered
/// (ADR-draft-a-privileged-enrolment-connects-where-its-own-run-answered).
fn take_answered(config: &mut ClientConfig, answered: &Answered) {
    config.endpoint.clone_from(&answered.endpoint);
    let ca_file = answered.ca.as_ref().map(|(copy, _)| copy.clone());
    config.ca_pem = answered.ca.as_ref().map(|(_, pem)| pem.clone());
    match &mut config.tls {
        Some(tls) => tls.ca_file = ca_file,
        None if ca_file.is_some() => {
            config.tls = Some(crate::config::TlsConfig {
                ca_file,
                cert_file: None,
                key_file: None,
            });
        }
        None => {}
    }
}

/// Reads what a privileged enrolment connects with, once, into `config`: the certificates of
/// `[tls] ca_file`, and the bootstrap identity, which a `bootstrap.p12` already opened to in
/// memory (ADR-0027 clause 15). The connection then reads none of these files again.
///
/// `service install` and `service config` run as root or an Administrator, and these paths come
/// from answers whose defaults `supervisor.toml` gives, a file the service's account can rewrite
/// (ADR-0021 clause 11). So each is read as [`read_privileged`] says, and a file that is no
/// certificate or no key is named.
fn read_once(
    config: &mut ClientConfig,
    bootstrap: &Bootstrap,
    roots: &[&Path],
) -> Result<(), String> {
    let state_dir = config.state_dir.clone();
    let read = |path: &Path| read_privileged(path, roots, Some(&state_dir));
    if let (None, Some(ca)) = (&config.ca_pem, config.ca_file()) {
        config.ca_pem = Some(crate::tls::certificates_in(ca, read(ca)?.to_vec())?);
    }
    config.bootstrap_identity = Some(match bootstrap {
        Bootstrap::Pair { cert, key } => crate::config::BootstrapIdentity {
            cert_pem: crate::tls::certificates_in(cert, read(cert)?.to_vec())?,
            key_pem: crate::tls::key_in(key, std::mem::take(&mut *read(key)?))?,
        },
        // The decrypted key is never written: the connection reads it from memory.
        Bootstrap::Pkcs12 { identity, .. } => identity.clone(),
    });
    Ok(())
}

/// The most links a privileged read follows on an administrator's path, as Linux's `ELOOP` bound.
const MAX_LINKS: usize = 40;

/// The file at `path`, read by a privileged step: a regular file of at most
/// [`crate::tls::MAX_PEM_BYTES`], opened without blocking, so a device or a FIFO is never read.
///
/// The roots — the data root and the layout root — and the state directory belong to the
/// service's account. The path is resolved one component at a time, `..` and every link on the
/// way included, and each directory it passes is judged:
///
/// - **A root or the state directory** that the walk reaches must hold the file directly: the
///   name after it is opened in that directory, held, and a link at the name is refused. A root
///   is held as [`HeldDir::open_root`](crate::nofollow::HeldDir::open_root) holds it (ADR-0021
///   clause 13), the state directory without following a link at its own name (clauses 11
///   and 12). The state directory directly in a root is reached by that name too, and counts as
///   the account's where it is missing.
/// - **A path that goes on beneath one of them** is refused: a directory there could be a link
///   the account put in place.
/// - **Anywhere else** the path is the administrator's, and a link on it is followed, at most
///   [`MAX_LINKS`]. The file is opened by its name in the directory the walk ends in, held.
pub(crate) fn read_privileged(
    path: &Path,
    roots: &[&Path],
    state_dir: Option<&Path>,
) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
    let named = |why: String| format!("cannot read {}: {why}", path.display());
    // The roots as resolved; the state directory by its resolved parent and its own name, not
    // followed, so it counts as the account's even where it is missing or a link.
    let state = state_dir
        .and_then(|dir| dir.parent().zip(dir.file_name()))
        .and_then(|(parent, name)| Some(std::fs::canonicalize(parent).ok()?.join(name)));
    let owned: Vec<(PathBuf, &Path, bool)> = roots
        .iter()
        .filter_map(|root| Some((std::fs::canonicalize(root).ok()?, *root, true)))
        .chain(
            state
                .clone()
                .zip(state_dir)
                .map(|(form, dir)| (form, dir, false)),
        )
        .collect();
    let absolute = std::path::absolute(path).map_err(|e| named(e.to_string()))?;
    let mut pending: std::collections::VecDeque<Part> =
        absolute.components().map(Part::of).collect();
    let mut at = PathBuf::new();
    let mut links = 0;
    while let Some(next) = pending.pop_front() {
        match next {
            Part::Up => {
                at.pop();
            }
            Part::Here => {}
            Part::Name(name) => {
                let here = at.join(&name);
                let meta = std::fs::symlink_metadata(&here).map_err(|e| named(e.to_string()))?;
                if meta.file_type().is_symlink() {
                    links += 1;
                    if links > MAX_LINKS {
                        return Err(named(format!("more than {MAX_LINKS} links on the way")));
                    }
                    let target = std::fs::read_link(&here).map_err(|e| named(e.to_string()))?;
                    if target.is_absolute() {
                        at = PathBuf::new();
                    }
                    for part in target.components().rev() {
                        pending.push_front(Part::of(part));
                    }
                    continue;
                }
                at = here;
            }
            // The root, and on Windows the drive.
            Part::Root(root) => at.push(root),
        }
        if pending.is_empty() {
            break;
        }
        // A directory the walk reached: one of the account's holds the file directly, or not at all.
        let real = std::fs::canonicalize(&at).map_err(|e| named(e.to_string()))?;
        if let Some((_, dir, root)) = owned.iter().find(|(form, _, _)| *form == real) {
            // The state directory directly in a root holds its own files, reached by its name.
            if let (2, Some(Part::Name(sub)), Some(Part::Name(name))) =
                (pending.len(), pending.front(), pending.back())
            {
                if let (Some(dir), true) = (state_dir, state.as_ref() == Some(&real.join(sub))) {
                    return read_in(crate::nofollow::HeldDir::open(dir), name, &named);
                }
            }
            let (1, Some(Part::Name(name))) = (pending.len(), pending.front()) else {
                return Err(named(format!(
                    "it lies beneath {}, which the service's account owns, but not directly in it",
                    dir.display()
                )));
            };
            let held = if *root {
                crate::nofollow::HeldDir::open_root(dir)
            } else {
                crate::nofollow::HeldDir::open(dir)
            };
            return read_in(held, name, &named);
        }
    }
    let (dir, name) = split(&at).map_err(named)?;
    let held = if dir.parent().is_some() {
        crate::nofollow::HeldDir::open(dir)
    } else {
        crate::nofollow::HeldDir::open_root(dir)
    };
    read_in(held, std::ffi::OsStr::new(name), &named)
}

/// One component of a path being resolved, owned, so a link's target can be put in its place.
enum Part {
    Root(std::ffi::OsString),
    Here,
    Up,
    Name(std::ffi::OsString),
}

impl Part {
    fn of(component: std::path::Component<'_>) -> Self {
        use std::path::Component;
        match component {
            Component::Prefix(_) | Component::RootDir => Part::Root(component.as_os_str().into()),
            Component::CurDir => Part::Here,
            Component::ParentDir => Part::Up,
            Component::Normal(name) => Part::Name(name.into()),
        }
    }
}

/// The regular file `name` in `held`, read within the bound, a link at the name refused.
fn read_in(
    held: std::io::Result<crate::nofollow::HeldDir>,
    name: &std::ffi::OsStr,
    named: &impl Fn(String) -> String,
) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
    let name = name
        .to_str()
        .ok_or_else(|| named("its name is not UTF-8".to_string()))?;
    let file = held
        .and_then(|held| held.open_file(name))
        .map_err(|e| named(e.to_string()))?;
    crate::tls::read_opened(file).map_err(named)
}

/// `path` as its directory and its file name, which must be UTF-8.
fn split(path: &Path) -> Result<(&Path, &str), String> {
    let dir = path
        .parent()
        .ok_or_else(|| "it names no file".to_string())?;
    let name = path
        .file_name()
        .ok_or_else(|| "it names no file".to_string())?
        .to_str()
        .ok_or_else(|| "its name is not UTF-8".to_string())?;
    Ok((dir, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A certificate and its key as PEM.
    fn pair() -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["bootstrap".to_string()])
            .expect("params")
            .self_signed(&key)
            .expect("certificate");
        (cert.pem(), key.serialize_pem())
    }

    /// A configuration whose `[tls] ca_file` is `ca`, in a state directory of `dir`.
    fn config(dir: &Path, ca: &Path) -> ClientConfig {
        let mut config = ClientConfig {
            endpoint: "wss://127.0.0.1:9/v1/opamp".to_string(),
            state_dir: dir.join("state"),
            ..ClientConfig::default()
        };
        config.tls = Some(crate::config::TlsConfig {
            ca_file: Some(ca.to_path_buf()),
            cert_file: None,
            key_file: None,
        });
        config
    }

    /// A bootstrap pair written into `dir`.
    fn bootstrap(dir: &Path) -> Bootstrap {
        let (cert, key) = pair();
        let (cert_path, key_path) = (dir.join("bootstrap.pem"), dir.join("bootstrap-key.pem"));
        std::fs::write(&cert_path, cert).expect("cert");
        std::fs::write(&key_path, key).expect("key");
        Bootstrap::Pair {
            cert: cert_path,
            key: key_path,
        }
    }

    /// `run`'s error, which must come before anything connects: a run still going after ten
    /// seconds read something it should have refused.
    fn refusal(config: ClientConfig, bootstrap: Bootstrap, account: PathBuf) -> String {
        let (done, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            done.send(run(&config, &bootstrap, &Written::default(), &[&account]).err())
        });
        wait.recv_timeout(std::time::Duration::from_secs(10))
            .expect("the enrolment read what it should have refused")
            .expect("refused")
    }

    /// A `[tls]` path that names a device is refused, naming it, without reading it: the CA file
    /// and either file of the bootstrap pair alike (HARDENING.md H21).
    /// Verifies: ADR-0021
    #[cfg(unix)]
    #[test]
    fn a_device_a_tls_path_names_is_refused_unread() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let (ca, _) = pair();
        let ca_path = admin.path().join("ca.pem");
        std::fs::write(&ca_path, ca).expect("ca");
        let zero = PathBuf::from("/dev/zero");

        let refused = refusal(
            config(account.path(), &zero),
            bootstrap(admin.path()),
            account.path().to_path_buf(),
        );
        assert!(refused.contains("/dev/zero"), "{refused}");
        assert!(refused.contains("not a regular file"), "{refused}");

        let Bootstrap::Pair { cert, key } = bootstrap(admin.path()) else {
            unreachable!()
        };
        for pair in [
            Bootstrap::Pair {
                cert: zero.clone(),
                key: key.clone(),
            },
            Bootstrap::Pair {
                cert,
                key: zero.clone(),
            },
        ] {
            let refused = refusal(
                config(account.path(), &ca_path),
                pair,
                account.path().to_path_buf(),
            );
            assert!(refused.contains("/dev/zero"), "{refused}");
        }
    }

    /// A `[tls]` file over the bound is refused, naming it.
    /// Verifies: ADR-0021
    #[test]
    fn a_tls_file_over_the_bound_is_refused() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let large = admin.path().join("large.pem");
        let file = std::fs::File::create(&large).expect("large");
        file.set_len(crate::tls::MAX_PEM_BYTES + 1).expect("size");

        let refused = refusal(
            config(account.path(), &large),
            bootstrap(admin.path()),
            account.path().to_path_buf(),
        );
        assert!(refused.contains("large.pem"), "{refused}");
        assert!(refused.contains("at most"), "{refused}");
    }

    /// A link the account placed at the CA copy in the data root is not followed, even to a file
    /// of certificates (HARDENING.md H21).
    /// Verifies: ADR-0021
    #[cfg(unix)]
    #[test]
    fn a_link_at_the_ca_copy_is_not_followed() {
        let account = tempfile::tempdir().expect("account");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let (ca, _) = pair();
        let target = elsewhere.path().join("their-ca.pem");
        std::fs::write(&target, ca).expect("ca");
        let copy = account.path().join(crate::config_init::SERVER_CA_FILE);
        std::os::unix::fs::symlink(&target, &copy).expect("symlink");

        let refused = refusal(
            config(account.path(), &copy),
            bootstrap(elsewhere.path()),
            account.path().to_path_buf(),
        );
        assert!(refused.contains("is a link"), "{refused}");
    }

    /// A file beneath a directory of the account's but not directly in it is refused, naming it,
    /// since a directory between could be a link.
    /// Verifies: ADR-0021
    #[test]
    fn a_tls_path_deeper_in_the_account_s_directory_is_refused() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let (ca, _) = pair();
        let deeper = account.path().join("sub");
        std::fs::create_dir(&deeper).expect("sub");
        std::fs::write(deeper.join("ca.pem"), &ca).expect("ca");

        let refused = refusal(
            config(account.path(), &deeper.join("ca.pem")),
            bootstrap(admin.path()),
            account.path().to_path_buf(),
        );
        assert!(refused.contains("not directly in it"), "{refused}");
        assert!(refused.contains("ca.pem"), "{refused}");
    }

    /// A directory of the root's that the account swapped for a link to a directory elsewhere
    /// carries no read past the root: the path is refused as lying deeper beneath it, as it is
    /// while the directory is a real one.
    /// Verifies: ADR-0021
    #[cfg(unix)]
    #[test]
    fn a_link_in_place_of_a_directory_of_the_root_s_is_not_read_through() {
        let account = tempfile::tempdir().expect("account");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let (ca, _) = pair();
        std::fs::write(elsewhere.path().join("ca.pem"), ca).expect("ca");
        let sub = account.path().join("certs");
        std::os::unix::fs::symlink(elsewhere.path(), &sub).expect("symlink");

        let refused = refusal(
            config(account.path(), &sub.join("ca.pem")),
            bootstrap(elsewhere.path()),
            account.path().to_path_buf(),
        );
        assert!(refused.contains("not directly in it"), "{refused}");

        // Nor spelled with `..` from elsewhere, nor through the administrator's own link to it.
        let around = elsewhere.path().join("..").join(
            account
                .path()
                .file_name()
                .expect("a name")
                .to_string_lossy()
                .as_ref(),
        );
        let admin_link = elsewhere.path().join("certs");
        std::os::unix::fs::symlink(&sub, &admin_link).expect("the administrator's link");
        for spelled in [
            around.join("certs").join("ca.pem"),
            admin_link.join("ca.pem"),
        ] {
            let refused = refusal(
                config(account.path(), &spelled),
                bootstrap(elsewhere.path()),
                account.path().to_path_buf(),
            );
            assert!(
                refused.contains("not directly in it"),
                "{}: {refused}",
                spelled.display()
            );
        }
    }

    /// A file the administrator named outside the account's directories may be reached through a
    /// link, as a distribution's CA bundle often is; where the link leads is judged again, so one
    /// that leads to a link at the CA copy is refused.
    /// Verifies: ADR-0021
    #[cfg(unix)]
    #[test]
    fn an_administrator_s_link_outside_the_account_s_directories_is_followed() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let (ca, _) = pair();
        let bundle = admin.path().join("ca-bundle.crt");
        std::fs::write(&bundle, &ca).expect("ca");
        let link = admin.path().join("ca.pem");
        std::os::unix::fs::symlink(&bundle, &link).expect("symlink");
        let mut followed = config(account.path(), &link);

        read_once(&mut followed, &bootstrap(admin.path()), &[account.path()]).expect("read");
        assert_eq!(followed.ca_pem.as_deref(), Some(ca.as_bytes()));

        // Where the link leads, the rules of the account's directories hold.
        let theirs = admin.path().join("theirs.pem");
        std::fs::write(&theirs, &ca).expect("theirs");
        let copy = account.path().join(crate::config_init::SERVER_CA_FILE);
        std::os::unix::fs::symlink(&theirs, &copy).expect("link at the copy");
        let into = admin.path().join("into.pem");
        std::os::unix::fs::symlink(&copy, &into).expect("link to the copy");
        let refused = refusal(
            config(account.path(), &into),
            bootstrap(admin.path()),
            account.path().to_path_buf(),
        );
        assert!(refused.contains("is a link"), "{refused}");
    }

    /// A state directory the account swapped for a link is not read through: a file named in it
    /// is refused, though the link leads to a plain directory (ADR-0021 clause 12).
    /// Verifies: ADR-0021
    #[cfg(unix)]
    #[test]
    fn a_state_directory_that_is_a_link_is_not_read_through() {
        let account = tempfile::tempdir().expect("account");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let (ca, _) = pair();
        std::fs::write(elsewhere.path().join("ca.pem"), ca).expect("ca");
        let state = account.path().join("state");
        std::os::unix::fs::symlink(elsewhere.path(), &state).expect("symlink");
        let mut config = config(account.path(), &state.join("ca.pem"));
        config.state_dir.clone_from(&state);

        let refused = refusal(
            config,
            bootstrap(elsewhere.path()),
            account.path().to_path_buf(),
        );
        assert!(refused.contains("state/ca.pem"), "{refused}");
        // Refused at the state directory's own name, not as lying deeper beneath the root.
        assert!(!refused.contains("not directly in it"), "{refused}");
    }

    /// A file directly in a real state directory beneath a root, the default layout, is read
    /// through the state directory.
    /// Verifies: ADR-0021
    #[test]
    fn a_file_in_the_state_directory_beneath_a_root_is_read() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let (ca, _) = pair();
        let state = account.path().join("state");
        std::fs::create_dir(&state).expect("state");
        std::fs::write(state.join("ca.pem"), &ca).expect("ca");
        let mut config = config(account.path(), &state.join("ca.pem"));
        config.state_dir.clone_from(&state);

        read_once(&mut config, &bootstrap(admin.path()), &[account.path()]).expect("read");
        assert_eq!(config.ca_pem.as_deref(), Some(ca.as_bytes()));
    }

    /// A path that reaches a root through a link outside it is read as a file of that root: a
    /// link at the CA copy is refused however the path spells the directory.
    /// Verifies: ADR-0021
    #[cfg(unix)]
    #[test]
    fn a_path_through_a_link_to_a_root_is_held_to_the_root() {
        let account = tempfile::tempdir().expect("account");
        let elsewhere = tempfile::tempdir().expect("elsewhere");
        let (ca, _) = pair();
        let target = elsewhere.path().join("their-ca.pem");
        std::fs::write(&target, ca).expect("ca");
        std::os::unix::fs::symlink(&target, account.path().join("server-ca.pem")).expect("link");
        let around = elsewhere.path().join("around");
        std::os::unix::fs::symlink(account.path(), &around).expect("symlink");

        let refused = refusal(
            config(account.path(), &around.join("server-ca.pem")),
            bootstrap(elsewhere.path()),
            account.path().to_path_buf(),
        );
        assert!(refused.contains("is a link"), "{refused}");
    }

    /// What the enrolment connects with is read once, into memory: once read, the files can go
    /// and the connection's TLS is still built.
    /// Verifies: ADR-0021
    #[test]
    fn the_enrolment_s_trust_and_identity_are_read_once_into_memory() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let (ca, _) = pair();
        let copy = account.path().join(crate::config_init::SERVER_CA_FILE);
        std::fs::write(&copy, &ca).expect("ca");
        let pair = bootstrap(admin.path());
        let mut config = config(account.path(), &copy);

        read_once(&mut config, &pair, &[account.path()]).expect("read");
        assert_eq!(config.ca_pem.as_deref(), Some(ca.as_bytes()));
        assert!(config.bootstrap_identity.is_some());

        std::fs::remove_file(&copy).expect("remove the copy");
        let Bootstrap::Pair { cert, key } = pair else {
            unreachable!()
        };
        std::fs::remove_file(cert).expect("remove the cert");
        std::fs::remove_file(key).expect("remove the key");
        let tls = crate::tls::client_tls(&config).expect("built from memory");
        assert_eq!(tls.ca_pem.as_deref(), Some(ca.as_bytes()));
        assert!(tls.identity.is_some());
    }

    /// The run that answered enrols with its answers: a `supervisor.toml` rewritten after the
    /// write, to another endpoint and another CA, changes neither the endpoint the enrolment
    /// connects to nor the trust it connects with, and the rewritten CA is not read at all.
    /// Verifies: ADR-draft-a-privileged-enrolment-connects-where-its-own-run-answered
    #[test]
    fn a_run_that_answered_enrols_with_its_answers_whatever_the_file_says_by_now() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let (answered_ca, _) = pair();
        let copy = account.path().join(crate::config_init::SERVER_CA_FILE);
        let answered = Answered {
            endpoint: "wss://fleet.example.com/v1/opamp".to_string(),
            ca: Some((copy.clone(), answered_ca.clone().into_bytes())),
        };
        // What the account rewrote: another endpoint, and a CA that is no file to read.
        let mut rewritten = config(account.path(), Path::new("/dev/zero"));
        rewritten.endpoint = "wss://theirs.example.net/v1/opamp".to_string();

        let prepared = prepare(
            &rewritten,
            &bootstrap(admin.path()),
            &Written {
                answered: Some(answered.clone()),
                ..Written::default()
            },
            &[account.path()],
        )
        .expect("prepared from the answers");
        assert_eq!(prepared.endpoint, answered.endpoint);
        assert_eq!(prepared.ca_file(), Some(copy.as_path()));
        let tls = crate::tls::client_tls(&prepared).expect("built");
        assert_eq!(tls.ca_pem.as_deref(), Some(answered_ca.as_bytes()));

        // An answer without a CA leaves the built-in roots, whatever the file names.
        let without = Answered {
            ca: None,
            ..answered.clone()
        };
        let prepared = prepare(
            &rewritten,
            &bootstrap(admin.path()),
            &Written {
                answered: Some(without),
                ..Written::default()
            },
            &[account.path()],
        )
        .expect("prepared from the answers");
        assert_eq!(prepared.ca_file(), None);
        assert_eq!(
            crate::tls::client_tls(&prepared).expect("built").ca_pem,
            None
        );
    }

    /// A run that answered nothing takes the endpoint and the CA from the file: the residual the
    /// decision names.
    /// Verifies: ADR-draft-a-privileged-enrolment-connects-where-its-own-run-answered
    #[test]
    fn a_run_that_answered_nothing_enrols_with_what_the_file_says() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let (ca, _) = pair();
        let copy = account.path().join(crate::config_init::SERVER_CA_FILE);
        std::fs::write(&copy, &ca).expect("ca");
        let mut file = config(account.path(), &copy);
        file.endpoint = "wss://in-the-file.example.com/v1/opamp".to_string();

        let prepared = prepare(
            &file,
            &bootstrap(admin.path()),
            &Written::default(),
            &[account.path()],
        )
        .expect("prepared from the file");
        assert_eq!(prepared.endpoint, file.endpoint);
        assert_eq!(prepared.ca_pem.as_deref(), Some(ca.as_bytes()));
    }

    /// `--endpoint` over a kept file that names another endpoint refuses the enrolment, naming
    /// both; where the two are the same, the run enrols with what the file says.
    /// Verifies: ADR-draft-a-privileged-enrolment-connects-where-its-own-run-answered
    #[test]
    fn an_endpoint_named_over_a_kept_file_that_names_another_refuses_the_enrolment() {
        let account = tempfile::tempdir().expect("account");
        let admin = tempfile::tempdir().expect("admin");
        let (ca, _) = pair();
        let copy = account.path().join(crate::config_init::SERVER_CA_FILE);
        std::fs::write(&copy, &ca).expect("ca");
        let mut kept = config(account.path(), &copy);
        kept.endpoint = "wss://theirs.example.net/v1/opamp".to_string();
        let named = "wss://fleet.example.com/v1/opamp";
        let written = Written {
            named_over_kept: Some(named.to_string()),
            ..Written::default()
        };

        let refused = prepare(&kept, &bootstrap(admin.path()), &written, &[account.path()])
            .expect_err("another endpoint");
        assert!(refused.contains(named), "{refused}");
        assert!(
            refused.contains("wss://theirs.example.net/v1/opamp"),
            "{refused}"
        );

        kept.endpoint = named.to_string();
        let prepared = prepare(&kept, &bootstrap(admin.path()), &written, &[account.path()])
            .expect("the same endpoint");
        assert_eq!(prepared.endpoint, named);
        assert_eq!(prepared.ca_pem.as_deref(), Some(ca.as_bytes()));
    }
}
