//! What `package fetch` reads out of the bytes an upstream sends: GitHub's tag listing and release
//! object, and the checksum files beside an artifact, with the asset names and tags in them.
//!
//! Every function here reads bytes from the network, so each has a fuzz target that runs in CI
//! (specification Q-2): `fuzz/fuzz_targets/release_listing.rs` and
//! `fuzz/fuzz_targets/checksum_file.rs`. Their size limit is enforced before this module sees the
//! bytes, by the bounded reads of the module above.
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

/// The version a tag names behind `prefix`, or `None` when the tag is not such a release: exactly
/// three numeric parts behind the prefix, and nothing else (`v0.158.0`, `version/1.2.3`).
pub fn version_behind(prefix: &str, tag: &str) -> Option<String> {
    let numeric = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let rest = tag.strip_prefix(prefix)?;
    let shaped = rest.split('.').count() == 3;
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
