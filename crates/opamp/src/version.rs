//! Taking a version string apart (ADR-0029).
//!
//! A version this project produces looks like `1.2.3`, `1.2.3+a1b2c3d`, or `1.2.3-dev+a1b2c3d`
//! (ADR-0009). Three different questions get asked of that one string, and they do not want the
//! same answer:
//!
//! - *Which release is this?* — the base and, when present, the pre-release. A `-dev` build is not
//!   the release it is heading for, so the marker belongs to the answer.
//! - *Which build is this?* — the whole string, commit metadata included.
//! - *What goes in a column headed "Version"?* — the first answer; the second belongs on hover.
//!
//! [`identity`] is the first answer, and it is what the self-update probe compares and what the
//! Server displays. The build metadata is dropped from both because the commit hash is the one part
//! an operator neither knows nor can type when uploading a release — and because SemVer itself says
//! metadata is ignored when versions are compared.
//!
//! This lives in the shared crate rather than in either end: the Client writes these strings and
//! the Server displays them, and ADR-0005 put this crate between them so the two cannot drift.
//!
//! [`current`] is the other half of that: the version *this build* reports. `build.rs` computes it
//! and bakes it in, and every surface on either end reads it here rather than `CARGO_PKG_VERSION`,
//! which knows nothing of tags or commits.

/// The version this build reports, e.g. `1.2.3+a1b2c3d` or `1.2.3-dev+b4e5f6a`.
///
/// The one place either binary states its version (ADR-0009): the OpAMP `service.version`
/// attribute, both CLIs' `--version` output, and the install layout of ADR-0010 all call this.
///
/// It is resolved at compile time by this crate's `build.rs` — the base from `Cargo.toml`
/// (ADR-0026), the `-dev` marker and the commit short-hash from git — so a binary carries the
/// answer rather than looking for a repository that is not there when it runs.
#[must_use]
pub fn current() -> &'static str {
    env!("OPAMP_BUILD_VERSION")
}

/// A version string taken apart. Borrowed from the input — nothing here allocates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version<'a> {
    /// `MAJOR.MINOR.PATCH`, always three numeric components.
    pub base: &'a str,
    /// The pre-release without its `-`, e.g. `dev`; `None` when the version is a release.
    pub prerelease: Option<&'a str>,
    /// The build metadata without its `+`, e.g. the commit short-hash; `None` when absent.
    pub build: Option<&'a str>,
    /// The input up to the build metadata — what [`identity`](Version::identity) returns. Private
    /// so that the only way to hold a `Version` is to have parsed one.
    identity: &'a str,
}

impl<'a> Version<'a> {
    /// What identifies the release: the base, plus the pre-release when there is one.
    ///
    /// A slice of the input rather than a rebuilt string, so it cannot differ from what was parsed.
    #[must_use]
    pub fn identity(&self) -> &'a str {
        self.identity
    }
}

/// Splits a version string into base, pre-release, and build metadata.
///
/// Returns `None` when the string does not begin with three dot-separated numeric components —
/// which is a refusal, not a fallback: a value that is not a version cannot be compared to one.
/// Leading zeros are rejected the way SemVer rejects them, so `01.2.3` is not a version.
///
/// The pre-release and build metadata are held to the SemVer character set — dot-separated,
/// non-empty identifiers of `[0-9A-Za-z-]`. That is not pedantry: this string becomes a directory
/// name in the self-update layout (ADR-0010), so a value carrying `/`, `\`, or a bare `..` is both
/// not a version *and* a path-traversal waiting to happen. Refusing it here is the single gate the
/// rest of the project trusts.
#[must_use]
pub fn parse(raw: &str) -> Option<Version<'_>> {
    let (without_build, build) = match raw.split_once('+') {
        Some((left, right)) if !right.is_empty() => (left, Some(right)),
        Some(_) => return None, // a trailing `+` with nothing after it
        None => (raw, None),
    };
    let (base, prerelease) = match without_build.split_once('-') {
        Some((left, right)) if !right.is_empty() => (left, Some(right)),
        Some(_) => return None, // a trailing `-` with nothing after it
        None => (without_build, None),
    };

    let mut components = base.split('.');
    for _ in 0..3 {
        let component = components.next()?;
        if component.is_empty() || !component.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        if component.len() > 1 && component.starts_with('0') {
            return None;
        }
    }
    if components.next().is_some() {
        return None;
    }

    if prerelease.is_some_and(|pre| !is_dot_identifiers(pre))
        || build.is_some_and(|meta| !is_dot_identifiers(meta))
    {
        return None;
    }

    Some(Version {
        base,
        prerelease,
        build,
        identity: without_build,
    })
}

/// Whether `field` is one or more dot-separated, non-empty SemVer identifiers — each drawn only
/// from ASCII letters, digits, and `-`. A `/`, `\`, `.`-only, or empty identifier fails, which is
/// what keeps a parsed version from ever naming a path outside its layout.
fn is_dot_identifiers(field: &str) -> bool {
    field
        .split('.')
        .all(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

/// The identifying part of a version string — everything except the build metadata — or `None`
/// when the value is not a version (ADR-0029).
///
/// `1.2.3+a1b2c3d` yields `1.2.3`; `1.2.3-dev+a1b2c3d` yields `1.2.3-dev`.
#[must_use]
pub fn identity(raw: &str) -> Option<&str> {
    parse(raw).map(|v| v.identity())
}

/// Whether two version strings name the same release, ignoring which build produced each.
///
/// `false` when either side is not a version: the self-update probe fails closed rather than
/// installing something it could not check.
#[must_use]
pub fn same_release(a: &str, b: &str) -> bool {
    match (identity(a), identity(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Orders two versions by SemVer precedence (SemVer §11): the numeric base first, then the
/// pre-release rules — a pre-release has lower precedence than the release it heads for, and
/// pre-release identifiers compare field by field. Build metadata is ignored (SemVer §10), so two
/// builds of the same release compare `Equal`.
///
/// `None` when either side is not a version. The self-update's anti-downgrade check relies on this:
/// a value it cannot order is one it must not install over what is running.
#[must_use]
pub fn precedence(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    Some(compare(&parse(a)?, &parse(b)?))
}

fn compare(a: &Version, b: &Version) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    // The base is three numeric identifiers; parse() already proved each is digits with no leading
    // zero, so "longer is larger, equal length compares lexically" orders them without parsing into
    // an integer that a pathologically long component could overflow.
    for (x, y) in a.base.split('.').zip(b.base.split('.')) {
        match cmp_numeric(x, y) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    match (a.prerelease, b.prerelease) {
        (None, None) => Ordering::Equal,
        // A release outranks any pre-release of the same base (1.0.0 > 1.0.0-rc.1).
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => compare_prerelease(x, y),
    }
}

fn compare_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let (mut ai, mut bi) = (a.split('.'), b.split('.'));
    loop {
        return match (ai.next(), bi.next()) {
            (None, None) => Ordering::Equal,
            // Fewer fields loses when all the shared ones are equal (rc < rc.1).
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(x), Some(y)) => match compare_identifier(x, y) {
                Ordering::Equal => continue,
                other => other,
            },
        };
    }
}

/// One pre-release identifier: numeric ones compare numerically and rank below alphanumeric ones;
/// alphanumeric ones compare in ASCII order (SemVer §11.4).
fn compare_identifier(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (is_numeric(a), is_numeric(b)) {
        (true, true) => cmp_numeric(a, b),
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.cmp(b),
    }
}

fn is_numeric(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}

/// Two digit strings with no leading zeros: the longer is the larger, and equal lengths compare
/// lexically — a numeric compare that cannot overflow.
fn cmp_numeric(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_apart_the_shapes_this_project_produces() {
        let release = parse("1.2.3").expect("a release");
        assert_eq!(release.base, "1.2.3");
        assert_eq!(release.prerelease, None);
        assert_eq!(release.build, None);
        assert_eq!(release.identity(), "1.2.3");

        let tagged = parse("1.2.3+a1b2c3d").expect("a release build");
        assert_eq!(tagged.build, Some("a1b2c3d"));
        assert_eq!(tagged.identity(), "1.2.3");

        let dev = parse("0.1.1-dev+799e36a").expect("a development build");
        assert_eq!(dev.base, "0.1.1");
        assert_eq!(dev.prerelease, Some("dev"));
        assert_eq!(dev.build, Some("799e36a"));
        assert_eq!(dev.identity(), "0.1.1-dev");
    }

    /// The failure that prompted ADR-0029: a package uploaded as the release number against a
    /// binary that reports the commit it was built from.
    #[test]
    fn a_release_matches_the_build_that_carries_it() {
        assert!(same_release("0.1.1", "0.1.1+799e36a"));
        assert!(same_release("0.1.1+799e36a", "0.1.1"));
        // Two builds of the same release are the same release; which bytes arrived is the content
        // hash's question, not this one.
        assert!(same_release("0.1.1+799e36a", "0.1.1+deadbee"));
    }

    /// SemVer precedence (§11), the ordering the self-update's anti-downgrade check reads.
    #[test]
    fn precedence_orders_versions_by_semver_rules() {
        use std::cmp::Ordering;

        // Numeric base, component by component.
        assert_eq!(precedence("1.0.0", "2.0.0"), Some(Ordering::Less));
        assert_eq!(precedence("1.2.0", "1.10.0"), Some(Ordering::Less));
        assert_eq!(precedence("1.0.10", "1.0.2"), Some(Ordering::Greater));
        assert_eq!(precedence("1.2.3", "1.2.3"), Some(Ordering::Equal));

        // Build metadata is ignored: two builds of a release are equal.
        assert_eq!(
            precedence("1.2.3+aaaaaaa", "1.2.3+bbbbbbb"),
            Some(Ordering::Equal)
        );

        // A pre-release ranks below the release it heads for, and pre-releases order field by field.
        assert_eq!(precedence("1.0.0-rc.1", "1.0.0"), Some(Ordering::Less));
        assert_eq!(
            precedence("1.0.0-alpha", "1.0.0-beta"),
            Some(Ordering::Less)
        );
        assert_eq!(precedence("1.0.0-rc", "1.0.0-rc.1"), Some(Ordering::Less));
        assert_eq!(
            precedence("1.0.0-alpha.2", "1.0.0-alpha.10"),
            Some(Ordering::Less)
        );
        assert_eq!(precedence("1.0.0-2", "1.0.0-alpha"), Some(Ordering::Less));

        // A value that is not a version cannot be ordered.
        assert_eq!(precedence("1.0.0", "latest"), None);
        assert_eq!(precedence("", "1.0.0"), None);
    }

    /// The distinction the pre-release exists for (ADR-0009), kept at the gate that can enforce it.
    #[test]
    fn a_development_build_is_not_the_release_it_heads_for() {
        assert!(!same_release("0.1.1", "0.1.1-dev+799e36a"));
        assert!(!same_release("0.1.1-dev", "0.1.1"));
        assert!(same_release("0.1.1-dev+799e36a", "0.1.1-dev+deadbee"));
    }

    #[test]
    fn different_releases_never_match() {
        assert!(!same_release("0.1.1", "0.1.2"));
        assert!(!same_release("1.0.0", "0.1.0"));
    }

    /// A package version is free-form by the API's own contract, so this parser meets values that
    /// are not versions at all. It says so rather than guessing.
    #[test]
    fn what_is_not_a_version_is_refused() {
        for not_a_version in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "1.2.x",
            "latest",
            "1.2.-3",
            "1.2.3-",
            "1.2.3+",
            " 1.2.3",
            "1.2.3 799e36a", // the `+` a query string turned into a space
        ] {
            assert!(parse(not_a_version).is_none(), "{not_a_version:?} parsed");
            assert!(identity(not_a_version).is_none(), "{not_a_version:?}");
        }
        // And a comparison against one is false rather than an accident.
        assert!(!same_release("0.1.1", "0.1.1 799e36a"));
        assert!(!same_release("latest", "latest"));
    }

    /// The self-update turns this string into a directory name (ADR-0010), so a pre-release or
    /// build metadata that smuggles a path separator or `..` is not a version. Parsing it away is
    /// the gate that stops a crafted Server offer from escaping `versions/`.
    #[test]
    fn a_version_that_would_escape_a_path_is_refused() {
        for traversal in [
            "1.0.0+../../../evil",
            "1.0.0-../x",
            "1.0.0+a/b",
            r"1.0.0+a\b",
            "1.0.0+..",
            "1.0.0-.",
            "1.0.0+a..b",
            "1.0.0+a.",
        ] {
            assert!(
                parse(traversal).is_none(),
                "{traversal:?} parsed as a version"
            );
            assert!(identity(traversal).is_none(), "{traversal:?}");
        }
        // The shapes this project actually produces still parse — the tightening rejects only what
        // was never a valid identifier to begin with.
        assert!(parse("1.2.3+a1b2c3d").is_some());
        assert!(parse("0.1.1-dev+799e36a").is_some());
        assert!(parse("1.2.3-rc.1+build.7").is_some());
    }

    /// The baked string has the shape ADR-0009 prescribes — and, now that both live here, it is
    /// checked with the parser the rest of the project judges it by rather than a second grammar.
    #[test]
    fn the_baked_version_has_the_adr_0009_shape() {
        let full = current();
        let parsed = parse(full).unwrap_or_else(|| panic!("{full:?} is not a version"));
        assert!(
            matches!(parsed.prerelease, None | Some("dev")),
            "{full:?}: an unexpected pre-release"
        );
        // Builds inside a repository (all local and CI builds) carry the commit short-hash.
        if let Some(metadata) = parsed.build {
            assert_eq!(metadata.len(), 7, "{full:?}: metadata is not a short hash");
            assert!(metadata.bytes().all(|b| b.is_ascii_hexdigit()));
        }
    }

    /// SemVer rejects leading zeros, and so does the version `build.rs` produces.
    #[test]
    fn leading_zeros_are_not_a_version() {
        assert!(parse("01.2.3").is_none());
        assert!(parse("1.02.3").is_none());
        assert!(parse("0.1.1").is_some(), "a bare zero component is fine");
    }
}
