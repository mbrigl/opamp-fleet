//! The headers of an AppImage `opamp-fleetctl package fetch` opens before it repacks the GLPI
//! Agent's Linux build (CLT-glpi-agent-kind §6): the 64-byte ELF header that says where the
//! squashfs file system begins, and the head of that file system's superblock, whose inode count
//! the repack holds to the tree member cap. The input is the ELF header, then the superblock head.
//! Neither read panics, whatever the bytes. Verifies: Q-2, CLT-glpi-agent-kind
#![no_main]

use libfuzzer_sys::fuzz_target;

// `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the parsers are compiled in by
// their path. This target calls two of them.
#[allow(
    dead_code,
    reason = "the release listing's and the checksum file's parsers are fuzzed by their own targets"
)]
#[path = "../../crates/fleet-tools/src/fetch/upstream.rs"]
mod upstream;

fuzz_target!(|data: &[u8]| {
    let (header, rest) = data.split_at(data.len().min(upstream::ELF_HEADER_LEN));
    let _ = upstream::squashfs_offset(header);
    let _ = upstream::squashfs_inodes(rest);
});
