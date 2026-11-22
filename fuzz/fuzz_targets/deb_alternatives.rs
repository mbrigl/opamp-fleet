//! A maintainer script out of a downloaded package, as `opamp-fleetctl package fetch --agent
//! icinga2` reads its `update-alternatives --install` lines (CLT-icinga2-repacked-kind §18).
//! Whatever it holds, each name yields one link and one target, each one word. Verifies: Q-2
#![no_main]

use libfuzzer_sys::fuzz_target;

// `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the parsers are compiled in by
// their path. This target calls one of them.
#[allow(
    dead_code,
    reason = "the release text, the index and the ELF parsers are fuzzed by targets of their own"
)]
#[path = "../../crates/fleet-tools/src/fetch/debian.rs"]
mod debian;

fuzz_target!(|data: &[u8]| {
    // The tool reads a `postinst` only as UTF-8.
    let Ok(script) = std::str::from_utf8(data) else {
        return;
    };
    for (link, target) in debian::winning_alternatives(script) {
        for word in [&link, &target] {
            assert!(!word.is_empty() && !word.contains(char::is_whitespace), "{word:?}");
        }
    }
});
