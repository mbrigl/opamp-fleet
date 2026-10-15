//! The filesystem adapters of the Server's stores (ADR-0006), and the two moves every one of them
//! makes: a directory only its owner can enter, and a file replaced in one step, so a crash
//! mid-write leaves the old document rather than half a new one.

mod agents;
mod audit;
mod bootstrap;
mod configs;
mod deployments;
mod labels;
mod packages;
mod revocation;

pub use agents::FsAgentStore;
pub use audit::{files_in as audit_files, FsAuditStore};
pub use bootstrap::FsBootstrapStore;
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

/// [`replace`], with the file owner-only (`0600`) on Unix, and durable: the content reaches the
/// disk before the rename makes it the document, and on Unix the rename reaches it before this
/// returns, so a power loss leaves the old document or the new one, never an empty file or a
/// revocation lost after it was answered. The mode is set in the open call rather than after the
/// write, so the content is never briefly readable by another local user, and the rename carries
/// the mode with it.
pub fn replace_owner_only(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let temp = temp_of(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temp)
        .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
    drop(file);
    rename_into_place(&temp, path)?;
    sync_dir_of(path)
}

/// Makes a rename into `path`'s directory durable by syncing the directory itself.
#[cfg(unix)]
fn sync_dir_of(path: &Path) -> Result<(), String> {
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| format!("cannot persist {}: {e}", path.display()))
}

/// Windows opens no directory as a file to sync, so the rename is left to the filesystem.
#[cfg(not(unix))]
fn sync_dir_of(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn temp_of(path: &Path) -> PathBuf {
    let mut temp = OsString::from(path.as_os_str());
    temp.push(".tmp");
    PathBuf::from(temp)
}

fn rename_into_place(temp: &Path, path: &Path) -> Result<(), String> {
    std::fs::rename(temp, path).map_err(|e| format!("cannot persist {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A durable replace still replaces: the new content under the old name, nothing left beside
    /// it, and on Unix only the owner may read it. That it reached the disk before a power loss is
    /// not observable from a test.
    #[test]
    fn replace_owner_only_replaces_the_document() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("revocations.json");
        replace_owner_only(&path, b"old").expect("first write");
        replace_owner_only(&path, b"new").expect("replace");
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
        assert!(
            !temp_of(&path).exists(),
            "the temporary file was left behind"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
