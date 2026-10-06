# ADR-0009: The version is baked at build time from `Cargo.toml` and git, read through one helper in `crates/opamp`, and compared and shown without its build metadata

- **Status:** 🟢 accepted
- **Date:** 2026-08-10
- **Deciders:** Markus Brigl

## Context

The version a binary carries is load-bearing in several places at once. The OpAMP
`service.version` identifying attribute tells the Server which build of the Client it is talking to;
the CLI prints it via `--version`; the OS-service layout of [ADR-0010](0010-client-os-service-and-installation-layout.md)
names its versioned install directories after it — the identifier the self-update
([ADR-0017](0017-client-self-update-and-its-consent.md)) stages, switches, and rolls back by. The workspace's static
`CARGO_PKG_VERSION` alone says nothing about *which build* of a number is actually running.

Forces:

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
- **The number should be decided once, in the source.** `Cargo.toml` is where a Rust project's
  version lives, and it is a value a reviewer sees in the diff of the release commit rather than in
  a ref nobody reads. A tag typed by hand, at the keyboard, at the end of a working day, is the
  thing that names the release forever — so the second typing is the failure.
- **Two sources drift.** If `Cargo.toml` says one thing and a tag says another, something must
  decide which is the release — and it must not be silence.
- **Both binaries of one workspace must report the same kind of string.** `cargo:rustc-env` reaches
  only the crate whose build script emitted it, so `env!("OPAMP_BUILD_VERSION")` can only be read
  from inside the crate that resolved it. A Server printing `env!("CARGO_PKG_VERSION")` claims to be
  a release it is not and names no commit — the exact failure the `-dev` marker exists to prevent.
  A second copy of the resolution logic (some 120 lines — the tag glob, the strict component
  grammar, the failure modes) in a second build script would be free to drift from the first. Both
  ends already depend on `crates/opamp`, and that crate already owns version *handling* for both.
- **One string carries three different jobs:**

  | Job | What it needs | What the full string gives |
  |---|---|---|
  | *Which release is this?* | `MAJOR.MINOR.PATCH` | `0.1.1+799e36a` |
  | *Which build is on that host?* | the commit, the `-dev` marker | the same string |
  | *What does an operator read in a table?* | something scannable | `0.1.1-dev+799e36a` in a column headed "Version" |

  Requiring the full string where an operator types a version is unusable: `+` in a URL query string
  decodes to a space, so `?version=0.1.1+799e36a` — the obvious way to type the full string — arrives
  as `0.1.1 799e36a` and can never match anything. The correct spelling, `?version=0.1.1%2B799e36a`,
  is not a thing an operator should have to know to ship a release. ADR-0010's version directories
  already split identity from provenance: the directory is named by the bare `MAJOR.MINOR.PATCH`
  base and the commit — "never the pre-release" — while the manifest inside it records the build.
- **The self-update probe is not what proves which bytes arrived.** That is settled before the probe
  runs — ADR-0015 verifies the artifact's content hash on every download, and its Ed25519 signature
  when a key is configured. The probe answers two narrower questions ADR-0017 states plainly: *does
  this binary run at all on this host*, and *is it this program rather than something else offered
  under the same name*. Neither needs a commit hash.
- Versioning is a public contract (artifact names, install directory names, what the fleet sees) —
  costly to change once operators depend on it.

## Decision

We will compute the full version string at compile time — the number from **`[workspace.package]
version` in `Cargo.toml`**, the release-or-development marker and the commit from git — bake it into
every binary through **one helper in `crates/opamp`**, have the **release pipeline create the
`version/*` tag** from the number, and treat a version's **build metadata as provenance**: recorded
and reported everywhere, compared nowhere, and shown only where someone asked for detail.

### The number and its provenance

1. **`Cargo.toml` decides the number.** The base version is `[workspace.package] version`, which
   Cargo hands the build script as `CARGO_PKG_VERSION`. Bumping the version is an ordinary commit,
   reviewed like any other. The base is **strict semantic versioning**: exactly three non-negative
   integers, no leading zeros, no pre-release or build metadata; a malformed version fails the build
   (fail closed) rather than being guessed at. The **`OPAMP_FLEET_VERSION` environment override**,
   if set, states the base instead — the escape hatch for builds without a git checkout (source
   tarballs, distro packaging). It is validated by the same grammar; invalid values fail the build.

2. **Git says only what is *around* the number.** A `version/<base>` tag pointing at HEAD itself
   (exact match, not a nearest reachable tag) means this commit *is* that release. Its absence means
   the build is on the way to it and gets the pre-release **`-dev`**, so a development build reports
   the version it is heading for — `0.1.0-dev+a1b2c3d` — and is unmistakably not that release. Tags
   are parsed against
   `^version/(0|[1-9][0-9]*)(\.|/)(0|[1-9][0-9]*)(\.|/)(0|[1-9][0-9]*)$` — `.` or `/` (mixed
   permitted) as separator — and normalised to dot-separated `MAJOR.MINOR.PATCH`. A `version/*` tag on
   HEAD that names **something else**, or is malformed, **fails the build**: the file and the tag
   disagree and neither wins. With the base in `Cargo.toml`, the file that decides the version is the
   one every binary reads.

3. **Build metadata is always appended:** `+<short-hash>` — the abbreviated commit id of HEAD
   (7 hex characters of the full hash). Nothing time-dependent goes into the string, so rebuilding
   the same commit reproduces the byte-identical version — which also feeds ADR-0010's
   install-directory naming and must therefore not depend on *when* a binary was compiled.
   Examples: `1.2.3+a1b2c3d` (release build), `1.2.3-dev+b4e5f6a` (a build heading for that
   release). Under SemVer, everything after `+` is informational and ignored for precedence, and
   `1.2.3-dev` orders before `1.2.3` — which is simply true of a build heading for `1.2.3`.

4. **Mechanics: a git library in `build.rs`, no `git` binary.** The build script reads the
   repository through the **`git2`** library (the libgit2 bindings, vendored and statically compiled
   — nothing links against a system libgit2) as a **build-dependency**: discover the repository
   upward from the manifest directory (`Repository::discover`), take HEAD's commit id, and check
   which `refs/tags/version/*` point at HEAD. No `git` executable is required at build time, nothing
   parses CLI output, and the result cannot vary with the host's git version or locale.
   Build-dependencies are compiled into the build script only — **nothing of `git2`/libgit2 ends up
   in the shipped binary**, which carries just the resulting string. The build script emits
   `cargo:rustc-env=OPAMP_BUILD_VERSION=<full string>` plus `cargo:rerun-if-changed=.git/HEAD`,
   `cargo:rerun-if-changed=.git/refs` and `cargo:rerun-if-env-changed=OPAMP_FLEET_VERSION`. If the
   override is unset and no repository is found, the build fails with a message naming the
   override.

5. **The resolution runs once, in `crates/opamp/build.rs`, and one helper reads it:
   `opamp::version::current()`.** The resolution logic sits in the shared crate's build script,
   beside the protobuf codegen, because `cargo:rustc-env` reaches only the crate whose build script
   emitted it and both binaries already depend on `opamp`. `current()` returns
   `env!("OPAMP_BUILD_VERSION")` and lives in `opamp::version` beside `parse`, `identity` and
   `same_release`, which is where this project already answers questions about a version string —
   no new module. It is the single version implementation for the whole workspace: no binary crate
   carries a version build script or a version module of its own. `opamp` has `git2` as a
   **build**-dependency only; it is linked into no artifact.

6. **One version everywhere — the CLI included.** Every surface on both ends that states a version
   calls `opamp::version::current()` and therefore always agrees: the OpAMP `service.version`
   identifying attribute, both CLIs' **`--version` output** — a clap-based CLI must wire clap's
   version explicitly to the helper, because clap's built-in default would silently report
   `CARGO_PKG_VERSION` and undo this decision — the self-check token, and the version part of
   ADR-0010's install-directory names (`+` and `.` are legal filename characters on ext4, APFS, and
   NTFS).

### Releasing

7. **The pipeline tags, and a release is building the tagged commit.** The release run reads the
   version, creates `version/<version>` at the commit being released, and pushes it with the
   workflow's own token — which, by GitHub's rule against recursive triggering, starts no second
   run. Only then does it build. No pipeline-side version plumbing decides identity: the build of a
   commit carrying a well-formed `version/<base>` tag is the release.

8. **Drift is refused, never resolved** — and the first refusal is the compiler's:

   | Situation | Answer |
   |---|---|
   | HEAD carries `version/<base>` | a release build: `<base>+<hash>` |
   | HEAD carries a `version/*` tag naming **something else** | **the build fails** — the file and the tag disagree and neither wins |
   | `version/<v>` already exists, or a release names it | **the run fails before it builds** — the number is spent |
   | the run was started by a hand-pushed `version/*` tag | the tag and `Cargo.toml` must agree, or **fail** |

   The third row is checked first, ahead of every build, and it does not care whether the run
   *intends* to publish: whether a version can still be released is a property of the version, so a
   dry run answers it too — which is the run that is meant to find a forgotten bump. Tag and release
   are asked for separately, because either can exist without the other: a draft release reserves a
   tag name that was never pushed, and a tag can be pushed without a release being cut. The one
   exception is the tag a release run was *started* by, which is expected to be there.

   This deliberately gives up re-running a release. Reusing a tag that is already on the commit
   would be harmless in the ordinary case and would let a run that failed in `publish` be repeated —
   but it also means a green run can overwrite artifacts that have already been downloaded, and it
   is the same tolerance under which a forgotten bump releases nothing and says nothing. Recovering
   a half-published release is rare and can be done by hand; catching the spent number is neither.

9. **The released binary is checked after building.** The release workflow must fetch tags, and
   the pipeline checks that the binary reports exactly `<version>+<hash>` — in particular **no
   `-dev` pre-release**, so a shallow clone without tags cannot silently publish a development
   build. This is belt and braces, since the build script has already refused the disagreement it
   would catch.

10. **A dry run neither tags nor publishes.** `workflow_dispatch` takes a `dry-run` input, true by
    default: it builds and packs all targets and stops. A `-dev` version is expected there, and only
    there.

### Comparing and showing

11. **One parser, in the shared crate.** A small helper in `opamp` splits a version string into its
    `MAJOR.MINOR.PATCH` base, an optional pre-release, and optional build metadata. It lives there
    rather than in either end because the Client writes these strings and the Server displays them,
    and ADR-0005 put the shared crate between them precisely so the two cannot drift.

12. **The self-update probe ([ADR-0017](0017-client-self-update-and-its-consent.md)) compares everything except the
    build metadata.** `0.1.1` offered and `0.1.1+799e36a` reported is a match; `0.1.1` offered and
    `0.1.1-dev+799e36a` reported is **not**. Dropping `+<hash>` is what removes the trap, because the
    hash is the part an operator cannot type and does not know at upload time. Keeping the
    pre-release is what preserves the distinction `-dev` exists for: a `-dev` build is a build
    heading for a release and is not that release, and this probe is the last gate that can say so
    before a fleet installs one.

13. **An offered version that has no parseable base fails the install**, naming the value. Fail
    closed: a package version is free-form by the API's own contract, and one that is not a version
    cannot be compared to a version. This is the Client's own package, where the shape is ours.

14. **`?version=` takes the release number.** `0.1.1` is the expected spelling; the full string still
    works, since its base and pre-release are the same. Nothing has to be percent-encoded.

15. **The Server shows the base and the pre-release, and keeps the rest.** `AgentView.service_version`
    is `MAJOR.MINOR.PATCH[-prerelease]` — what belongs in a column headed "Version" — and the
    complete string as reported is in the `service_build` field beside it. The bundled UI shows
    the former in its Version column and the latter on hover, and searches both.

16. **The Client keeps reporting the full string.** `service.version` on the wire carries all of it;
    provenance has to reach the Server for anyone to answer "which build is on that host". What the
    Server puts in a column is not what an Agent says.

17. **Everything written to disk keeps the full string** — the version directory's `manifest.toml`,
    the self-update marker, `--version` output. Those are the internal record this decision relies on
    existing.

## Alternatives considered

- **The `version/*` tag as the only source of the version**, `Cargo.toml` left static — the safer
  arrangement against drift, since there is no second place a version appears and no "forgot to
  bump" release. Weighed and set aside: it keeps the hand-typed tag as the thing that names a
  release, and leaves the number outside the reviewed diff.
- **Leave `build.rs` on the tag and let `Cargo.toml` decide only what the pipeline releases.** The
  smaller change, dropped on the evidence: with `Cargo.toml` at `0.1.0` and no `version/*` tag in the
  repository at all, `client --version` answered `0.0.0-dev`, so the file said one thing and every
  binary built from it said another. A version source no binary reads is not a version source.
- **Have the pipeline write `Cargo.toml` from a tag** — the same coupling in the other direction. It
  needs the pipeline to commit to the branch during a release, which is a worse thing to automate
  than a tag: a tag is immutable and inspectable, a commit changes the history a release was cut
  from.
- **Read `Cargo.toml` but leave the tagging to a human.** Half the change, and it keeps exactly the
  step that motivated it — the hand-typed tag — while adding the file that can disagree with it.
- **Pipeline-resolved version exported as an env var, `CARGO_PKG_VERSION` fallback** — keeps
  `cargo build` free of git access, but leaves every non-pipeline build indistinguishable (`0.1.0`,
  no provenance) and concentrates version logic in CI where developers never exercise it. The env
  var survives as the no-git escape hatch.
- **Raw `git describe` output as the dev version** (the classic `1.2.3-4-ga1b2c3d` convention) —
  encodes lineage too, but its suffix is not SemVer build metadata (it lands in the *pre-release*
  position with unpadded numerics, giving surprising ordering) and duplicates what the `+` metadata
  already carries; the normalised `<base>-dev+<commit>` form keeps one grammar.
- **Shelling out to the `git` binary from `build.rs`** — needs no build-dependency, but makes every
  build depend on a `git` executable being installed and on `PATH` (not a given on minimal CI images,
  build containers, or Windows build boxes) and on parsing its localized CLI output; a library keeps
  the build hermetic. Rejected in favour of the Rust library; worth its own decision only if the
  build dependency ever hurts.
- **The `vergen` family (`vergen-git2`/`vergen-gix`)** — implements exactly this kind of
  `build.rs` stamping, but knows nothing of the `version/*` tag contract, which would still need
  custom code on top of a framework-shaped dependency; using `git2` directly keeps the few queries
  explicit.
- **`gix` (gitoxide) instead of `git2`** — pure Rust and adopted by Cargo, but a large,
  fast-moving crate family whose API still churns; `git2`'s API is small, stable, and libgit2 is
  battle-tested. The C code concern that drives this project's no-system-libraries posture
  (protox not protoc, rustls not OpenSSL — ADR-0006/0007) does not apply: libgit2 is vendored and
  statically built into the *build script only*, never linked from the system and never shipped.
- **A date in the build metadata** (`+<YYYYMMDD>.<hash>`, as commit date; a wall-clock build date
  would additionally break reproducibility) — the commit id already pins the exact source state, and
  git answers "when" for any commit; the date lengthened every identifier while adding no identity.
- **Read the version at runtime** (env var, sidecar file) — the reported version would describe the
  deployment environment, not the binary; a copied binary would change identity. Rejected.
- **Allow pre-release tags** (`version/1.2.3-rc.1`) — SemVer permits them, but strict released
  versions keep the tag grammar unambiguous; deferred until a release-candidate flow is actually
  wanted.
- **A second `build.rs` in `crates/server`** — rejected: it duplicates the tag grammar and the
  failure modes into a place that can drift, to avoid a build dependency the workspace already
  resolves. A rule implemented twice is the failure [ADR-0005](0005-workspace-and-crates.md)
  measures; a version that disagrees between two binaries of one workspace is exactly how it shows.
- **Leave the Server on `CARGO_PKG_VERSION`** — rejected; that is a defect, and it is not cosmetic.
  A support question that starts "which build is this?" cannot be answered from a Server's own
  output, and a release build and a development build of the same number are indistinguishable.
- **A fourth crate holding only build support** — rejected by [ADR-0005](0005-workspace-and-crates.md)'s
  standing rule that more crates are premature until a concrete need appears. One function shared by
  two build scripts is not that need when a shared crate already sits between the two ends.
- **`include!` a shared build-script fragment from both scripts** — rejected. It leaves two build
  scripts, needs `git2` in both manifests anyway, and hides a compiled file from the crate graph, so
  a reader of either manifest cannot see where the code comes from.
- **Keep the probe's exact comparison and document the encoding** (`%2B`) — it treats an unusable
  interface as a training problem. The operator who types the release number is the one behaving
  reasonably.
- **Compare the base only, dropping the pre-release too** — one step further than the problem needs.
  It would let an offer of `0.1.1` be satisfied by a `0.1.1-dev` build, which is exactly the
  confusion `-dev` exists to prevent, at the only gate that inspects a binary before a fleet runs it.
  If the pre-release should be ignored as well, that is a one-line change to the comparison and it
  belongs in this ADR's decision rather than in a later surprise.
- **Have the Client report only the base to the Server** — the smallest change to the display
  problem, and it throws away the answer to "which build is on that host", which is the question a
  fleet exists to answer.
- **Leave `service_version` holding the full string and add a base field beside it** — additive, so
  no API consumer breaks. Rejected for clause 15 because it leaves a field called `service_version`
  holding something nobody would call a version, and because the REST API is at `v1` with no external
  consumer: the moment to give the field its right meaning is before one exists. This is the
  reversible half of the decision, and the CHANGELOG carries it as breaking either way.
- **Compare with a full SemVer precedence implementation** (a `semver` dependency) in the probe — the
  correct answer if the probe ever *ordered* versions. It does not: it asks "is this the one I was
  offered", an equality question over a string this project itself produces.

## Sources / Prior art

- **This repository's `main` lineage** — `main:docs/adr/0008-release-pipeline-and-versioning.md`
  decided the `version/*` tag grammar, strict parsing, normalisation, and compile-time bake-in for
  the supervisor host; `main:crates/supervisor/src/lib.rs` holds its `version()` helper.
- Semantic Versioning 2.0.0 — the `MAJOR.MINOR.PATCH` grammar, pre-release ordering, and rule 10:
  "Build metadata MUST be ignored when determining version precedence… two versions that differ only
  in the build metadata have the same precedence." <https://semver.org/>.
- Cargo build scripts — `cargo:rustc-env`, `rerun-if-changed`, `rerun-if-env-changed`; `rustc-env`
  sets an environment variable "for the compilation of the crate being built", which is the
  constraint that makes clause 5 a decision about *where* the resolution runs rather than a free
  choice of module:
  <https://doc.rust-lang.org/cargo/reference/build-scripts.html#outputs-of-the-build-script>.
- **`cargo metadata`** as the way to read the version, rather than parsing TOML in shell: it answers
  with the resolved package version, so a workspace inheriting `version.workspace = true` is read
  correctly. <https://doc.rust-lang.org/cargo/commands/cargo-metadata.html>
- **GitHub Actions documentation** on recursive triggering: "events triggered by the `GITHUB_TOKEN`
  will not create a new workflow run", so a tag the pipeline pushes cannot start the pipeline again.
  This is what lets one run tag *and* publish.
  <https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow>
- `git2` — Rust bindings to libgit2, with vendored static builds via `libgit2-sys`:
  <https://docs.rs/git2/> and <https://libgit2.org/>. (`gix`/gitoxide, the considered pure-Rust
  alternative: <https://github.com/GitoxideLabs/gitoxide>.)
- `git describe --exact-match` — the CLI semantics "is HEAD itself tagged" that the tag lookup
  reproduces: <https://git-scm.com/docs/git-describe>.
- Reproducible builds — why nothing embedded in a binary should depend on the build clock
  (`SOURCE_DATE_EPOCH`): <https://reproducible-builds.org/docs/source-date-epoch/>.
- `vergen` (the considered off-the-shelf `build.rs` stamper): <https://docs.rs/vergen/>.
- [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986#section-2.2) and the
  `application/x-www-form-urlencoded` convention behind `+` meaning a space in a query string — why
  the full string cannot be typed into a URL unencoded.
- `fleet-packages/opamp-fleet-client.json`: a package registered as `0.1.1` against a binary
  reporting `0.1.1+799e36a`, and an attempt recorded as `0.1.1 799e36a` — a `+` that a query string
  turned into a space.
- [ADR-0010](0010-client-os-service-and-installation-layout.md)'s version-directory naming — the same
  base/provenance split, and the install layout that consumes this version.
- [ADR-0005](0005-workspace-and-crates.md) — the shared crate holds what both ends implement
  identically; one version rule needed by both ends is that case.
- [ADR-0005](0005-workspace-and-crates.md) — one workspace, one lockfile, and the standing
  rejection of further crates.
- Specification goals #10/#11 ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)) — package delivery
  and self-update, both of which identify builds by version.

## Consequences

- Positive: every binary is self-describing — base version, `-dev` pre-release marker, and commit
  hash — so a fleet report, a bug report, or an install directory always names the exact build, on
  both the Client and the Server, by the same rule. The same commit always reproduces the same
  version string. No `git` executable is needed at build time, and the shipped binaries carry only
  the version string — no trace of the git library: `cargo tree -e normal -p server` names no
  `git2`, and neither binary carries a libgit2 symbol.
- Positive: one number, in the file a Rust developer already looks at, changed in a reviewed commit
  rather than typed into a ref. A release is "merge the bump, run the pipeline", and a mistyped tag
  can no longer mint a wrong release, because no human types one.
- Positive: `-dev` reads forwards. A development build says which release it is *heading for*, which
  is what an operator reading a fleet view assumes it means, and `0.1.0-dev` sorting before `0.1.0`
  is simply true.
- Positive: the tag grammar, the release/development distinction and the "a tag that disagrees with
  the file fails the build" rule exist once. A future third surface adopts them by calling a
  function.
- Positive: a release is uploaded under the number it is called, with no encoding lore. The fleet
  table is scannable and still answers the provenance question on hover. The probe keeps the two
  checks it exists for and drops the one it never needed. `-dev` has a place where it is actually
  enforced rather than merely displayed.
- Negative / trade-offs: `git2` is a build-dependency of `opamp` and compiles vendored libgit2 (C)
  for the build script, lengthening cold builds and requiring a C compiler on build hosts — which
  `ring` (ADR-0007) already requires, so no new host prerequisite. `cargo build -p server` alone
  compiles `libgit2`; a whole-workspace build does anyway, so only a Server-only build pays.
- Negative / trade-offs: every commit changes the version, and the version is baked by the crate
  both ends depend on, so a new commit rebuilds `opamp` and therefore both binaries; a Server-only
  build cannot skip it.
- Negative / trade-offs: building outside a git repository (a source tarball) fails unless
  `OPAMP_FLEET_VERSION` is set (fail closed, escape hatch documented — a deliberate choice over
  silently unknown identity); release workflows must fetch tags or they produce `-dev` builds
  (mitigated by the mandated post-build check); the version string is longer and contains `+`, which
  any consumer that parses it must tolerate (SemVer-conformant parsers do).
- Negative / trade-offs: **`Cargo.toml` and the tags are two places a version appears.** Drift is
  caught rather than prevented: a version that was already released fails on the guard, before a
  single target is built, instead of quietly re-releasing. A *forgotten* bump is therefore a failed
  run, which is the outcome to prefer.
- Negative / trade-offs: **a release cannot be re-run.** Once the tag is pushed the number is spent,
  so a run that dies in `publish` — after the tag but before the artifacts — leaves a release that
  has to be finished by hand or a version that has to be skipped. That is the price of the guard
  in clause 8, and it is paid rarely.
- Negative / trade-offs: the release run needs `contents: write` to push a tag. A pipeline that
  can write to the repository is a larger blast radius than one that only reads it, and the token is
  the workflow's own rather than a human's.
- Negative / trade-offs: `AgentView.service_version` holds the base and pre-release inside
  `/api/v1` — a breaking change for any consumer, of which there is one, the bundled UI. Two versions
  that differ only by commit are indistinguishable to the probe, so re-offering a rebuilt artifact of
  the same release is accepted rather than refused; the content hash is what distinguishes those
  bytes.
- Follow-ups: whether the `Cargo.lock` bump that accompanies every version change should be checked
  in CI, so a release cannot be cut from a tree whose lockfile still names the previous version;
  whether the Server, published by no pipeline today, should follow the same release rule when it
  is; and whether the *package* version of a Managed Process (free-form by contract, an upstream
  project's own numbering) should be parsed at all, which is deliberately left alone here.
