//! A `Packages` index, as `opamp-fleetctl package fetch --agent icinga2` reads it once its digest
//! matched the signed `Release` text (CLT-icinga2-repacked-kind §18): its stanzas, their
//! relationship fields, the `libc6` floor they state, and Debian's order of their versions.
//! Whatever it holds, a relationship names a package by one word, and the version order is a
//! total order on the versions it states. Verifies: Q-2
#![no_main]

use std::cmp::Ordering;

use libfuzzer_sys::fuzz_target;

// `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the parsers are compiled in by
// their path. This target calls four of them.
#[allow(
    dead_code,
    reason = "the release text, the maintainer script and the ELF parsers are fuzzed by targets of their own"
)]
#[path = "../../crates/fleet-tools/src/fetch/debian.rs"]
mod debian;

fuzz_target!(|data: &[u8]| {
    // The tool refuses an index that is not UTF-8 before it parses it.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let entries = debian::parse_deb_index(text);
    for entry in &entries {
        for field in [&entry.depends, &entry.pre_depends, &entry.provides] {
            for alternatives in debian::relationships(field) {
                for name in alternatives {
                    assert!(
                        !name.is_empty() && !name.contains([',', '|', ':']),
                        "{name:?}"
                    );
                    assert!(!name.contains(char::is_whitespace), "{name:?}");
                }
            }
            let _ = debian::libc_floor(field);
        }
    }
    // The newest version is picked by this order, so it must be one.
    for a in entries.iter().take(8) {
        assert_eq!(debian::deb_version_cmp(&a.version, &a.version), Ordering::Equal);
        for b in entries.iter().take(8) {
            assert_eq!(
                debian::deb_version_cmp(&a.version, &b.version),
                debian::deb_version_cmp(&b.version, &a.version).reverse(),
                "{:?} {:?}",
                a.version,
                b.version
            );
        }
    }
});
