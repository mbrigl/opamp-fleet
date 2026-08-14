// Two things both ends need before they compile, in the crate both ends already depend on.
//
// 1. The OpAMP protobuf types, generated from the vendored, pinned schema (ADR-0006).
// 2. The version this build reports (ADR-0009, as amended by ADR-0026), baked in as
//    `OPAMP_BUILD_VERSION` and read back through `opamp::version::current`.
//
// The version lives here rather than in one binary's build script because ADR-0009 asks for a
// *single* helper that every surface reads, and `cargo:rustc-env` reaches only the crate whose
// script emitted it: a second copy in the Server would be a second implementation of the same rule,
// free to drift from this one.

use git2::Repository;

/// The Protocol Baseline. The single place the proto path derives from, which is what kept
/// upstream's relocation of the files (`proto/` to `proto/opamp/v1/`, adopted with `v0.19.0`) a
/// change to this file alone — docs/CONFORMANCE.md requires it stay that way.
const BASELINE: &str = "v0.20.0";

const TAG_PREFIX: &str = "version/";

fn main() {
    generate_protobuf_types();

    println!("cargo:rerun-if-env-changed=OPAMP_FLEET_VERSION");
    match resolve_version() {
        Ok(version) => println!("cargo:rustc-env=OPAMP_BUILD_VERSION={version}"),
        Err(e) => {
            eprintln!("cannot resolve the build version (ADR-0009, ADR-0026): {e}");
            std::process::exit(1);
        }
    }
}

fn generate_protobuf_types() {
    // The include root: `opamp.proto` imports its sibling as `opamp/v1/anyvalue.proto`, so the
    // import only resolves when the root is the directory *above* `opamp/v1`, never the directory
    // holding the files.
    let root = format!("proto/{BASELINE}");
    let files = [
        format!("{root}/opamp/v1/opamp.proto"),
        format!("{root}/opamp/v1/anyvalue.proto"),
    ];

    let descriptors = protox::compile(&files, [&root]).expect("compile OpAMP protobuf schema");
    prost_build::Config::new()
        .compile_fds(descriptors)
        .expect("generate Rust types from the OpAMP schema");

    for file in files {
        println!("cargo:rerun-if-changed={file}");
    }
}

// The **base** is `[workspace.package] version` from `Cargo.toml`, which Cargo hands this script as
// `CARGO_PKG_VERSION`; `OPAMP_FLEET_VERSION` overrides it for builds that want to state a version
// explicitly. Git decides only what is *around* that number:
//
// - a `version/<base>` tag pointing at HEAD — a **release** build: the base as it stands;
// - no such tag — a **development** build: the base plus the `-dev` pre-release, so a build heading
//   for a release is unmistakably not it;
// - a `version/*` tag on HEAD naming a **different** version — a disagreement between the file and
//   the tag, which fails the build rather than picking one.
//
// The commit short-hash is appended as SemVer build metadata whenever a repository is present, so
// the same commit always reproduces the byte-identical version string. Nothing time-dependent goes
// into it. A malformed version fails the build (fail closed) rather than being guessed at.

fn resolve_version() -> Result<String, String> {
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

    // The number itself, from the file that decides it (ADR-0026) or from the override.
    let (base, source) = match std::env::var("OPAMP_FLEET_VERSION") {
        Ok(raw) => (
            parse_components(&raw).ok_or_else(|| {
                format!("OPAMP_FLEET_VERSION {raw:?} is not a strict MAJOR.MINOR.PATCH version")
            })?,
            "OPAMP_FLEET_VERSION",
        ),
        Err(_) => {
            let raw = std::env::var("CARGO_PKG_VERSION")
                .map_err(|e| format!("CARGO_PKG_VERSION: {e}"))?;
            (
                parse_components(&raw).ok_or_else(|| {
                    format!(
                        "the version in Cargo.toml, {raw:?}, is not a strict MAJOR.MINOR.PATCH \
                         version (ADR-0009's grammar: three integers, no pre-release, no metadata)"
                    )
                })?,
                "Cargo.toml",
            )
        }
    };

    // Outside a checkout there is no commit to cite and no way to tell a release from a development
    // build, which is a question the file cannot answer. Fail closed, naming the way out.
    let Some(repo) = repo else {
        return Err(format!(
            "not inside a git repository, so this build cannot tell whether {base} (from {source}) \
             is released; set OPAMP_FLEET_VERSION=MAJOR.MINOR.PATCH to state it"
        ));
    };
    let hash = short_hash(&repo)?;

    match release_tag_on_head(&repo)? {
        // The tag agrees with the file: this commit *is* the release it says it is.
        Some(tag) if parse_tag(&tag).as_deref() == Some(base.as_str()) => {
            Ok(format!("{base}+{hash}"))
        }
        // A tag on HEAD naming something else. One of the two is wrong and this build cannot know
        // which, so it says so instead of shipping a binary that disagrees with its own tag.
        Some(tag) => Err(format!(
            "HEAD carries the tag {tag:?} but {source} says {base} — a release tag and the version \
             it releases must be the same (ADR-0026)"
        )),
        // No release tag here: a build on the way to `base`, and unmistakably not it.
        None => Ok(format!("{base}-dev+{hash}")),
    }
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
