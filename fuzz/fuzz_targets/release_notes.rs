//! A GitHub release object `opamp-fleetctl package fetch` reads for Telegraf, and the digest table
//! in its notes (CLT-telegraf-kind §3). The first line of the input names the archive, the rest is
//! the release object. Whatever it holds, a digest read out of the notes is 64 lower-case hex
//! characters, and the same notes give the same answer for the same name. Verifies: Q-2
#![no_main]

use libfuzzer_sys::fuzz_target;

// `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the parsers are compiled in by
// their path. This target calls two of them.
#[allow(
    dead_code,
    reason = "the tag listing's and the checksum file's parsers are fuzzed by release_listing and checksum_file"
)]
#[path = "../../crates/fleet-tools/src/fetch/upstream.rs"]
mod upstream;

fuzz_target!(|data: &[u8]| {
    let split = data.iter().position(|&b| b == b'\n').unwrap_or(data.len());
    let (name, release) = data.split_at(split);
    let Ok(asset) = std::str::from_utf8(name) else {
        return;
    };
    // The release object as GitHub sends it, and its notes taken as they are: the notes are
    // parsed whether or not they came out of JSON, so the table parser sees arbitrary text too.
    let notes = upstream::release_notes(release)
        .unwrap_or_else(|_| String::from_utf8_lossy(release).into_owned());
    if let Some(digest) = upstream::release_notes_digest(&notes, asset) {
        assert_eq!(digest.len(), 64, "{digest:?}");
        assert!(
            digest
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
            "{digest:?}"
        );
        assert_eq!(upstream::release_notes_digest(&notes, asset), Some(digest));
    }
});
