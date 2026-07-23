//! The one place this binary states its version (ADR-0009).
//!
//! `build.rs` computes the full string from git — `MAJOR.MINOR.PATCH` from the `version/*` tag on
//! HEAD for a release, the nearest reachable tag plus `-dev` otherwise, always with the commit
//! short-hash as build metadata — and bakes it in via the `OPAMP_BUILD_VERSION` compile-time
//! environment variable. Every surface that reports a version (the OpAMP `service.version`
//! attribute, the CLI `--version` output, the install layout of ADR-0010) must call [`version`]
//! rather than `CARGO_PKG_VERSION`, which knows nothing of tags or commits.

/// The version this build reports, e.g. `1.2.3+a1b2c3d` or `1.2.3-dev+b4e5f6a`.
#[must_use]
pub fn version() -> &'static str {
    env!("OPAMP_BUILD_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_has_the_adr_0009_shape() {
        let full = super::version();
        let (base, metadata) = full.split_once('+').unwrap_or((full, ""));
        let core = base.strip_suffix("-dev").unwrap_or(base);
        let parts: Vec<&str> = core.split('.').collect();
        assert_eq!(parts.len(), 3, "{full:?}: base is not MAJOR.MINOR.PATCH");
        for part in parts {
            assert!(
                !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()),
                "{full:?}: non-numeric version component {part:?}"
            );
        }
        // Builds inside a repository (all local and CI builds) carry the commit short-hash.
        if !metadata.is_empty() {
            assert_eq!(metadata.len(), 7, "{full:?}: metadata is not a short hash");
            assert!(metadata.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }
}
