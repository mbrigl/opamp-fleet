//! The versioned-install layout and the atomic `current` pointer (ADR-0007).
//!
//! Every version lives side by side under `versions/<sha256>/`; a stable `current` pointer selects
//! the active one, and the OS service runs `current/supervisor-host`. Switching versions — up or
//! back — is repointing `current`, never overwriting a running binary. The pointer is a symlink on
//! Unix and a directory junction on Windows (a junction needs no symlink privilege). Shared state
//! (Instance UID, effective configuration, the health file) lives at the layout root, outside any
//! versioned directory, so a rollback keeps identity and configuration.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The binary's filename within a version directory.
pub const BINARY_FILENAME: &str = if cfg!(windows) {
    "supervisor-host.exe"
} else {
    "supervisor-host"
};

/// The on-disk layout rooted at the state/install directory.
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// Create a layout rooted at `root` (the Supervisor Host's state directory).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory holding all side-by-side versions.
    #[must_use]
    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    /// The directory for the version identified by `hash`.
    #[must_use]
    pub fn version_dir(&self, hash: &str) -> PathBuf {
        self.versions_dir().join(hash)
    }

    /// The binary path inside a version directory.
    #[must_use]
    pub fn binary_in(version_dir: &Path) -> PathBuf {
        version_dir.join(BINARY_FILENAME)
    }

    /// The stable `current` pointer.
    #[must_use]
    pub fn current(&self) -> PathBuf {
        self.root.join("current")
    }

    /// The binary the service runs: `current/supervisor-host`.
    #[must_use]
    pub fn current_binary(&self) -> PathBuf {
        self.current().join(BINARY_FILENAME)
    }

    /// The update marker path.
    #[must_use]
    pub fn marker(&self) -> PathBuf {
        self.root.join(".update-marker")
    }

    /// The health file the daemon writes and the Updater polls.
    #[must_use]
    pub fn health_file(&self) -> PathBuf {
        self.root.join("health.json")
    }

    /// The version directory `current` currently resolves to, or `None` if the pointer is absent.
    ///
    /// # Errors
    /// Returns an error only for unexpected I/O failures reading the link.
    pub fn current_target(&self) -> Result<Option<PathBuf>> {
        match std::fs::read_link(self.current()) {
            Ok(target) => Ok(Some(target)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => {
                Err(err).with_context(|| format!("reading the pointer {:?}", self.current()))
            }
        }
    }

    /// Point `current` at `version_dir`, replacing any existing pointer. On Unix this is an atomic
    /// rename of a freshly created symlink; on Windows the junction is recreated (the service is
    /// stopped during a switch, so nothing reads the pointer in between).
    ///
    /// # Errors
    /// Returns an error if the pointer cannot be created or swapped.
    pub fn set_current(&self, version_dir: &Path) -> Result<()> {
        set_pointer(&self.current(), version_dir)
    }

    /// Remove version directories not present in `keep` (keep-N retention / GC).
    ///
    /// # Errors
    /// Returns an error if the versions directory cannot be listed.
    pub fn prune(&self, keep: &[PathBuf]) -> Result<()> {
        let versions = self.versions_dir();
        let entries = match std::fs::read_dir(&versions) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err).with_context(|| format!("listing {}", versions.display())),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !keep.iter().any(|k| k == &path) {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn set_pointer(link: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let staging = link.with_file_name("current.tmp");
    let _ = std::fs::remove_file(&staging);
    symlink(target, &staging).with_context(|| format!("creating symlink {}", staging.display()))?;
    // Atomic replace: rename over the existing pointer.
    std::fs::rename(&staging, link)
        .with_context(|| format!("swapping the pointer {}", link.display()))?;
    Ok(())
}

#[cfg(windows)]
fn set_pointer(link: &Path, target: &Path) -> Result<()> {
    use anyhow::ensure;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // Remove any existing junction (removes the link, not the target contents).
    if std::fs::symlink_metadata(link).is_ok() {
        std::fs::remove_dir(link)
            .with_context(|| format!("removing the existing pointer {}", link.display()))?;
    }
    // A directory junction needs no symlink privilege (unlike a Windows symlink).
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .status()
        .context("creating the current junction via mklink /J")?;
    ensure!(status.success(), "mklink /J failed for {}", link.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("opamp-layout-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn paths_are_derived_from_the_root() {
        let layout = Layout::new("/opt/opamp");
        assert_eq!(
            layout.version_dir("abc"),
            PathBuf::from("/opt/opamp/versions/abc")
        );
        assert_eq!(
            layout.current_binary(),
            PathBuf::from("/opt/opamp/current").join(BINARY_FILENAME)
        );
        assert_eq!(layout.marker(), PathBuf::from("/opt/opamp/.update-marker"));
    }

    // `current_target` resolves the pointer with `read_link`, which reads a Unix symlink but not a
    // Windows directory junction — so asserting on the pointer's target is Unix-only. The junction
    // path is exercised by manual smoke tests on a real Windows host (ADR-0006/ADR-0007).
    #[cfg(unix)]
    #[test]
    fn set_current_points_and_repoints_atomically() {
        let tmp = TempDir::new("switch");
        let layout = Layout::new(&tmp.0);
        let v1 = layout.version_dir("v1");
        let v2 = layout.version_dir("v2");
        std::fs::create_dir_all(&v1).unwrap();
        std::fs::create_dir_all(&v2).unwrap();

        assert_eq!(layout.current_target().unwrap(), None);

        layout.set_current(&v1).unwrap();
        assert_eq!(
            layout.current_target().unwrap().as_deref(),
            Some(v1.as_path())
        );

        // Repoint (the rollback direction) replaces the pointer.
        layout.set_current(&v2).unwrap();
        assert_eq!(
            layout.current_target().unwrap().as_deref(),
            Some(v2.as_path())
        );
    }

    #[test]
    fn prune_removes_all_but_kept_versions() {
        let tmp = TempDir::new("prune");
        let layout = Layout::new(&tmp.0);
        let keep = layout.version_dir("keep");
        let drop = layout.version_dir("drop");
        std::fs::create_dir_all(&keep).unwrap();
        std::fs::create_dir_all(&drop).unwrap();

        layout.prune(std::slice::from_ref(&keep)).unwrap();

        assert!(keep.exists());
        assert!(!drop.exists());
    }
}
