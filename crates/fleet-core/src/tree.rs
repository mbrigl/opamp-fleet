//! The links a kind's tree package keeps as links
//! (ADR-draft-the-operator-program-opamp-fleetctl clause 27,
//! ADR-draft-packages-signatures-and-delivery clause 57).
//!
//! A tree package carries no link, with one exception a kind decides: an absolute link it names
//! by its exact path in the tree and its exact target, such as a program loader an upstream tree
//! reaches on the host it runs on. `opamp-fleetctl` packs exactly those as links and refuses every
//! other link that leads out of the tree; the Client creates exactly those and refuses any other
//! link. Both ends hold a link against the kind's list with [`is_kept`], so they cannot disagree
//! on what matches.

use std::ffi::OsStr;
use std::path::Path;

/// One absolute link a kind's tree keeps: where it sits in the tree, as `/`-separated components
/// relative to the tree's root (`runtime/lib64/ld-linux-x86-64.so.2`), and the absolute target it
/// names, byte for byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptLink {
    /// The link's path in the tree, relative to its root, `/`-separated.
    pub path: &'static str,
    /// The link's target, absolute.
    pub target: &'static str,
}

/// Whether a link at `path` in the tree, relative to its root, naming `target` is one of `kept`:
/// its path has exactly the components a kept link names and its target is exactly that link's
/// absolute target. A relative target never matches, and neither does a kept link whose own path
/// is not plain relative components, so a kind's mistake keeps nothing.
#[must_use]
pub fn is_kept(kept: &[KeptLink], path: &Path, target: &Path) -> bool {
    kept.iter().any(|link| {
        let plain = !link.path.is_empty()
            && link
                .path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..");
        plain
            && link.target.starts_with('/')
            && target.as_os_str() == OsStr::new(link.target)
            && path
                .components()
                .map(|component| component.as_os_str())
                .eq(link.path.split('/').map(OsStr::new))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOADER: KeptLink = KeptLink {
        path: "runtime/lib64/ld-linux-x86-64.so.2",
        target: "/lib64/ld-linux-x86-64.so.2",
    };

    /// Only the named path with the named target is kept: another path, another target, a
    /// relative target and a kind's malformed entry keep nothing.
    /// Verifies: ADR-draft-packages-signatures-and-delivery
    #[test]
    fn only_the_named_path_with_the_named_target_is_kept() {
        let target = Path::new("/lib64/ld-linux-x86-64.so.2");
        assert!(is_kept(
            &[LOADER],
            Path::new("runtime/lib64/ld-linux-x86-64.so.2"),
            target
        ));
        assert!(!is_kept(
            &[],
            Path::new("runtime/lib64/ld-linux-x86-64.so.2"),
            target
        ));
        assert!(!is_kept(
            &[LOADER],
            Path::new("runtime/lib/ld-linux-x86-64.so.2"),
            target
        ));
        assert!(!is_kept(
            &[LOADER],
            Path::new("lib64/ld-linux-x86-64.so.2"),
            target
        ));
        assert!(!is_kept(
            &[LOADER],
            Path::new("runtime/lib64/ld-linux-x86-64.so.2"),
            Path::new("/etc/passwd")
        ));
        assert!(!is_kept(
            &[LOADER],
            Path::new("runtime/lib64/ld-linux-x86-64.so.2"),
            Path::new("/lib64/ld-linux-x86-64.so.2/")
        ));
        let relative = KeptLink {
            path: "runtime/lib64/ld-linux-x86-64.so.2",
            target: "../lib/ld.so",
        };
        assert!(!is_kept(
            &[relative],
            Path::new("runtime/lib64/ld-linux-x86-64.so.2"),
            Path::new("../lib/ld.so")
        ));
        let climbing = KeptLink {
            path: "../outside",
            target: "/lib64/ld-linux-x86-64.so.2",
        };
        assert!(!is_kept(&[climbing], Path::new("../outside"), target));
    }
}
