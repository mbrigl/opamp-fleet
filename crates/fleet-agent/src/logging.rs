//! The Client's own log on disk, while it runs as a service (ADR-0019).
//!
//! The Windows SCM discards a service's stderr, so a Client installed there had no readable log at
//! all; systemd and launchd do capture it, and the file is written on those platforms too so that
//! "where are the logs" has one answer everywhere — including in a container, where neither is
//! present. In the foreground nothing is written: somebody is reading stderr there.
//!
//! This is not the OTLP own-logs bridge (ADR-0016) under another name. That one needs a Server that
//! is already reachable, which is exactly what a bad `supervisor.toml`, an unusable certificate, or a
//! refused endpoint is not.
//!
//! **The destination is not known when logging starts.** `tracing` takes one subscriber per process
//! and `main` installs it before the command line is parsed, so the instance — and with it the state
//! directory — does not exist yet. Rather than a second reloadable layer, the file layer is
//! installed from the start with [`LogFile`] as its writer, which discards everything until
//! [`open`] hands it somewhere to write. Events logged before that still reach stderr.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::writer::{EitherWriter, MakeWriter};

/// The base name of the log file; the rotation appends the date.
const FILE_STEM: &str = "supervisor";
const FILE_SUFFIX: &str = "log";

/// The open file, once there is one. Set at most once per process.
static FILE: OnceLock<NonBlocking> = OnceLock::new();

/// The non-blocking writer's worker lives as long as the process: dropping this guard flushes and
/// stops it, so it is parked here rather than returned to a caller who would have to hold it.
static GUARD: OnceLock<WorkerGuard> = OnceLock::new();

/// The `fmt` layer's writer: the rotating file once [`open`] succeeded, and a sink until then.
///
/// A sink rather than an error, because this writer is consulted for every event from the first
/// line of `main` — long before there is an instance, a configuration, or a directory to write in.
pub struct LogFile;

impl<'a> MakeWriter<'a> for LogFile {
    type Writer = EitherWriter<NonBlocking, std::io::Sink>;

    fn make_writer(&'a self) -> Self::Writer {
        match FILE.get() {
            Some(file) => EitherWriter::A(file.clone()),
            None => EitherWriter::B(std::io::sink()),
        }
    }
}

/// Starts writing the rotating log file in `dir`, keeping `keep` days.
///
/// Returns the directory in use, for the one line that tells an operator where to look. Calling it
/// a second time is a no-op: one process writes one log.
///
/// `keep` is a bound the caller cannot escape — `ClientConfig` refuses `0` at load rather than
/// reading it as "keep everything", which is the setting that fills a disk on a host nobody
/// watches.
pub fn open(dir: &Path, keep: usize) -> Result<PathBuf, String> {
    if FILE.get().is_some() {
        return Ok(dir.to_path_buf());
    }
    keep_private(dir)
        .map_err(|e| format!("cannot create the log directory {}: {e}", dir.display()))?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_STEM)
        .filename_suffix(FILE_SUFFIX)
        .max_log_files(keep.max(1))
        .build(dir)
        .map_err(|e| format!("cannot open the log file in {}: {e}", dir.display()))?;
    let (file, guard) = tracing_appender::non_blocking(appender);
    let _ = FILE.set(file);
    let _ = GUARD.set(guard);
    Ok(dir.to_path_buf())
}

/// Creates the log directory owner-only, `0700`, and narrows one that exists, with the log files
/// already in it, to the Client alone: `0700` and `0600` (ADR-0059 clause 6). The state directory
/// it hangs off is passed through by the process account's group where one is registered, and the
/// log is never among what that group reads. The appender creates each day's file at the umask; the
/// directory is what keeps it.
///
/// A file there with a second hard link, or anything that is not a regular file, is left alone.
fn keep_private(dir: &Path) -> std::io::Result<()> {
    crate::storage::create_private_dir(dir)?;
    #[cfg(unix)]
    for entry in std::fs::read_dir(dir)? {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(entry.path())
        else {
            continue;
        };
        let meta = file.metadata()?;
        if meta.is_file() && meta.nlink() == 1 && meta.mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(meta.mode() & 0o700))?;
        }
    }
    Ok(())
}

/// Where the log goes for a given state directory, unless `[logging] dir` names somewhere else.
///
/// The state directory is the right home: it survives a self-update and `uninstall` deliberately
/// does not delete it (ADR-0019), so a log explaining a failed install is still there afterwards.
pub fn default_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Until a directory is named, the writer swallows everything rather than failing: it is
    /// consulted from the first line of `main`, before any instance exists.
    /// Verifies: ADR-0019
    #[test]
    fn the_writer_discards_until_a_file_is_opened() {
        // `FILE` is process-global and another test may have opened it, so this asserts the
        // mapping rather than the global's state.
        let writer = LogFile.make_writer();
        match (FILE.get(), writer) {
            (None, EitherWriter::B(_)) | (Some(_), EitherWriter::A(_)) => {}
            _ => panic!("the writer must follow whether a file is open"),
        }
    }

    /// Verifies: ADR-0019
    #[test]
    fn the_log_directory_hangs_off_the_state_directory() {
        assert_eq!(
            default_dir(Path::new("/var/lib/opamp/state")),
            Path::new("/var/lib/opamp/state/logs")
        );
    }

    /// The rotation is what bounds the log, so opening has to actually produce a writable file in
    /// a directory it creates.
    #[test]
    fn opening_creates_the_directory_and_a_writable_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = dir.path().join("nested").join("logs");
        let used = open(&logs, 7).expect("open");
        assert_eq!(used, logs);
        assert!(logs.is_dir(), "the directory is created");

        // The appender names the file after the stem and the day; writing through the layer is
        // covered by the integration test, which drives a real run.
        let appender = RollingFileAppender::builder()
            .rotation(Rotation::DAILY)
            .filename_prefix(FILE_STEM)
            .filename_suffix(FILE_SUFFIX)
            .max_log_files(7)
            .build(&logs)
            .expect("appender");
        use std::io::Write as _;
        let mut appender = appender;
        appender.write_all(b"line\n").expect("write");
        appender.flush().expect("flush");
        let written: Vec<_> = std::fs::read_dir(&logs)
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            written
                .iter()
                .any(|n| n.starts_with(FILE_STEM) && n.ends_with(FILE_SUFFIX)),
            "the file is named after the stem and the day: {written:?}"
        );
    }

    /// The log is the Client's alone (ADR-0059 clause 6): its directory is made `0700`, and one
    /// that exists — made at the umask by an earlier version — is narrowed to `0700` with the log
    /// files in it `0600`, so a process account that passes through the state directory reads no
    /// line of it.
    /// Verifies: ADR-0059
    #[cfg(unix)]
    #[test]
    fn the_log_directory_and_its_files_are_the_clients_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o7777;
        let dir = tempfile::tempdir().expect("tempdir");

        let fresh = dir.path().join("fresh").join("logs");
        keep_private(&fresh).expect("created");
        assert_eq!(mode(&fresh), 0o700);

        let earlier = dir.path().join("earlier");
        std::fs::create_dir(&earlier).expect("mkdir");
        std::fs::set_permissions(&earlier, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let log = earlier.join("supervisor.2026-10-21.log");
        std::fs::write(&log, b"line\n").expect("write");
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        keep_private(&earlier).expect("narrowed");
        assert_eq!(mode(&earlier), 0o700);
        assert_eq!(mode(&log), 0o600);
    }
}
