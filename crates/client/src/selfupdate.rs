//! The Client replacing its own binary (ADR-0020).
//!
//! Updating a Managed Process is done by a Supervisor that outlives it (ADR-0015): stop, swap,
//! restart, watch, roll back. Nothing here outlives anything — the process that installs the
//! package is the process that has to exit for the install to take effect. So the work is split
//! across the restart, and this module owns both halves:
//!
//! **Before.** [`install`] stages the new version *beside* the running one in the ADR-0010 layout,
//! proves it by running it (`client self-check`), records an [`UpdateMarker`], and moves the
//! `current` pointer. Nothing is overwritten, so the version being replaced is still on disk and
//! the rollback is a pointer move rather than a download.
//!
//! **After.** [`on_start`] runs before anything else in a fresh process. It finds the marker the
//! previous process left, counts the attempt, and either lets the run proceed on probation — until
//! [`commit`] declares the new version good — or gives up and points `current` back.
//!
//! The probe and the counter cover different failures and neither covers both. A binary that
//! cannot exec at all never reaches [`on_start`] to count anything, which is why it is proved
//! before the pointer moves; a binary that starts and then will not stay up cannot be caught by a
//! probe that only watched it print a line, which is why the counter exists.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::service::layout::{self, Layout, BINARY_FILENAME};

/// The file [`install`] leaves for the next process to find, in the Client's state directory —
/// which survives the version switch because it is outside `versions/`.
const MARKER_FILE: &str = "update-marker.json";

/// How many starts a new version gets to [`commit`] itself before it is rolled back.
///
/// Each attempt costs the service manager's restart delay, so this is a handful of seconds of
/// crash-looping and not more. One would be too few — a host under load can lose a start to
/// something unrelated to the new binary — and many would leave a fleet crash-looping on a broken
/// version for minutes while it counted.
const MAX_ATTEMPTS: u32 = 3;

/// What `client self-check` prints. A package can be offered under the configured name and still
/// be some other program; this is what only this program answers.
pub const SELF_CHECK_TOKEN: &str = "opamp-fleet-client self-check ok version=";

/// The exit code that asks the service manager for a restart (ADR-0020). Non-zero on purpose:
/// "restart on failure" is what all three managers offer, and there is no "restart on success".
pub const EXIT_RESTART_FOR_UPDATE: i32 = 10;

/// What the process that switched the pointer tells the one that comes after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateMarker {
    /// The version directory that was current before the switch — where a rollback points back.
    pub previous_dir: PathBuf,
    /// The version directory now current.
    pub new_dir: PathBuf,
    /// The version the new directory holds, as the Server offered it.
    pub version: String,
    /// The offered package hash, hex-encoded: the status reported after the restart has to name
    /// the package it is about, and the Agent state machine is gone by then.
    pub package_hash_hex: String,
    /// How many times a process has started and found this marker.
    pub attempts: u32,
}

/// What [`on_start`] found, and what the run should therefore do.
#[derive(Debug, PartialEq, Eq)]
pub enum Startup {
    /// No update in flight; an ordinary run.
    Ordinary,
    /// This process is the new version on probation. It must [`commit`] once it is up.
    OnProbation(Box<UpdateMarker>),
    /// The new version used up its attempts. `current` has been pointed back and this process
    /// must exit so the manager starts the version it now names.
    RolledBack(Box<UpdateMarker>),
    /// An update finished and the outcome is owed to the Server — either the new version
    /// committing, or the old one reporting why it is back.
    Outcome(Box<UpdateOutcome>),
}

/// The terminal status a restarted Client owes the Server (ADR-0020): the install necessarily
/// completes in a different process than the one that started it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateOutcome {
    pub version: String,
    pub package_hash_hex: String,
    /// `None` is `Installed`; `Some(reason)` is `InstallFailed`.
    pub error: Option<String>,
}

fn marker_path(state_dir: &Path) -> PathBuf {
    state_dir.join(MARKER_FILE)
}

fn load_marker(state_dir: &Path) -> Option<UpdateMarker> {
    let text = std::fs::read_to_string(marker_path(state_dir)).ok()?;
    match serde_json::from_str(&text) {
        Ok(marker) => Some(marker),
        Err(e) => {
            // A marker we cannot read is worse than none: it would keep a Client on probation
            // forever. Drop it and carry on with whatever is current.
            warn!(error = %e, "the update marker is unreadable; ignoring it");
            let _ = std::fs::remove_file(marker_path(state_dir));
            None
        }
    }
}

fn store_marker(state_dir: &Path, marker: &UpdateMarker) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(marker).expect("an UpdateMarker serializes");
    std::fs::write(marker_path(state_dir), json)
        .map_err(|e| format!("cannot write the update marker: {e}"))
}

/// Installs a verified artifact as a new version of *this Client* and points `current` at it
/// (ADR-0020). The caller reports `Installing` and then ends the run with
/// [`EXIT_RESTART_FOR_UPDATE`]; the terminal status comes from the process that starts next.
///
/// # Errors
/// Returns an error — with the previous version still current and still running — when this
/// executable is not in an ADR-0010 layout, when the artifact cannot be staged, or when the staged
/// binary does not answer for itself.
pub fn install(
    state_dir: &Path,
    artifact: &Path,
    version: &str,
    package_hash: &[u8],
    archive_key: Option<&str>,
) -> Result<UpdateMarker, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    // Outside the versioned layout there is no pointer to switch and no previous version to go
    // back to — a `cargo run` build, or a binary an operator dropped somewhere by hand.
    let (layout, running_dir) = Layout::enclosing(&exe).ok_or_else(|| {
        format!(
            "this Client does not run from a versioned install layout ({}); \
             self-update needs `client service install` (ADR-0010)",
            exe.display()
        )
    })?;

    let new_dir = layout.version_dir(&layout::version_dir_name(version));
    if new_dir == running_dir {
        return Err(format!(
            "the offered version {version:?} is the one already running"
        ));
    }
    stage(&new_dir, artifact, version, archive_key)?;

    // Prove it before anything points at it.
    probe(&new_dir.join(BINARY_FILENAME), version)?;

    let marker = UpdateMarker {
        previous_dir: running_dir,
        new_dir: new_dir.clone(),
        version: version.to_string(),
        package_hash_hex: hex::encode(package_hash),
        attempts: 0,
    };
    // The marker is written *before* the switch: a crash between the two leaves a marker naming a
    // switch that did not happen, which the next start resolves by pointing at what it names.
    store_marker(state_dir, &marker)?;
    layout.set_current(&new_dir)?;
    info!(version = %version, dir = %new_dir.display(), "staged a new Client version; restarting into it");
    Ok(marker)
}

/// Unpacks the artifact into `dir` as this platform's binary and writes the ADR-0010 manifest.
fn stage(
    dir: &Path,
    artifact: &Path,
    version: &str,
    archive_key: Option<&str>,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let binary = dir.join(BINARY_FILENAME);
    let mut out = std::fs::File::create(&binary)
        .map_err(|e| format!("cannot write {}: {e}", binary.display()))?;

    // The artifact is the program or an archive holding it (ADR-0018) — the same shapes a Managed
    // Process's package comes in, decided by the leading bytes rather than a file name.
    match crate::archive::detect(artifact)? {
        crate::archive::Kind::Raw => {
            let mut source = std::fs::File::open(artifact)
                .map_err(|e| format!("cannot read {}: {e}", artifact.display()))?;
            std::io::copy(&mut source, &mut out)
                .map_err(|e| format!("cannot write {}: {e}", binary.display()))?;
        }
        kind @ (crate::archive::Kind::TarGz | crate::archive::Kind::SevenZ) => {
            let written = match kind {
                crate::archive::Kind::SevenZ => {
                    crate::archive::extract_7z(artifact, BINARY_FILENAME, &mut out, archive_key)?
                }
                _ => crate::archive::extract_tar_gz(artifact, BINARY_FILENAME, &mut out)?,
            };
            info!(archive = %artifact.display(), bytes = written, "unpacked the Client archive");
        }
    }
    drop(out);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot make {} executable: {e}", binary.display()))?;
    }

    let bytes =
        std::fs::read(&binary).map_err(|e| format!("cannot read {}: {e}", binary.display()))?;
    let manifest = format!(
        "# Written by a self-update (ADR-0020).\nversion = \"{version}\"\nsha256 = \"{}\"\n",
        hex::encode(Sha256::digest(&bytes))
    );
    std::fs::write(dir.join("manifest.toml"), manifest)
        .map_err(|e| format!("cannot write the manifest: {e}"))
}

/// Runs `<binary> self-check`, retrying briefly past `ETXTBSY`.
///
/// The binary was written moments ago, and exec of a freshly written file fails with "Text file
/// busy" while any process still holds it open for writing — including a child another thread of
/// this Client forked for its own spawn, which inherits the descriptor until it execs. The same
/// race the Supervisor already handles after a package swap, and it says nothing about the
/// artifact: failing the update over it would refuse a binary that is perfectly good.
fn run_self_check(binary: &Path) -> std::io::Result<std::process::Output> {
    const BUSY_RETRIES: u32 = 10;
    const BUSY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

    let mut attempt = 0;
    loop {
        match std::process::Command::new(binary)
            .arg("self-check")
            .output()
        {
            Err(e) if is_text_file_busy(&e) && attempt < BUSY_RETRIES => {
                attempt += 1;
                warn!(
                    binary = %binary.display(), attempt,
                    "the staged binary is momentarily busy; retrying the self-check"
                );
                std::thread::sleep(BUSY_DELAY);
            }
            other => return other,
        }
    }
}

/// `ETXTBSY`: the file cannot be executed because someone holds it open for writing.
fn is_text_file_busy(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ETXTBSY)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

/// Runs the staged binary and requires it to answer for itself at the expected version.
fn probe(binary: &Path, expected_version: &str) -> Result<(), String> {
    let output = run_self_check(binary)
        .map_err(|e| format!("the staged binary {} does not run: {e}", binary.display()))?;
    if !output.status.success() {
        return Err(format!(
            "the staged binary answered the self-check with {}",
            output.status
        ));
    }
    let answer = String::from_utf8_lossy(&output.stdout);
    let Some(reported) = answer
        .lines()
        .find_map(|line| line.trim().strip_prefix(SELF_CHECK_TOKEN))
    else {
        return Err(
            "the staged binary is not an OpAMP Fleet Client: it did not answer the self-check"
                .to_string(),
        );
    };
    if reported.trim() != expected_version {
        return Err(format!(
            "the staged binary reports version {:?}, but the package offered {expected_version:?}",
            reported.trim()
        ));
    }
    Ok(())
}

/// What this process should do about an update in flight — called once, before the daemon body.
///
/// # Errors
/// Returns an error only when a rollback is needed and the pointer cannot be moved, which leaves
/// the host running a version its own marker says is failing.
pub fn on_start(state_dir: &Path) -> Result<Startup, String> {
    let Some(mut marker) = load_marker(state_dir) else {
        // Nothing in flight — but an outcome may still be owed from the run that finished one.
        return Ok(
            load_outcome(state_dir).map_or(Startup::Ordinary, |o| Startup::Outcome(o.into()))
        );
    };
    marker.attempts += 1;

    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    let running_dir = Layout::enclosing(&exe).map(|(_, dir)| dir);
    let is_new_version = running_dir.as_deref().is_some_and(|dir| {
        std::fs::canonicalize(dir).ok() == std::fs::canonicalize(&marker.new_dir).ok()
    });

    if !is_new_version {
        // We are the *old* version and the marker is still here: the switch never took effect, or
        // a rollback already pointed back at us. Either way the update did not happen.
        let reason = format!(
            "the new version {} did not take over; running {} instead",
            marker.version,
            running_dir
                .as_deref()
                .unwrap_or(Path::new("an unknown directory"))
                .display()
        );
        warn!(reason, "a self-update did not take effect");
        clear(state_dir);
        store_outcome(
            state_dir,
            &UpdateOutcome {
                version: marker.version.clone(),
                package_hash_hex: marker.package_hash_hex.clone(),
                error: Some(reason),
            },
        );
        return Ok(Startup::Outcome(
            load_outcome(state_dir).expect("just written").into(),
        ));
    }

    if marker.attempts > MAX_ATTEMPTS {
        let reason = format!(
            "the new version {} did not stay up after {MAX_ATTEMPTS} attempts",
            marker.version
        );
        warn!(reason, "rolling the Client back to its previous version");
        let (layout, _) = Layout::enclosing(&exe).expect("checked above");
        layout.set_current(&marker.previous_dir)?;
        clear(state_dir);
        store_outcome(
            state_dir,
            &UpdateOutcome {
                version: marker.version.clone(),
                package_hash_hex: marker.package_hash_hex.clone(),
                error: Some(reason),
            },
        );
        return Ok(Startup::RolledBack(marker.into()));
    }

    store_marker(state_dir, &marker)?;
    info!(
        version = %marker.version,
        attempt = marker.attempts,
        "running a freshly installed Client version on probation"
    );
    Ok(Startup::OnProbation(marker.into()))
}

/// Declares the version this process runs good: the marker goes, and the Server is owed
/// `Installed`. Called once the Client is up and has reached the Server.
pub fn commit(state_dir: &Path, marker: &UpdateMarker) {
    clear(state_dir);
    store_outcome(
        state_dir,
        &UpdateOutcome {
            version: marker.version.clone(),
            package_hash_hex: marker.package_hash_hex.clone(),
            error: None,
        },
    );
    info!(version = %marker.version, "the new Client version committed itself");
}

fn clear(state_dir: &Path) {
    let _ = std::fs::remove_file(marker_path(state_dir));
}

const OUTCOME_FILE: &str = "update-outcome.json";

fn store_outcome(state_dir: &Path, outcome: &UpdateOutcome) {
    let json = serde_json::to_vec_pretty(outcome).expect("an UpdateOutcome serializes");
    if let Err(e) = std::fs::write(state_dir.join(OUTCOME_FILE), json) {
        warn!(error = %e, "cannot record the self-update outcome; the Server will not hear it");
    }
}

fn load_outcome(state_dir: &Path) -> Option<UpdateOutcome> {
    let text = std::fs::read_to_string(state_dir.join(OUTCOME_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Drops the recorded outcome once it has been reported to the Server.
pub fn clear_outcome(state_dir: &Path) {
    let _ = std::fs::remove_file(state_dir.join(OUTCOME_FILE));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(dir: &Path, attempts: u32) -> UpdateMarker {
        UpdateMarker {
            previous_dir: dir.join("versions/opamp-client-1.0.0-aaaaaaa"),
            new_dir: dir.join("versions/opamp-client-2.0.0-bbbbbbb"),
            version: "2.0.0".to_string(),
            package_hash_hex: "abcd".to_string(),
            attempts,
        }
    }

    #[test]
    fn a_marker_round_trips_and_an_unreadable_one_is_dropped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let written = marker(dir.path(), 1);
        store_marker(dir.path(), &written).expect("store");
        assert_eq!(load_marker(dir.path()), Some(written));

        // A marker we cannot parse would keep a Client on probation forever.
        std::fs::write(marker_path(dir.path()), "not json").expect("corrupt");
        assert_eq!(load_marker(dir.path()), None);
        assert!(
            !marker_path(dir.path()).exists(),
            "the unreadable marker is removed, not left to be re-read"
        );
    }

    #[test]
    fn an_ordinary_start_finds_nothing_to_do() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(on_start(dir.path()).expect("start"), Startup::Ordinary);
    }

    #[test]
    fn committing_replaces_the_marker_with_an_installed_outcome() {
        let dir = tempfile::tempdir().expect("tempdir");
        let marker = marker(dir.path(), 1);
        store_marker(dir.path(), &marker).expect("store");

        commit(dir.path(), &marker);
        assert!(!marker_path(dir.path()).exists(), "no longer on probation");
        let outcome = load_outcome(dir.path()).expect("an outcome is owed");
        assert_eq!(outcome.version, "2.0.0");
        assert_eq!(outcome.error, None, "committing reports Installed");

        // The next start reports it, then it is gone.
        assert!(matches!(
            on_start(dir.path()).expect("start"),
            Startup::Outcome(_)
        ));
        clear_outcome(dir.path());
        assert_eq!(on_start(dir.path()).expect("start"), Startup::Ordinary);
    }

    /// The old version finding the marker means the switch never took hold — reported as a
    /// failure rather than silently forgotten, since the Server was told `Installing`.
    #[test]
    fn the_old_version_finding_a_marker_reports_the_update_as_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        // `new_dir` is a path this test binary certainly does not run from.
        store_marker(dir.path(), &marker(dir.path(), 0)).expect("store");

        let startup = on_start(dir.path()).expect("start");
        let Startup::Outcome(outcome) = startup else {
            panic!("expected an owed outcome, got {startup:?}");
        };
        assert_eq!(outcome.version, "2.0.0");
        assert!(
            outcome
                .error
                .as_deref()
                .is_some_and(|e| e.contains("did not take over")),
            "got {:?}",
            outcome.error
        );
        assert!(!marker_path(dir.path()).exists());
    }

    /// A binary that cannot run at all is the failure class no post-restart mechanism can catch,
    /// because it never gets far enough to count an attempt. Refusing it is the whole reason the
    /// probe happens before the pointer moves.
    #[test]
    fn the_probe_refuses_a_binary_that_cannot_run() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("not-there");
        assert!(probe(&missing, "2.0.0").is_err());
    }

    /// A program that runs happily and answers something else: offered under the configured
    /// package name, and still not this Client. Unix-only because the impostor is a shell script;
    /// what it proves is platform-independent.
    #[cfg(unix)]
    #[test]
    fn the_probe_refuses_a_binary_that_is_not_this_client() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("impostor");
        std::fs::write(&binary, "#!/bin/sh\necho hello\n").expect("write");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let err = probe(&binary, "2.0.0").expect_err("an impostor must be refused");
        assert!(err.contains("not an OpAMP Fleet Client"), "got {err}");
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_refuses_a_client_of_the_wrong_version() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("client");
        std::fs::write(
            &binary,
            format!("#!/bin/sh\necho '{SELF_CHECK_TOKEN}1.0.0'\n"),
        )
        .expect("write");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let err = probe(&binary, "2.0.0").expect_err("a version mismatch must be refused");
        assert!(err.contains("reports version"), "got {err}");
        // The same binary at the version it claims is accepted.
        probe(&binary, "1.0.0").expect("the versions agree");
    }
}
