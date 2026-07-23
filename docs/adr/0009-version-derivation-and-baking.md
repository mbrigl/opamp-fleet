# ADR-0009: Version computed from git in `build.rs` — strict SemVer from `version/*` tags, `-dev` pre-release for non-release builds, commit-hash build metadata

- **Status:** 🟢 accepted
- **Date:** 2026-07-23
- **Deciders:** Markus Brigl

## Context

The version a binary carries is about to become load-bearing in three places at once. The OpAMP
`service.version` identifying attribute tells the Server which build of the Client it is talking to
(`crates/client/src/agent.rs` reports it today); the CLI prints it via `--version`; and the OS-service
work proposed alongside this decision ([ADR-0010](0010-client-os-service-and-cli.md)) names its
versioned install directories after it — the identifier a future self-update stages, switches, and
rolls back by. All three currently would report the workspace's static `CARGO_PKG_VERSION`
(`0.1.0`), which nobody maintains per release and which says nothing about *which build* of `0.1.0`
is actually running.

Forces:

- **The version must derive from the repository, not from a hand-maintained file.** The
  maintainer's convention is a git tag `version/<major><sep><minor><sep><patch>` (separator `.` or
  `/`). Bumping `Cargo.toml` per release would create a second source of truth that drifts from
  the tags.
- **Every build — not only release builds — must be identifiable.** A fleet Server, a bug report,
  or an install directory must distinguish two development builds from each other and any
  development build from a release. That demands two things of the version string: a SemVer
  **pre-release marker** that makes non-releases unmistakable, and **build metadata** carrying the
  provenance (the exact commit) of the build.
- **The resolved version must be available at compile time** so it can be baked into the binary —
  a runtime lookup (reading git, an env var, or a file next to the binary) would make the reported
  version depend on the deployment environment instead of the build.
- **A plain `cargo build` in a git clone must keep working with no tags and no setup** — only a
  build that claims to be a release may fail closed on version resolution.
- Versioning is a public contract (artifact names, install directory names, what the fleet sees) —
  costly to change once operators depend on it, hence per `AGENTS.md` §3 this ADR. The `main`
  lineage of this repository proved the tag grammar and the compile-time bake-in
  (`main:docs/adr/0008-release-pipeline-and-versioning.md`); this ADR adopts that contract but
  moves the computation into the Rust build itself and stamps *every* build, so the release
  *pipeline* (build targets, archives, checksums, publishing) — deliberately left to its own
  follow-up decision — carries no version logic beyond checking out the right commit.

## Decision

We will compute the full version string in the binary crate's **`build.rs`** at compile time, from
git, and bake it into the binary; every build — release or not — is stamped with its provenance.

- **Base version, resolved in `build.rs`, first match wins:**
  1. **`OPAMP_FLEET_VERSION` environment override**, if set — the escape hatch for builds without
     a git checkout (source tarballs, distro packaging). Validated like a tag (below); invalid
     values fail the build.
  2. **A `version/*` tag pointing at HEAD itself** (exact match, not a nearest reachable tag),
     parsed against `^version/(0|[1-9][0-9]*)(\.|/)(0|[1-9][0-9]*)(\.|/)(0|[1-9][0-9]*)$` —
     exactly three non-negative integers under **strict semantic versioning**: no leading zeros,
     `.` or `/` (mixed permitted) as separator, no pre-release or build metadata — and normalised
     to dot-separated `MAJOR.MINOR.PATCH`. A *malformed* `version/*` tag on HEAD **fails the
     build** (fail closed) rather than being skipped or guessed at.
  3. **The most recent `version/*` tag *reachable* from HEAD, with the pre-release `-dev`
     appended** (git-describe semantics; same strict parsing and normalisation) — a development
     build descending from the `version/1.2.3` release identifies as `1.2.3-dev`, so its release
     lineage is visible at a glance while the SemVer pre-release `-dev` still marks it
     unmistakably as *not* that release. If **no** `version/*` tag is reachable at all (a fresh
     history — this repository today), the base falls back to **`0.0.0-dev`**.
- **Build metadata is always appended:** `+<short-hash>` — the abbreviated commit id of HEAD
  (7 hex characters of the full hash). Nothing time-dependent goes into the string, so rebuilding
  the same commit reproduces the byte-identical version — which also feeds ADR-0010's
  install-directory naming and must therefore not depend on *when* a binary was compiled.
  Examples: `1.2.3+a1b2c3d` (release build), `1.2.3-dev+b4e5f6a` (a build descending from that
  release), `0.0.0-dev+a1b2c3d` (no release tag reachable). Under SemVer, everything after `+` is
  informational and ignored for precedence — version comparisons (a future self-update) compare
  only the base and pre-release parts. One ordering nuance is deliberate and must be respected
  there: SemVer orders `1.2.3-dev` *before* `1.2.3`, although the dev build descends from (is
  newer than) the release — `-dev` versions are provenance labels, not points on the upgrade path,
  and a future self-update must never treat a `-dev` build as an upgrade source or target for
  ordering decisions; among dev builds, the commit in the metadata identifies but does not order
  them.
- **Release = building a commit that carries a well-formed `version/*` tag.** No pipeline-side
  version plumbing decides identity; the release workflow simply checks out the tag and builds —
  and must fetch tags and **assert the produced binary reports no `-dev` pre-release**, so a
  shallow clone without tags cannot silently publish a development build.
- **Mechanics: a git library in `build.rs`, no `git` binary.** `build.rs` reads the repository
  through the **`git2`** library (the libgit2 bindings, vendored and statically compiled — nothing
  links against a system libgit2) as a **build-dependency**: discover the repository upward from
  the manifest directory (`Repository::discover`), take HEAD's commit id, check which
  `refs/tags/version/*` point at HEAD, and otherwise resolve the most recent reachable one
  (`git2`'s describe API with a `version/*` pattern — the library equivalent of `git describe`).
  No `git` executable is required at build time,
  nothing parses CLI output, and the result cannot vary with the host's git version or locale.
  Build-dependencies are compiled into the build script only — **nothing of `git2`/libgit2 ends up
  in the shipped binary**, which carries just the resulting string. `build.rs` emits
  `cargo:rustc-env=OPAMP_BUILD_VERSION=<full string>` plus `cargo:rerun-if-changed=.git/HEAD`,
  `cargo:rerun-if-changed=.git/refs` and `cargo:rerun-if-env-changed=OPAMP_FLEET_VERSION`. If the
  override is unset and no repository is found, the build fails with a message naming the
  override. The binary reads the result through one helper — `crates/client/src/version.rs`,
  `pub fn version() -> &'static str { env!("OPAMP_BUILD_VERSION") }`.
- **One version everywhere — the CLI included from the start.** Every surface that states a
  version calls `version()` and therefore always agrees: the OpAMP `service.version` identifying
  attribute, the **CLI `--version` output** — today's hand-rolled flag switches to `version()`
  immediately, and the subcommand CLI proposed in [ADR-0010](0010-client-os-service-and-cli.md)
  must wire clap's version explicitly to `version()` (clap's built-in default would silently
  report `CARGO_PKG_VERSION` and undo this decision) — and the version part of ADR-0010's
  install-directory names (`+` and `.` are legal filename characters on ext4, APFS, and NTFS).
  The Server adopts the same `build.rs` and helper when its builds need it.

## Alternatives considered

- **Version from `Cargo.toml`, bumped per release** — the common Rust convention, but it makes the
  version a hand-maintained file that must be kept in sync with tags; the tag as single source of
  truth keeps `Cargo.toml` stable and removes a whole class of "forgot to bump" releases.
- **Pipeline-resolved version exported as an env var, `CARGO_PKG_VERSION` fallback** (the `main`
  lineage's mechanism, and this ADR's own first draft) — keeps `cargo build` free of git access,
  but leaves every non-pipeline build indistinguishable (`0.1.0`, no provenance) and concentrates
  version logic in CI where developers never exercise it. Superseded by computing in `build.rs`;
  the env var survives as the no-git escape hatch.
- **A fixed `0.0.0-dev` base for every non-release build** (this ADR's earlier draft) — makes
  non-releases maximally uniform, but hides which release line a development build descends from,
  information git already has. Rejected per maintainer direction: the reachable-tag base plus
  `-dev` carries the lineage and the pre-release marker still separates it from the release; the
  fixed base survives only as the no-tag-reachable fallback.
- **Raw `git describe` output as the dev version** (the classic `1.2.3-4-ga1b2c3d` convention) —
  encodes lineage too, but its suffix is not SemVer build metadata (it lands in the *pre-release*
  position with unpadded numerics, giving surprising ordering) and duplicates what the `+` metadata
  already carries; the normalised `<base>-dev+<date>.<commit>` form keeps one grammar.
- **Shelling out to the `git` binary from `build.rs`** (this ADR's second draft) — needs no
  build-dependency, but makes every build depend on a `git` executable being installed and on
  `PATH` (not a given on minimal CI images, build containers, or Windows build boxes) and on
  parsing its localized CLI output; a library keeps the build hermetic. Rejected per maintainer
  direction in favour of the Rust library.
- **The `vergen` family (`vergen-git2`/`vergen-gix`)** — implements exactly this kind of
  `build.rs` stamping, but knows nothing of the `version/*` tag contract, which would still need
  custom code on top of a framework-shaped dependency; using `git2` directly keeps the few queries
  explicit.
- **`gix` (gitoxide) instead of `git2`** — pure Rust and adopted by Cargo, but a large,
  fast-moving crate family whose API still churns; `git2`'s API is small, stable, and libgit2 is
  battle-tested. The C code concern that drives this project's no-system-libraries posture
  (protox not protoc, rustls not OpenSSL — ADR-0006/0007) does not apply: libgit2 is vendored and
  statically built into the *build script only*, never linked from the system and never shipped.
  Rejected per maintainer direction in favour of `git2`.
- **A date in the build metadata** (`+<YYYYMMDD>.<hash>`, this ADR's earlier draft — as commit
  date; a wall-clock build date would additionally break reproducibility) — the commit id already
  pins the exact source state, and git answers "when" for any commit; the date lengthened every
  identifier while adding no identity. Removed per maintainer direction.
- **Read the version at runtime** (env var, sidecar file) — the reported version would describe the
  deployment environment, not the binary; a copied binary would change identity. Rejected.
- **Allow pre-release tags** (`version/1.2.3-rc.1`) — SemVer permits them, but strict released
  versions keep the tag grammar unambiguous; deferred until a release-candidate flow is actually
  wanted.

## Sources / Prior art

- **This repository's `main` lineage** — `main:docs/adr/0008-release-pipeline-and-versioning.md`
  decided the `version/*` tag grammar, strict parsing, normalisation, and compile-time bake-in for
  the supervisor host; `main:crates/supervisor/src/lib.rs` holds its `version()` helper. This ADR
  keeps that contract and moves the computation from the pipeline into `build.rs`.
- Semantic Versioning 2.0.0 — the `MAJOR.MINOR.PATCH` grammar, pre-release ordering, and the rule
  that build metadata (`+…`) is ignored for precedence: <https://semver.org/>.
- Cargo build scripts — `cargo:rustc-env`, `rerun-if-changed`, `rerun-if-env-changed`:
  <https://doc.rust-lang.org/cargo/reference/build-scripts.html>.
- `git2` — Rust bindings to libgit2, with vendored static builds via `libgit2-sys`:
  <https://docs.rs/git2/> and <https://libgit2.org/>. (`gix`/gitoxide, the considered pure-Rust
  alternative: <https://github.com/GitoxideLabs/gitoxide>.)
- `git describe --exact-match` — the CLI semantics "is HEAD itself tagged" that the tag lookup
  reproduces: <https://git-scm.com/docs/git-describe>.
- Reproducible builds — why nothing embedded in a binary should depend on the build clock
  (`SOURCE_DATE_EPOCH`): <https://reproducible-builds.org/docs/source-date-epoch/>.
- `vergen` (the considered off-the-shelf `build.rs` stamper): <https://docs.rs/vergen/>.
- Specification goals #10/#11 ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)) — package delivery
  and self-update, both of which identify builds by version;
  [ADR-0010](0010-client-os-service-and-cli.md), whose install layout consumes this version.

## Consequences

- Positive: every binary is self-describing — base version, `-dev` pre-release marker, and commit
  hash — so a fleet report, a bug report, or an install directory always names the exact
  build; releases need zero version plumbing in CI (check out the tag, build, assert non-dev); the
  same commit always reproduces the same version string; the tag stays the single source of truth;
  `Cargo.toml` stays stable; no `git` executable is needed at build time, and the shipped binaries
  carry only the version string — no trace of the git library.
- Negative / trade-offs: `git2` enters as a build-dependency and compiles vendored libgit2 (C) for
  the build script, lengthening cold builds and requiring a C compiler on build hosts — which
  `ring` (ADR-0007) already requires, so no new host prerequisite;
  building outside a git repository (a source tarball) fails unless `OPAMP_FLEET_VERSION` is set
  (fail closed, escape hatch documented — a deliberate choice over silently unknown identity);
  release workflows must fetch tags or they produce `-dev` builds (mitigated by the mandated
  non-dev assertion); the version string is longer and contains `+`, which any consumer that
  parses it must tolerate (SemVer-conformant parsers do); sibling binaries (Server later) must
  adopt the same `build.rs` rather than calling `env!("CARGO_PKG_VERSION")` directly.
- Follow-ups: the **release pipeline ADR** (workflow trigger, build targets, artifact naming,
  checksums, publishing, and the assert-non-dev check decided here); the **self-update ADR**
  anticipated by ADR-0010, which will compare and stage exactly these versions and must ignore
  build metadata for precedence per SemVer.
