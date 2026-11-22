//! A checksum file `opamp-fleetctl package fetch` downloads beside an artifact: a sidecar holding a
//! bare digest, or a `sha256sum`-style file of `<hash>  <name>` lines (OPS-operator-program-fleetctl).
//! The first line of the input names the artifact, the rest is the file. Whatever it holds, the
//! digest read out of it carries no whitespace. Verifies: Q-2
#![no_main]

use std::path::Path;

use libfuzzer_sys::fuzz_target;

// `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the parsers are compiled in by
// their path. This target calls two of them.
#[allow(
    dead_code,
    reason = "the release listing's and the release notes' parsers are fuzzed by release_listing and release_notes"
)]
#[path = "../../crates/fleet-tools/src/fetch/upstream.rs"]
mod upstream;

fuzz_target!(|data: &[u8]| {
    // `package fetch` refuses a checksum file that is not UTF-8 before it parses it.
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let (artifact, sums) = text.split_once('\n').unwrap_or((text, ""));
    let digest = upstream::bare_digest(sums);
    assert!(!digest.contains(char::is_whitespace), "{digest:?}");
    if let Some(digest) = upstream::parse_sums(sums, Path::new(artifact)) {
        assert!(!digest.contains(char::is_whitespace), "{digest:?}");
    }
});
