//! The versioned install layout (ADR-0010): what makes a future self-update a pointer switch.
//!
//! ```text
//! <root>/versions/opamp-client-<MAJOR.MINOR.PATCH>-<hash>/client   # side-by-side versions
//! <root>/current -> versions/opamp-client-…/                       # symlink (Unix) / junction (Windows)
//! <root>/state/                                                    # default per-instance state
//! ```
//!
//! The directory name is Elastic Agent's `<component>-<version>-<hash>` scheme: always the bare
//! version base and the commit short-hash, never the pre-release — whether a directory holds a
//! release or a dev build is answered by the manifest inside it, which records the full ADR-0009
//! version string and the binary's SHA-256 (what a future self-update verifies against). The
//! service's program is `<root>/current/client`, so switching versions never re-registers the
//! service.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The platform's binary filename inside a version directory.
pub const BINARY_FILENAME: &str = if cfg!(windows) {
    "client.exe"
} else {
    "client"
};

/// The component prefix of every version directory.
const COMPONENT: &str = "opamp-client";

/// The manifest inside each version directory: the full version string and the content hash.
const MANIFEST_FILENAME: &str = "manifest.toml";

/// The install layout rooted at an operator-chosen directory (never a fixed path).
#[derive(Debug, Clone)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// A layout rooted at `root`; nothing is created until something is staged.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `<root>/versions` — every installed version, side by side.
    #[must_use]
    pub fn versions_dir(&self) -> PathBuf {
        self.root.join("versions")
    }

    /// `<root>/versions/<name>` for a version-directory name.
    #[must_use]
    pub fn version_dir(&self, name: &str) -> PathBuf {
        self.versions_dir().join(name)
    }

    /// `<root>/current` — the stable pointer the service is registered against.
    #[must_use]
    pub fn current(&self) -> PathBuf {
        self.root.join("current")
    }

    /// `<root>/current/client` — the program path an installed service runs.
    #[must_use]
    pub fn current_binary(&self) -> PathBuf {
        self.current().join(BINARY_FILENAME)
    }

    /// `<root>/state` — the default per-instance state directory (ADR-0010).
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }

    /// Point `current` at `version_dir`.
    ///
    /// On Unix this is atomic: a temporary symlink is `rename`d over `current` (never
    /// unlink-then-relink, which leaves a window with no pointer). On Windows the junction is
    /// recreated; callers only switch while the service is stopped (ADR-0010).
    ///
    /// # Errors
    /// Returns an error if the pointer cannot be created.
    pub fn set_current(&self, version_dir: &Path) -> Result<(), String> {
        #[cfg(unix)]
        {
            let staging = self.root.join(".current.tmp");
            let _ = std::fs::remove_file(&staging);
            std::os::unix::fs::symlink(version_dir, &staging)
                .map_err(|e| format!("cannot create the current pointer: {e}"))?;
            std::fs::rename(&staging, self.current())
                .map_err(|e| format!("cannot switch the current pointer: {e}"))
        }
        #[cfg(windows)]
        {
            let current = self.current();
            if current.exists() {
                std::fs::remove_dir(&current)
                    .map_err(|e| format!("cannot remove the current junction: {e}"))?;
            }
            // A directory junction needs no symlink privilege (unlike a real symlink), which is
            // why ADR-0010 uses one. `mklink /J` is the canonical way to create it.
            let status = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&current)
                .arg(version_dir)
                .status()
                .map_err(|e| format!("cannot run mklink: {e}"))?;
            if status.success() {
                Ok(())
            } else {
                Err(format!("mklink /J failed with {status}"))
            }
        }
    }

    /// Detect the layout an executable runs from (`<root>/versions/<version-dir>/client`),
    /// returning the layout and the version directory. `None` for binaries outside a layout
    /// (development builds, `cargo run`).
    #[must_use]
    pub fn enclosing(exe: &Path) -> Option<(Layout, PathBuf)> {
        let version_dir = exe.parent()?;
        let versions = version_dir.parent()?;
        if versions.file_name()? != "versions" {
            return None;
        }
        let root = versions.parent()?;
        Some((Layout::new(root), version_dir.to_path_buf()))
    }

    /// Self-heal a torn pointer switch (ADR-0010): if `current` does not resolve to
    /// `running_dir` — the version directory this binary actually runs from — repoint it.
    /// Returns whether a repair happened.
    ///
    /// # Errors
    /// Returns an error if the pointer cannot be inspected or repaired.
    pub fn heal_current(&self, running_dir: &Path) -> Result<bool, String> {
        let points_at_us = std::fs::canonicalize(self.current())
            .ok()
            .zip(std::fs::canonicalize(running_dir).ok())
            .is_some_and(|(current, running)| current == running);
        if points_at_us {
            return Ok(false);
        }
        self.set_current(running_dir)?;
        Ok(true)
    }
}

/// The version-directory name for a full ADR-0009 version string:
/// `opamp-client-<MAJOR.MINOR.PATCH>-<hash>` — never the pre-release (ADR-0010).
#[must_use]
pub fn version_dir_name(full_version: &str) -> String {
    let (base, metadata) = full_version.split_once('+').unwrap_or((full_version, ""));
    let core = base.split_once('-').map_or(base, |(core, _)| core);
    if metadata.is_empty() {
        // An OPAMP_FLEET_VERSION-override build outside a repository has no commit to cite.
        format!("{COMPONENT}-{core}")
    } else {
        format!("{COMPONENT}-{core}-{metadata}")
    }
}

/// Stage the running executable into its version directory, write the manifest, and point
/// `current` at it. Returns the program path to register the service with
/// (`<root>/current/client`). Staging an already-present version replaces its contents — an
/// idempotent re-install, never a silent mix of two builds.
///
/// # Errors
/// Returns an error if the executable cannot be read or the layout cannot be written.
pub fn stage_current_exe(layout: &Layout) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    let bytes = std::fs::read(&exe).map_err(|e| format!("cannot read {}: {e}", exe.display()))?;
    let sha256 = hex::encode(Sha256::digest(&bytes));

    let version = crate::version::version();
    let dir = layout.version_dir(&version_dir_name(version));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let binary = dir.join(BINARY_FILENAME);
    std::fs::write(&binary, &bytes)
        .map_err(|e| format!("cannot write {}: {e}", binary.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot mark {} executable: {e}", binary.display()))?;
    }

    let manifest = format!(
        "# Written by `client service install` (ADR-0010).\nversion = \"{version}\"\nsha256 = \"{sha256}\"\n"
    );
    let manifest_path = dir.join(MANIFEST_FILENAME);
    std::fs::write(&manifest_path, manifest)
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;

    layout.set_current(&dir)?;
    Ok(layout.current_binary())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_name_is_base_plus_hash_never_the_prerelease() {
        assert_eq!(
            version_dir_name("1.2.3+a1b2c3d"),
            "opamp-client-1.2.3-a1b2c3d"
        );
        assert_eq!(
            version_dir_name("1.2.3-dev+b4e5f6a"),
            "opamp-client-1.2.3-b4e5f6a"
        );
        assert_eq!(
            version_dir_name("0.0.0-dev+a1b2c3d"),
            "opamp-client-0.0.0-a1b2c3d"
        );
        // An override build outside a repository carries no metadata.
        assert_eq!(version_dir_name("9.9.9"), "opamp-client-9.9.9");
    }

    #[test]
    fn paths_derive_from_the_root() {
        let layout = Layout::new("/opt/x");
        assert_eq!(layout.versions_dir(), PathBuf::from("/opt/x/versions"));
        assert_eq!(layout.current(), PathBuf::from("/opt/x/current"));
        assert_eq!(layout.state_dir(), PathBuf::from("/opt/x/state"));
        assert!(layout.current_binary().starts_with("/opt/x/current"));
    }

    #[cfg(unix)]
    #[test]
    fn set_current_points_and_repoints() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Canonicalize up front: macOS tempdirs live under /var → /private/var, so a resolved
        // pointer would otherwise never equal the raw path.
        let layout = Layout::new(dir.path().canonicalize().expect("canonical tempdir"));
        let a = layout.version_dir("opamp-client-1.0.0-aaaaaaa");
        let b = layout.version_dir("opamp-client-2.0.0-bbbbbbb");
        std::fs::create_dir_all(&a).expect("create a");
        std::fs::create_dir_all(&b).expect("create b");

        layout.set_current(&a).expect("point at a");
        assert_eq!(std::fs::canonicalize(layout.current()).expect("resolve"), a);
        // Repointing replaces the pointer without a gap.
        layout.set_current(&b).expect("repoint at b");
        assert_eq!(std::fs::canonicalize(layout.current()).expect("resolve"), b);
    }

    #[cfg(unix)]
    #[test]
    fn stage_writes_binary_manifest_and_pointer() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let layout = Layout::new(dir.path());
        let program = stage_current_exe(&layout).expect("stage");
        assert_eq!(program, layout.current_binary());

        let version_dir = layout.version_dir(&version_dir_name(crate::version::version()));
        let staged = version_dir.join(BINARY_FILENAME);
        assert!(staged.is_file());
        let mode = staged.metadata().expect("metadata").permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the staged binary must be executable");

        let manifest =
            std::fs::read_to_string(version_dir.join(MANIFEST_FILENAME)).expect("manifest");
        assert!(manifest.contains(&format!("version = \"{}\"", crate::version::version())));
        let sha = manifest
            .lines()
            .find_map(|l| l.strip_prefix("sha256 = \""))
            .and_then(|rest| rest.strip_suffix('"'))
            .expect("sha256 line");
        assert_eq!(sha.len(), 64);

        assert_eq!(
            std::fs::canonicalize(layout.current()).expect("resolve current"),
            std::fs::canonicalize(&version_dir).expect("resolve version dir")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_torn_pointer_is_healed_a_correct_one_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Canonicalize up front — see set_current_points_and_repoints.
        let layout = Layout::new(dir.path().canonicalize().expect("canonical tempdir"));
        let a = layout.version_dir("opamp-client-1.0.0-aaaaaaa");
        let b = layout.version_dir("opamp-client-2.0.0-bbbbbbb");
        std::fs::create_dir_all(&a).expect("create a");
        std::fs::create_dir_all(&b).expect("create b");

        // Torn swap: current points at a, but the running binary lives in b.
        layout.set_current(&a).expect("point at a");
        assert!(layout.heal_current(&b).expect("heal"), "must repair");
        assert_eq!(std::fs::canonicalize(layout.current()).expect("resolve"), b);
        // Second run: nothing to do.
        assert!(!layout.heal_current(&b).expect("heal again"));
    }

    #[test]
    fn enclosing_detects_a_layout_and_rejects_loose_binaries() {
        let (layout, version_dir) = Layout::enclosing(Path::new(
            "/opt/fleet/versions/opamp-client-1.2.3-a1b2c3d/client",
        ))
        .expect("a layout path");
        assert_eq!(layout.current(), PathBuf::from("/opt/fleet/current"));
        assert_eq!(
            version_dir,
            PathBuf::from("/opt/fleet/versions/opamp-client-1.2.3-a1b2c3d")
        );
        assert!(Layout::enclosing(Path::new("/usr/bin/client")).is_none());
        assert!(Layout::enclosing(Path::new("client")).is_none());
    }
}
