//! What `package fetch` reads out of the bytes an upstream sends: GitHub's tag listing and release
//! object, the checksum files beside an artifact, with the asset names and tags in them, and the
//! headers of an AppImage the GLPI repack opens.
//!
//! Every function here reads bytes from the network, so each has a fuzz target that runs in CI
//! (specification Q-2): `fuzz/fuzz_targets/release_listing.rs`,
//! `fuzz/fuzz_targets/checksum_file.rs` and `fuzz/fuzz_targets/appimage_header.rs`. Their size
//! limit is enforced before this module sees the bytes, by the bounded reads of the module above;
//! the AppImage's headers are read at fixed lengths.
//!
//! `fleet-tools` has no library (DEV-six-crates-tokio-axum §22), so the fuzz crate compiles this
//! file into its targets by its path. That is why this module names nothing of `crate::` and uses
//! only `std`, `serde_json` and `fleet_core`, which the fuzz crate depends on too. The fuzz
//! crate's build enforces it: `scripts/check-rust.sh` checks `fuzz/Cargo.toml` on every
//! `scripts/check-all.sh` run, and `scripts/check-fuzz.sh` builds each target in
//! `.github/workflows/fuzz.yml`. A path of `crate::` resolves in neither.

use std::path::Path;

/// The tag names of a GitHub tag listing (`GET /repos/{repo}/tags`), in the order it lists them.
/// An entry without a string `name` is passed over.
///
/// # Errors
/// Returns the message to print, without the URL, when the answer is no JSON or no list.
pub fn tag_names(body: &[u8]) -> Result<Vec<String>, String> {
    let listing: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("the tag listing is not JSON: {e}"))?;
    Ok(listing
        .as_array()
        .ok_or("the tag listing is not a list")?
        .iter()
        .filter_map(|tag| Some(tag.get("name")?.as_str()?.to_string()))
        .collect())
}

/// The assets of a GitHub release object (`GET /repos/{repo}/releases/tags/{tag}`), as
/// `(name, download URL)`. An asset without both as strings is passed over. `None` when the answer
/// is JSON but carries no list of assets, which is how a missing release reads.
///
/// # Errors
/// Returns the message to print, without the URL, when the answer is no JSON.
pub fn release_assets(body: &[u8]) -> Result<Option<Vec<(String, String)>>, String> {
    let release: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("the release is not JSON: {e}"))?;
    let Some(assets) = release.get("assets").and_then(|a| a.as_array()) else {
        return Ok(None);
    };
    Ok(Some(
        assets
            .iter()
            .filter_map(|asset| {
                Some((
                    asset.get("name")?.as_str()?.to_string(),
                    asset.get("browser_download_url")?.as_str()?.to_string(),
                ))
            })
            .collect(),
    ))
}

/// The version a tag names behind `prefix`, or `None` when the tag is not such a release: a count
/// of numeric parts in `parts` behind the prefix, and nothing else (`v0.158.0`, `version/1.2.3`,
/// GLPI's `1.19`). `1.0-beta1` fails on its last part.
pub fn version_behind(
    prefix: &str,
    parts: std::ops::RangeInclusive<usize>,
    tag: &str,
) -> Option<String> {
    let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let rest = tag.strip_prefix(prefix)?;
    let shaped = parts.contains(&rest.split('.').count());
    (shaped && rest.split('.').all(numeric)).then(|| rest.to_string())
}

/// The platform an asset named `<prefix><os>_<arch>.tar.gz` is for, folded by the table an Agent's
/// report and the Server's store fold through (PKG-signed-packages-and-delivery §6) — or `None` when the name has
/// another shape or a token that cannot be canonical.
pub fn platform_of(name: &str, prefix: &str) -> Option<(String, String)> {
    let (os, arch) = name
        .strip_prefix(prefix)?
        .strip_suffix(".tar.gz")?
        .split_once('_')?;
    Some((
        fleet_core::platform::os_token(os).ok()?,
        fleet_core::platform::arch_token(arch).ok()?,
    ))
}

/// The digest of a sidecar checksum file: its first word, which is all a bare digest holds. Empty
/// when the file holds no word, which matches no artifact's hash.
pub fn bare_digest(text: &str) -> String {
    text.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The artifact's digest out of a `sha256sum`-style file: `<hash>  <name>` lines, where the name
/// may carry a `*` for binary mode or a path in front of it. Matched on the file name alone, which
/// is the one thing every one of these files spells the same way.
pub fn parse_sums(text: &str, artifact: &Path) -> Option<String> {
    let wanted = artifact.file_name()?.to_string_lossy().into_owned();
    text.lines()
        .filter_map(|line| line.split_once(char::is_whitespace))
        .find(|(_, name)| {
            Path::new(name.trim().trim_start_matches('*'))
                .file_name()
                .is_some_and(|n| n.to_string_lossy() == wanted)
        })
        .map(|(hash, _)| hash.trim().to_string())
}

/// How many bytes of an AppImage [`squashfs_offset`] reads: a 64-bit ELF header.
pub const ELF_HEADER_LEN: usize = 64;

/// How many bytes of a squashfs superblock [`squashfs_inodes`] reads: its magic `hsqs` and its
/// inode count, a 32-bit little-endian number.
pub const SUPERBLOCK_HEAD_LEN: usize = 8;

/// Where the squashfs file system of a type-2 AppImage begins, read from the AppImage's first
/// [`ELF_HEADER_LEN`] bytes: the end of the ELF program in front of it, which is where its section
/// header table ends — `e_shoff`, plus `e_shnum` entries of `e_shentsize` bytes. The AppImage
/// runtime finds its own file system the same way (CLT-glpi-agent-kind §6).
///
/// # Errors
/// Returns why the bytes are no such header: too short, no 64-bit little-endian ELF file, no
/// AppImage type-2 mark (`AI\x02` at byte 8), or a table that ends beyond any file.
pub fn squashfs_offset(header: &[u8]) -> Result<u64, &'static str> {
    let header: &[u8; ELF_HEADER_LEN] = header
        .get(..ELF_HEADER_LEN)
        .and_then(|head| head.try_into().ok())
        .ok_or("it is shorter than an ELF header")?;
    if &header[0..4] != b"\x7fELF" {
        return Err("it is no ELF file");
    }
    if header[4] != 2 || header[5] != 1 {
        return Err("it is no 64-bit little-endian ELF file");
    }
    if &header[8..11] != b"AI\x02" {
        return Err("it carries no AppImage type-2 mark");
    }
    let u16_at = |at: usize| u64::from(u16::from_le_bytes([header[at], header[at + 1]]));
    let mut shoff = [0u8; 8];
    shoff.copy_from_slice(&header[0x28..0x30]);
    u16_at(0x3A)
        .checked_mul(u16_at(0x3C))
        .and_then(|table| table.checked_add(u64::from_le_bytes(shoff)))
        .ok_or("its section header table ends beyond any file")
}

/// The inode count a squashfs superblock states, read from its first [`SUPERBLOCK_HEAD_LEN`]
/// bytes: one inode per file, directory and link the file system holds, hard links aside.
///
/// # Errors
/// Returns why the bytes are no superblock: too short, or no magic `hsqs`.
pub fn squashfs_inodes(head: &[u8]) -> Result<u32, &'static str> {
    let head: &[u8; SUPERBLOCK_HEAD_LEN] = head
        .get(..SUPERBLOCK_HEAD_LEN)
        .and_then(|head| head.try_into().ok())
        .ok_or("nothing follows its ELF program")?;
    if &head[0..4] != b"hsqs" {
        return Err("no squashfs file system follows its ELF program");
    }
    Ok(u32::from_le_bytes([head[4], head[5], head[6], head[7]]))
}
