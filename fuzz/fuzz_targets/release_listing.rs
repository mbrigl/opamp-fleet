//! An answer of GitHub's API that `opamp-fleetctl package fetch` reads (OPS-operator-program-fleetctl):
//! a tag listing, whose tags name the versions offered, or a release object, whose asset names name
//! the platforms fetched. Every platform read out of an asset name is canonical. Verifies: Q-2
#![no_main]

use libfuzzer_sys::fuzz_target;

// `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the parsers are compiled in by
// their path. This target calls four of them.
#[allow(
    dead_code,
    reason = "the checksum file's parsers and the AppImage's headers are fuzzed by their own targets"
)]
#[path = "../../crates/fleet-tools/src/fetch/upstream.rs"]
mod upstream;

fuzz_target!(|data: &[u8]| {
    if let Ok(tags) = upstream::tag_names(data) {
        for tag in &tags {
            for (prefix, parts) in [("v", 3..=3), ("version/", 3..=3), ("", 2..=3)] {
                let _ = upstream::version_behind(prefix, parts, tag);
            }
        }
    }
    if let Ok(Some(assets)) = upstream::release_assets(data) {
        for (name, _) in &assets {
            for prefix in ["otelcol-contrib_0.165.0_", "supervisor_0.8.6_"] {
                // A platform read out of an asset name is canonical, so the artifact is stored
                // under the Platform an Agent reports (PKG-signed-packages-and-delivery §6).
                if let Some((os, arch)) = upstream::platform_of(name, prefix) {
                    assert_eq!(fleet_core::platform::os_token(&os), Ok(os.clone()));
                    assert_eq!(fleet_core::platform::arch_token(&arch), Ok(arch.clone()));
                }
            }
        }
    }
});
