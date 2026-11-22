//! A suite's signed `Release` text, as `opamp-fleetctl package fetch --agent icinga2` reads it once
//! `gpgv` verified it (CLT-icinga2-repacked-kind §18): the suite it names, its `Valid-Until`, and
//! the SHA-256 and size it states for an index. Whatever it holds, a digest read out of it is one
//! word, and the suite and date rules answer without panicking. Verifies: Q-2
#![no_main]

use libfuzzer_sys::fuzz_target;

// `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the parsers are compiled in by
// their path. This target calls three of them.
#[allow(
    dead_code,
    reason = "the index, the maintainer script and the ELF parsers are fuzzed by targets of their own"
)]
#[path = "../../crates/fleet-tools/src/fetch/debian.rs"]
mod debian;

fuzz_target!(|data: &[u8]| {
    // `gpgv` hands back the signed text, which the tool reads only as UTF-8.
    let Ok(release) = std::str::from_utf8(data) else {
        return;
    };
    for path in [
        "main/binary-amd64/Packages",
        "main/binary-amd64/Packages.xz",
        "main/binary-amd64/Packages.gz",
    ] {
        if let Some((hash, _)) = debian::release_digest(release, path) {
            assert!(!hash.is_empty() && !hash.contains(char::is_whitespace), "{hash:?}");
        }
    }
    let _ = debian::release_names(release, "bookworm-security");
    let _ = debian::release_current(release, time::OffsetDateTime::UNIX_EPOCH);
});
