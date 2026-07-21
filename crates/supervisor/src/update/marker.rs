//! The update marker (ADR-0007): crash-consistent record of an in-flight update.
//!
//! Written atomically (temp file + rename) before the pointer switch and cleared only after a
//! healthy commit, the marker lets an interrupted update be recovered on the next startup: whichever
//! version `current` ends up resolving to is compared against the marker's target to decide whether
//! the switch had completed (commit) or not (aborted). See [`super::resume_if_pending`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// The persisted description of an update that is in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateMarker {
    /// SHA-256 of the version being switched to.
    pub target_hash: String,
    /// The version directory being switched to.
    pub target_dir: PathBuf,
    /// The version directory to roll back to, if any (absent on a first install).
    pub previous_dir: Option<PathBuf>,
}

/// Write the marker atomically: serialize to a temp file, then rename over the marker path.
///
/// # Errors
/// Returns an error if the marker cannot be serialized or written.
pub fn write_atomic(path: &Path, marker: &UpdateMarker) -> Result<()> {
    let json = serde_json::to_vec_pretty(marker).context("serializing the update marker")?;
    let staging = path.with_extension("tmp");
    std::fs::write(&staging, &json).with_context(|| format!("writing {}", staging.display()))?;
    std::fs::rename(&staging, path)
        .with_context(|| format!("committing the marker {}", path.display()))?;
    Ok(())
}

/// Read the marker, returning `None` if it does not exist.
///
/// # Errors
/// Returns an error if the marker exists but cannot be read or parsed.
pub fn read(path: &Path) -> Result<Option<UpdateMarker>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let marker = serde_json::from_slice(&bytes).context("parsing the update marker")?;
            Ok(Some(marker))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

/// Remove the marker (a no-op if it is already gone).
///
/// # Errors
/// Returns an error if the marker exists but cannot be removed.
pub fn clear(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("clearing {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_clear_roundtrip() {
        let dir = std::env::temp_dir().join(format!("opamp-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".update-marker");

        assert_eq!(read(&path).unwrap(), None);

        let marker = UpdateMarker {
            target_hash: "abc123".to_string(),
            target_dir: PathBuf::from("/opt/opamp/versions/abc123"),
            previous_dir: Some(PathBuf::from("/opt/opamp/versions/old")),
        };
        write_atomic(&path, &marker).unwrap();
        assert_eq!(read(&path).unwrap(), Some(marker));

        clear(&path).unwrap();
        assert_eq!(read(&path).unwrap(), None);
        // Clearing an absent marker is fine.
        clear(&path).unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }
}
