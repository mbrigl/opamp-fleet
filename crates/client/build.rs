//! Computes the version this build bakes into the binary (ADR-0009).
//!
//! Resolution order, first match wins: the `OPAMP_FLEET_VERSION` override (the escape hatch for
//! builds outside a git checkout), a well-formed `version/*` tag pointing at HEAD (a release
//! build), the most recent *reachable* `version/*` tag plus the `-dev` pre-release (a development
//! build), and `0.0.0-dev` when no such tag is reachable at all. The commit short-hash is
//! appended as SemVer build metadata whenever a repository is present, so the same commit always
//! reproduces the byte-identical version string. A malformed tag or override fails the build
//! (fail closed) rather than guessing.

use git2::{DescribeFormatOptions, DescribeOptions, Repository};

const TAG_PREFIX: &str = "version/";

fn main() {
    println!("cargo:rerun-if-env-changed=OPAMP_FLEET_VERSION");
    match resolve() {
        Ok(version) => println!("cargo:rustc-env=OPAMP_BUILD_VERSION={version}"),
        Err(e) => {
            eprintln!("cannot resolve the build version (ADR-0009): {e}");
            std::process::exit(1);
        }
    }
}

fn resolve() -> Result<String, String> {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").map_err(|e| format!("CARGO_MANIFEST_DIR: {e}"))?;
    let repo = Repository::discover(&manifest_dir).ok();
    if let Some(repo) = &repo {
        // The version changes with the checked-out commit and with tag edits.
        println!(
            "cargo:rerun-if-changed={}",
            repo.path().join("HEAD").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            repo.path().join("refs").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            repo.path().join("packed-refs").display()
        );
    }

    if let Ok(raw) = std::env::var("OPAMP_FLEET_VERSION") {
        let base = parse_components(&raw).ok_or_else(|| {
            format!("OPAMP_FLEET_VERSION {raw:?} is not a strict MAJOR.MINOR.PATCH version")
        })?;
        // Without a repository there is no commit to cite; the override then stands alone.
        return Ok(match &repo {
            Some(repo) => format!("{base}+{}", short_hash(repo)?),
            None => base,
        });
    }

    let repo = repo.ok_or_else(|| {
        "not inside a git repository and OPAMP_FLEET_VERSION is unset; set \
         OPAMP_FLEET_VERSION=MAJOR.MINOR.PATCH to build from sources without a checkout"
            .to_string()
    })?;
    let hash = short_hash(&repo)?;

    let base = if let Some(tag) = release_tag_on_head(&repo)? {
        parse_tag(&tag)
            .ok_or_else(|| format!("tag {tag:?} on HEAD is not a well-formed {TAG_PREFIX}* tag"))?
    } else if let Some(tag) = nearest_reachable_tag(&repo) {
        let base = parse_tag(&tag).ok_or_else(|| {
            format!("nearest reachable tag {tag:?} is not a well-formed {TAG_PREFIX}* tag")
        })?;
        format!("{base}-dev")
    } else {
        "0.0.0-dev".to_string()
    };
    Ok(format!("{base}+{hash}"))
}

/// The abbreviated commit id of HEAD: the first 7 hex characters of the full hash.
fn short_hash(repo: &Repository) -> Result<String, String> {
    let head = repo
        .head()
        .and_then(|r| r.peel_to_commit())
        .map_err(|e| format!("cannot resolve HEAD: {e}"))?;
    Ok(head.id().to_string()[..7].to_string())
}

/// A `version/*` tag pointing exactly at HEAD — the marker of a release build.
fn release_tag_on_head(repo: &Repository) -> Result<Option<String>, String> {
    let head = repo
        .head()
        .and_then(|r| r.peel_to_commit())
        .map_err(|e| format!("cannot resolve HEAD: {e}"))?
        .id();
    let refs = repo
        .references_glob(&format!("refs/tags/{TAG_PREFIX}*"))
        .map_err(|e| format!("cannot list version tags: {e}"))?;
    for reference in refs {
        let reference = reference.map_err(|e| format!("cannot read a version tag: {e}"))?;
        // Peeling covers both lightweight and annotated tags.
        if reference.peel_to_commit().map(|c| c.id()) == Ok(head) {
            if let Some(name) = reference.name() {
                return Ok(Some(name.trim_start_matches("refs/tags/").to_string()));
            }
        }
    }
    Ok(None)
}

/// The most recent `version/*` tag reachable from HEAD (`git describe` semantics), if any.
fn nearest_reachable_tag(repo: &Repository) -> Option<String> {
    repo.describe(
        DescribeOptions::new()
            .describe_tags()
            .pattern(&format!("{TAG_PREFIX}*")),
    )
    .ok()?
    // Abbreviation size 0 yields the bare tag name even when HEAD is ahead of it.
    .format(Some(DescribeFormatOptions::new().abbreviated_size(0)))
    .ok()
}

/// Strict parse of a `version/*` tag name into a normalised `MAJOR.MINOR.PATCH` string.
fn parse_tag(tag: &str) -> Option<String> {
    parse_components(tag.strip_prefix(TAG_PREFIX)?)
}

/// Strict SemVer core grammar (ADR-0009): exactly three non-negative integers without leading
/// zeros, separated by `.` or `/` (mixed permitted), normalised to dots. No pre-release, no
/// build metadata, no whitespace.
fn parse_components(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw.split(['.', '/']).collect();
    if parts.len() != 3 {
        return None;
    }
    for part in &parts {
        let numeric = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        let no_leading_zero = *part == "0" || !part.starts_with('0');
        if !numeric || !no_leading_zero {
            return None;
        }
    }
    Some(parts.join("."))
}
