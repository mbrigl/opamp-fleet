//! A file out of a downloaded package, as `opamp-fleetctl package fetch --agent icinga2` reads the
//! `DT_NEEDED` names of an ELF file without running it (CLT-icinga2-repacked-kind §18). Whatever
//! its headers claim, the read stays inside the bytes, and a name read is one C string.
//! Verifies: Q-2
#![no_main]

use libfuzzer_sys::fuzz_target;

// `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the parsers are compiled in by
// their path. This target calls one of them.
#[allow(
    dead_code,
    reason = "the release text, the index and the maintainer script parsers are fuzzed by targets of their own"
)]
#[path = "../../crates/fleet-tools/src/fetch/debian.rs"]
mod debian;

fuzz_target!(|data: &[u8]| {
    if let Ok(Some(needed)) = debian::elf_needed(data) {
        for name in needed {
            assert!(!name.contains('\0'), "{name:?}");
        }
    }
});
