//! The filesystem adapters of the Server's stores (ADR-0006), and the two moves every one of them
//! makes: a directory only its owner can enter, and a file replaced in one step, so a crash
//! mid-write leaves the old document rather than half a new one.

mod agents;
mod audit;
mod configs;
mod deployments;
mod labels;
mod packages;
mod revocation;

pub use agents::FsAgentStore;
pub use audit::{files_in as audit_files, FsAuditStore};
pub use configs::FsConfigBackend;
pub use deployments::FsDeploymentBackend;
pub use labels::FsLabelStore;
pub use packages::FsPackageBackend;
pub use revocation::FsLedgerStore;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Creates `dir` if it is missing and, on Unix, restricts it to its owner (`0700`) — whatever it
/// holds may carry credentials, and other local users on the Server host have no business there.
pub fn create_private_dir(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
    }
    Ok(())
}

/// Replaces `path` with `bytes` by writing `<path>.tmp` beside it and renaming it into place.
pub fn replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = temp_of(path);
    std::fs::write(&temp, bytes).map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
    rename_into_place(&temp, path)
}

/// [`replace`], with the file owner-only (`0600`) on Unix. The mode is set in the open call rather
/// than after the write, so the content is never briefly readable by another local user, and the
/// rename carries the mode with it.
pub fn replace_owner_only(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = temp_of(path);
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        file.write_all(bytes)
            .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&temp, bytes)
            .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
    }
    rename_into_place(&temp, path)
}

fn temp_of(path: &Path) -> PathBuf {
    let mut temp = OsString::from(path.as_os_str());
    temp.push(".tmp");
    PathBuf::from(temp)
}

fn rename_into_place(temp: &Path, path: &Path) -> Result<(), String> {
    std::fs::rename(temp, path).map_err(|e| format!("cannot persist {}: {e}", path.display()))
}
