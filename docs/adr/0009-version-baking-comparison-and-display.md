# ADR-0009: The version — strict SemVer baked in at compile time, `-dev` for non-release builds, commit-hash build metadata that is provenance, and one helper every surface reads

- **Status:** 🟢 accepted
- **Date:** 2026-07-23
- **Deciders:** Markus Brigl

Point 10 replaces the probe comparison of
[ADR-0018](0018-client-self-update.md) — the staged binary had to report the offered version
*character for character* — and point 12 replaces the consequence
[ADR-0021](0021-release-pipeline-and-artifacts.md) drew from it, that `?version=` "must therefore be
the *full* baked string". Nothing else in those two ADRs changes.

## Context

### What a version is needed for

The version a binary carries is load-bearing in three places at once. The OpAMP
`service.version` identifying attribute tells the Server which build of the Client it is talking to
(`crates/client/src/agent.rs` reports it); the CLI prints it via `--version`; and the OS-service
work ([ADR-0010](0010-client-os-service.md)) names its versioned install directories after
it — the identifier a self-update stages, switches, and rolls back by. The workspace's static
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
- Versioning is a public contract (artifact names, install directory names, what the fleet sees) —
  costly to change once operators depend on it, hence per `AGENTS.md` §3 this ADR. The `main`
  lineage of this repository proved the tag grammar and the compile-time bake-in
  (`main:docs/adr/0008-release-pipeline-and-versioning.md`); this decision adopts that contract but
  moves the computation into the Rust build itself and stamps *every* build, so the release
  *pipeline* (build targets, archives, checksums, publishing) carries no version logic beyond
  creating the tag from `Cargo.toml` and checking out the right commit.

### Where the number comes from

`Cargo.toml` is where a Rust project's version lives. It is a value a reviewer sees in the diff of
the release commit rather than in a ref nobody reads. The number should be decided once, in the
source, and not typed a second time as a tag. The second typing is the failure: a tag is written by
hand, at the keyboard, at the end of a working day, and it is the thing that names the release
forever. With the tag as the only source, a release is two acts by a human — bump nothing, tag the
commit, push the tag — on top of the release pipeline of
[ADR-0021](0021-release-pipeline-and-artifacts.md).

The forces that favour a tag as the only source have not disappeared, and the decision below has to
answer them rather than ignore them:

- **Two sources drift.** If `Cargo.toml` says one thing and a tag says another, something must
  decide which is the release — and it must not be silence.
- **The binary's identity may not become a build-environment property.** `build.rs` resolves the
  version from the repository at compile time and bakes it in. Reading `Cargo.toml` at build time
  alone would work, but the version would stop carrying the provenance (`+<hash>`, `-dev`) that
  ADR-0010's install directories and ADR-0018's self-update rely on, and every one of those rules
  would have to be re-decided.
- **The self-update compares the version it was offered.** `selfupdate::probe` refuses a staged
  binary whose reported version is not the offered one (point 10 says how far).

### Comparing and showing a version

An exact comparison met its first operator and lost. A Client package was uploaded as `0.1.1`, the
binary in it reports `0.1.1+799e36a`, and the install never happened. The store still holds the
fingerprint of the attempt to fix it by hand:

```json
{ "version": "0.1.1", "previous": { "version": "0.1.1 799e36a" } }
```

That space is not a typo. `+` in a URL query string decodes to a space, so
`?version=0.1.1+799e36a` — the obvious way to type the full string — arrives as
`0.1.1 799e36a` and can never match anything. The correct spelling is `?version=0.1.1%2B799e36a`,
which is not a thing an operator should have to know to ship a release.

So an exact comparison is unusable in the one place it is used. But the deeper problem is that
**one string is carrying three different jobs**:

| Job | What it needs | What one string gives it |
|---|---|---|
| *Which release is this?* | `MAJOR.MINOR.PATCH` | `0.1.1+799e36a` |
| *Which build is on that host?* | the commit, the `-dev` marker | the same string |
| *What does an operator read in a table?* | something scannable | `0.1.1-dev+799e36a` in a column headed "Version" |

The codebase already resolves this tension once, in the other direction. ADR-0010's version
directories are `opamp-client-<MAJOR.MINOR.PATCH>-<hash>` — deliberately "never the pre-release" —
because a *directory* names a version of a component while the manifest inside it records the build.
The identity is the base there, and the provenance sits beside it. This decision applies the same
split to the two places that would otherwise conflate them.

One thing the probe is **not** for, which is what makes relaxing it safe: proving *which bytes*
arrived. That is settled before the probe runs — ADR-0015 verifies the artifact's content
hash on every download, and its Ed25519 signature when a key is configured. The probe answers two
narrower questions ADR-0018 states plainly: *does this binary run at all on this host*, and *is it
this program rather than something else offered under the same name*. Neither needs a commit hash.

### Where the helper lives

Every surface reporting a version reads one helper, never `CARGO_PKG_VERSION`, which knows nothing
of tags or commits. The Server did not read it: `crates/server/src/main.rs` printed
`env!("CARGO_PKG_VERSION")`, so on the same commit the two binaries of one workspace disagreed about
what they were:

```console
$ server --version
server 0.1.3
$ opamp-fleet-client --version
opamp-fleet-client 0.1.3-dev+ade2775
```

The number in `Cargo.toml` is the release a build is *heading for* (point 3), so the
Server's output claimed to be a release it was not, and named no commit. That is the exact failure
the `-dev` marker exists to prevent — "a build heading for a release is unmistakably not it" —
reported by the one binary that had opted out of the mechanism.

The obstacle is mechanical rather than architectural: `cargo:rustc-env` reaches only the crate whose
build script emitted it, so `env!("OPAMP_BUILD_VERSION")` can only be read from inside the crate
that resolved it. Adopting "the same `build.rs`" in the Server, taken literally, means a second
copy of some 120 lines — the tag glob, the strict component grammar, the three failure modes — in a
second build script, free to drift from the first. That is the shape
[ADR-0020](0020-what-lives-in-which-crate.md) measures against.

Both ends depend on `crates/opamp`, and that crate owns version *handling* for both of them:
`opamp::version::parse`, `identity`, and `same_release` (point 9) are there precisely
because "the Client writes these strings and the Server displays them". What was missing was the
string itself.

## Decision

We will compute the full version string in
a **`build.rs`**
at compile time, from git, and bake it into the binary; every build — release or not — is stamped
with its provenance.

We will take the release version from **`[workspace.package] version` in `Cargo.toml`** and have the
**release pipeline create the `version/*` tag from it** — before it builds, so nothing downstream
changes.

We will treat a version's **build metadata as provenance** — recorded and reported everywhere,
compared nowhere, and shown only where someone asked for detail.

We will resolve the baked version in **`crates/opamp/build.rs`** and expose it as
**`opamp::version::current()`** — the one helper both binaries read.

### The number and the release

1. **`Cargo.toml` decides the number.** Bumping the version is an ordinary commit, reviewed like
   any other.

2. **The pipeline tags.** The release run reads the version, creates `version/<version>` at the
   commit being released, and pushes it with the workflow's own token — which, by GitHub's rule
   against recursive triggering, starts no second run. Only then does it build.

3. **`build.rs` takes the base from the same file, and git says only what is *around* it.** The base
   is `CARGO_PKG_VERSION`, which Cargo hands the build script from `Cargo.toml`; a `version/<base>`
   tag on HEAD means this commit *is* that release, and its absence means the build is on the way to
   it and gets the `-dev` pre-release. The commit short-hash is appended (point 6). So a
   development build reports `0.1.0-dev+a1b2c3d` — the version it is heading for, not the version
   it descends *from*. The `OPAMP_FLEET_VERSION` override, validated as strict `MAJOR.MINOR.PATCH`,
   stays the escape hatch that states the base for builds without a git checkout.

   This is the part that reaches furthest, and it is the point of taking the number from
   `Cargo.toml`: with the base in the tag, `Cargo.toml` could say `0.1.0` while every binary built
   from it said `0.0.0-dev`, and the file that supposedly decides the version would be one no
   binary reads.

4. **Drift is refused, never resolved** — and the first refusal is the compiler's:

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

   And after building, the pipeline checks that the binary reports exactly `<version>+<hash>` — belt
   and braces, since `build.rs` has already refused the disagreement it would catch.

5. **A dry run neither tags nor publishes.** `workflow_dispatch` takes a `dry-run` input, true by
   default: it builds and packs all targets and stops. A `-dev` version is expected there, and only
   there.

### How it is stamped and read

6. **Build metadata is always appended:** `+<short-hash>` — the abbreviated commit id of HEAD
   (7 hex characters of the full hash). Nothing time-dependent goes into the string, so rebuilding
   the same commit reproduces the byte-identical version — which also feeds ADR-0010's
   install-directory naming and must therefore not depend on *when* a binary was compiled.
   Examples: `1.2.3+a1b2c3d` (release build), `1.2.3-dev+b4e5f6a` (a build heading for that
   release). Under SemVer, everything after `+` is informational and ignored for precedence —
   version comparisons compare only the base and pre-release parts. SemVer orders `1.2.3-dev`
   *before* `1.2.3`, which is simply true of a build heading for that release; among dev builds,
   the commit in the metadata identifies but does not order them.

7. **Mechanics: a git library in `build.rs`, no `git` binary.** `build.rs` reads the repository
   through the **`git2`** library (the libgit2 bindings, vendored and statically compiled — nothing
   links against a system libgit2) as a **build-dependency**: discover the repository upward from
   the manifest directory (`Repository::discover`), take HEAD's commit id,
   and check which `refs/tags/version/*` point at HEAD.
   No `git` executable is required at build time,
   nothing parses CLI output, and the result cannot vary with the host's git version or locale.
   Build-dependencies are compiled into the build script only — **nothing of `git2`/libgit2 ends up
   in the shipped binary**, which carries just the resulting string. `build.rs` emits
   `cargo:rustc-env=OPAMP_BUILD_VERSION=<full string>` plus `cargo:rerun-if-changed=.git/HEAD`,
   `cargo:rerun-if-changed=.git/refs` and `cargo:rerun-if-env-changed=OPAMP_FLEET_VERSION`. If the
   override is unset and no repository is found, the build fails with a message naming the
   override.
   The binaries read the result through **one** helper —
   `pub fn current() -> &'static str { env!("OPAMP_BUILD_VERSION") }` — and that helper is the
   single version implementation for the whole workspace. That it exists exactly once is the
   decision; points 16–19 say where it lives.

8. **One version everywhere.** Every surface that states a version calls
   `opamp::version::current()` and therefore always agrees: the OpAMP `service.version` identifying
   attribute, the **CLI `--version` output** of both binaries — a clap-based CLI wires clap's
   version explicitly to the helper, because clap's built-in default would silently report
   `CARGO_PKG_VERSION` and undo this decision — and the version part of ADR-0010's
   install-directory names (`+` and `.` are legal filename characters on ext4, APFS, and NTFS).

### How it is compared and shown

9. **One parser, in the shared crate.** A small helper in `opamp` splits a version string into its
   `MAJOR.MINOR.PATCH` base, an optional pre-release, and optional build metadata. It lives there
   rather than in either end because the Client writes these strings and the Server displays them,
   and ADR-0005 put the shared crate between them precisely so the two cannot drift.

10. **The self-update probe compares everything except the build metadata.** `0.1.1` offered and
   `0.1.1+799e36a` reported is a match; `0.1.1` offered and `0.1.1-dev+799e36a` reported is **not**.
   Dropping `+<hash>` is what fixes the trap, because the hash is the part an operator cannot type
   and does not know at upload time. Keeping the pre-release is what preserves the distinction
   the `-dev` marker exists for (point 3): a `-dev` build is a build heading for a
   release and is not that release, and this probe is the last gate that can say so before a fleet
   installs one.

11. **An offered version that has no parseable base fails the install**, naming the value. Fail
   closed: a package version is free-form by the API's own contract, and one that is not a version
   cannot be compared to a version. This is the Client's own package, where the shape is ours.

12. **`?version=` takes the release number.** `0.1.1` is the expected spelling; the full string still
   works, since its base and pre-release are the same. Nothing has to be percent-encoded.

13. **The Server shows the base and the pre-release, and keeps the rest.** `AgentView.service_version`
   is `MAJOR.MINOR.PATCH[-prerelease]` — what belongs in a column headed "Version" — and the
   complete string as reported is in a `service_build` field beside it. The bundled UI shows
   the former in its Version column and the latter on hover, and searches both.

14. **The Client keeps reporting the full string.** `service.version` on the wire is unchanged;
   provenance has to reach the Server for anyone to answer "which build is on that host". What
   changes is what the Server puts in a column, not what an Agent says.

15. **Everything written to disk keeps the full string** — the version directory's `manifest.toml`,
   the self-update marker, `--version` output. Those are the internal record this decision relies on
   existing.

### Where the helper lives

16. The resolution logic runs in the shared crate's existing build script, beside the
   protobuf codegen — the code that ran in `crates/client/build.rs`, unchanged. Its inputs, its
   grammar, its three failure modes and its `OPAMP_FLEET_VERSION` override are the same code, not a
   reimplementation.

17. `current()` joins `parse`/`identity`/`same_release` in `opamp::version`, which is where this
   project answers questions about a version string. No new module.

18. There is no `crates/client/build.rs` and no `crates/client/src/version.rs`, and every call site on
   both ends — the OpAMP `service.version` attribute, both CLIs' `--version`, the ADR-0010 install
   layout, the self-check token — reads `opamp::version::current()`.

19. `opamp` has `git2` as a **build**-dependency. It is linked into no artifact.

This preserves the Client's reported string, and the Server's string names the build it is.

## Alternatives considered

- **The tag as the only source of the version** — `build.rs` resolves the base from a `version/*`
  tag on HEAD, else from the most recent reachable one plus `-dev`, else `0.0.0-dev`, and
  `Cargo.toml` stays a stable, unread `0.1.0`. It is the safer arrangement against drift, because
  there is only one place a version appears, and it removes a whole class of "forgot to bump"
  releases. The decider has weighed it and set it aside for the reasons in the Context: the
  hand-typed tag is the failure, and a development build should say which release it is heading
  for.
- **Leave `build.rs` on the tag and let `Cargo.toml` decide only what the pipeline releases.** The
  smaller change. It was tried and dropped on the evidence: with `Cargo.toml` at `0.1.0` and no
  `version/*` tag in the repository at all, `client --version` answered `0.0.0-dev`, so the file
  said one thing and every binary built from it said another. A version source no binary reads is
  not a version source.
- **Have the pipeline write `Cargo.toml` from a tag** — the same coupling in the other direction. It
  needs the pipeline to commit to the branch during a release, which is a worse thing to automate
  than a tag: a tag is immutable and inspectable, a commit changes the history a release was cut
  from.
- **Read `Cargo.toml` but leave the tagging to a human.** Half the change, and it keeps exactly the
  step that motivated it — the hand-typed tag — while adding the file that can disagree with it.
- **Pipeline-resolved version exported as an env var, `CARGO_PKG_VERSION` fallback** (the `main`
  lineage's mechanism) — keeps `cargo build` free of git access, but leaves every non-pipeline
  build indistinguishable (`0.1.0`, no provenance) and concentrates version logic in CI where
  developers never exercise it. The env var survives as the no-git escape hatch.
- **Raw `git describe` output as the dev version** (the classic `1.2.3-4-ga1b2c3d` convention) —
  encodes lineage too, but its suffix is not SemVer build metadata (it lands in the *pre-release*
  position with unpadded numerics, giving surprising ordering) and duplicates what the `+` metadata
  already carries; the normalised `<base>-dev+<commit>` form keeps one grammar.
- **Shelling out to the `git` binary from `build.rs`** — needs no build-dependency, but makes every
  build depend on a `git` executable being installed and on `PATH` (not a given on minimal CI
  images, build containers, or Windows build boxes) and on parsing its localized CLI output; a
  library keeps the build hermetic. Rejected per maintainer direction in favour of the Rust
  library.
  It would also remove the build dependency from both ends of the shared crate; that is worth its
  own decision if the build dependency ever hurts, not a side effect of moving the helper.
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
- **A date in the build metadata** (`+<YYYYMMDD>.<hash>`, as commit date; a wall-clock build date
  would additionally break reproducibility) — the commit id already pins the exact source state,
  and git answers "when" for any commit; the date lengthens every identifier while adding no
  identity. Rejected per maintainer direction.
- **Read the version at runtime** (env var, sidecar file) — the reported version would describe the
  deployment environment, not the binary; a copied binary would change identity. Rejected.
- **Allow pre-release tags** (`version/1.2.3-rc.1`) — SemVer permits them, but strict released
  versions keep the tag grammar unambiguous; deferred until a release-candidate flow is actually
  wanted.
- **Keep the exact comparison and document the encoding** (`%2B`) — the honest reading of an exact
  rule, and it treats an unusable interface as a training problem. The operator who types the
  release number is the one behaving reasonably.
- **Compare the base only, dropping the pre-release too** — the literal form of the request that
  prompted point 10, and one step further than the problem needs. It would let an offer
  of `0.1.1` be satisfied by a `0.1.1-dev` build, which is exactly the confusion `-dev` exists to
  prevent, at the only gate that inspects a binary before a fleet runs it. If the pre-release should
  be ignored as well, that is a one-line change to the comparison and it belongs in this decision
  rather than in a later surprise.
- **Have the Client report only the base to the Server** — the smallest change to the display
  problem, and it throws away the answer to "which build is on that host", which is the question a
  fleet exists to answer.
- **Leave `service_version` holding the full string and add a base field beside it** — additive, so
  no API consumer breaks. Rejected for point 13 because it leaves a field called
  `service_version` holding something nobody would call a version, and because the REST API is at
  `v1` with no external consumer: the moment to give the field its right meaning is now, not after
  one exists. This is the reversible half of the decision, and the CHANGELOG carries it as breaking
  either way.
- **Compare with a full SemVer precedence implementation** (a `semver` dependency) — the correct
  answer if versions were ever *ordered* here. They are not: the probe asks "is this the one I was
  offered", an equality question over a string this project itself produces.
- **A second `build.rs` in `crates/server`** — the literal reading of "the Server adopts the same
  `build.rs`". Rejected: it duplicates the tag grammar and the three failure modes into a place that
  can drift, to avoid a build dependency the workspace already resolves. A rule implemented twice is
  the failure ADR-0020 measures; a version that disagrees between two binaries of one workspace is
  exactly how it shows.
- **Leave the Server on `CARGO_PKG_VERSION`** — rejected; that is the defect, and it is not
  cosmetic. A support question that starts "which build is this?" cannot be answered from a
  Server's own output, and a release build and a development build of the same number are
  indistinguishable.
- **A fourth crate holding only build support** — rejected by
  [ADR-0005](0005-workspace-and-server-runtime.md)'s standing rule that more crates are premature
  until a concrete need appears. One function shared by two build scripts is not that need when a
  shared crate already sits between the two ends.
- **`include!` a shared build-script fragment from both scripts** — rejected. It leaves two build
  scripts, needs `git2` in both manifests anyway, and hides a compiled file from the crate graph, so
  a reader of either manifest cannot see where the code comes from.

## Sources / Prior art

- **This repository's `main` lineage** — `main:docs/adr/0008-release-pipeline-and-versioning.md`
  decided the `version/*` tag grammar, strict parsing, normalisation, and compile-time bake-in for
  the supervisor host; `main:crates/supervisor/src/lib.rs` holds its `version()` helper. This
  decision keeps that contract and moves the computation from the pipeline into `build.rs`.
- Semantic Versioning 2.0.0 — the `MAJOR.MINOR.PATCH` grammar, pre-release ordering, and the rule
  that build metadata (`+…`) is ignored for precedence: <https://semver.org/>.
  Rule 10: "Build metadata MUST be ignored when determining version precedence… two versions that
  differ only in the build metadata have the same precedence." The specification the version
  strings follow says the hash is not part of the identity.
- Cargo build scripts — `cargo:rustc-env`, `rerun-if-changed`, `rerun-if-env-changed`:
  <https://doc.rust-lang.org/cargo/reference/build-scripts.html>.
  [Outputs of the build script](https://doc.rust-lang.org/cargo/reference/build-scripts.html#outputs-of-the-build-script)
  — `cargo::rustc-env` sets an environment variable "for the compilation of the crate being built",
  which is the constraint that makes the helper's home a decision about *where* the resolution runs
  rather than a free choice of module.
- `git2` — Rust bindings to libgit2, with vendored static builds via `libgit2-sys`:
  <https://docs.rs/git2/> and <https://libgit2.org/>. (`gix`/gitoxide, the considered pure-Rust
  alternative: <https://github.com/GitoxideLabs/gitoxide>.)
- `git describe --exact-match` — the CLI semantics "is HEAD itself tagged" that the tag lookup
  reproduces: <https://git-scm.com/docs/git-describe>.
- Reproducible builds — why nothing embedded in a binary should depend on the build clock
  (`SOURCE_DATE_EPOCH`): <https://reproducible-builds.org/docs/source-date-epoch/>.
- `vergen` (the considered off-the-shelf `build.rs` stamper): <https://docs.rs/vergen/>.
- **GitHub Actions documentation** on recursive triggering: "events triggered by the `GITHUB_TOKEN`
  will not create a new workflow run", so a tag the pipeline pushes cannot start the pipeline again.
  This is what lets one run tag *and* publish.
  <https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow>
- **`cargo metadata`** as the way to read the version, rather than parsing TOML in shell: it answers
  with the resolved package version, so a workspace inheriting `version.workspace = true` is read
  correctly.
  <https://doc.rust-lang.org/cargo/commands/cargo-metadata.html>
- **This project's ADR-0021**, whose release pipeline mechanism this decision keeps.
- The failure that prompted point 10, in `fleet-packages/opamp-fleet-client.json`: a
  package registered as `0.1.1` against a binary reporting `0.1.1+799e36a`, and a previous attempt
  recorded as `0.1.1 799e36a` — a `+` that a query string turned into a space.
- [RFC 3986](https://www.rfc-editor.org/rfc/rfc3986#section-2.2) and the
  `application/x-www-form-urlencoded` convention behind `+` meaning a space in a query string — why
  the full string cannot be typed into a URL unencoded.
- [ADR-0010](0010-client-os-service.md)'s version-directory naming — the same
  base/provenance split, already decided once in this codebase.
- [ADR-0020](0020-what-lives-in-which-crate.md) — the measurement the helper's home follows: the
  shared crate holds what both ends implement identically. One version rule needed by both ends is
  that case; the Client's two transports, measured the same way, are not.
- [ADR-0005](0005-workspace-and-server-runtime.md) — one workspace, one lockfile, and the standing
  rejection of further crates, which rules out the build-support-crate alternative above.
- Specification goals #10/#11 ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)) — package delivery
  and self-update, both of which identify builds by version;
  [ADR-0010](0010-client-os-service.md), whose install layout consumes this version.

## Consequences

- Positive: every binary is self-describing — base version, `-dev` pre-release marker, and commit
  hash — so a fleet report, a bug report, or an install directory always names the exact
  build;
  the same commit always reproduces the same version string;
  no `git` executable is needed at build time, and the shipped binaries carry only the version
  string — no trace of the git library.
- Positive: one number, in the file a Rust developer already looks at, changed in a reviewed commit
  rather than typed into a ref. A release is "merge the bump, run the pipeline".
- Positive: a mistyped tag cannot mint a wrong release, because no human types one.
- Positive: `-dev` reads forwards. A development build says which release it is *heading for*
  rather than which one it descends from, which is what an operator reading a fleet view assumes it
  means — and there is no ordering nuance to warn about, since `0.1.0-dev` sorting before `0.1.0`
  is simply true rather than a trap.
- Positive: a release is uploaded under the number it is called, with no encoding lore. The fleet
  table is scannable and still answers the provenance question on hover. The probe keeps the two
  checks it exists for and loses the one it never needed. `-dev` has a place where it is actually
  enforced rather than merely displayed.
- Positive: `server --version` reports `0.2.0-dev+<hash>` — the build it is, with the commit it came
  from. An operator can tell a release from a build heading for one, on both binaries, by the same
  rule.
- Positive: the tag grammar, the release/development distinction and the "a tag that disagrees with
  the file fails the build" rule exist once. A future third surface adopts them by calling a
  function.
- Negative / trade-offs: `git2` enters as a build-dependency and compiles vendored libgit2 (C) for
  the build script, lengthening cold builds and requiring a C compiler on build hosts — which
  `ring` (ADR-0007) already requires, so no new host prerequisite;
  building outside a git repository (a source tarball) fails unless `OPAMP_FLEET_VERSION` is set
  (fail closed, escape hatch documented — a deliberate choice over silently unknown identity);
  release workflows must fetch tags or they produce `-dev` builds (mitigated by the mandated
  assertion that the binary reports exactly `<version>+<hash>`);
  the version string is longer and contains `+`, which any consumer that
  parses it must tolerate (SemVer-conformant parsers do).
- Negative / trade-offs: **every build's self-report changes** with the version source. A host that
  reported `0.0.0-dev` from a tag-only lineage reports `0.1.0-dev` from the same commit lineage once
  `Cargo.toml` decides, and ADR-0010's install directory is named from it. Nothing migrates: a
  Client that updates lands in a directory named after the new version, which is what that layout
  is for.
- Negative / trade-offs: **drift is real** — `Cargo.toml` and the tags are two places a version
  appears. It is caught rather than prevented: a version that was already released fails on the
  guard, before a single target is built, instead of quietly re-releasing. A *forgotten* bump is
  therefore a failed run, which is the outcome to prefer, but it is a failure a tag-only source
  would not have.
- Negative / trade-offs: **a release cannot be re-run.** Once the tag is pushed the number is spent,
  so a run that dies in `publish` — after the tag but before the artifacts — leaves a release that
  has to be finished by hand or a version that has to be skipped. That is the price of the guard
  above, and it is paid rarely.
- Negative / trade-offs: the release run needs `contents: write` to push a tag. A pipeline that
  can write to the repository is a larger blast radius than one that only reads it, and the token is
  the workflow's own rather than a human's.
- Negative / trade-offs: `AgentView.service_version` changes meaning inside `/api/v1` — a
  breaking change for any consumer, of which there is one, the bundled UI, changed in the same
  commit. Two versions that differ only by commit are indistinguishable to the probe, so
  re-offering a rebuilt artifact of the same release is accepted rather than refused; the content
  hash is what distinguishes those bytes, and it already did.
- Negative / trade-offs: `opamp` has `git2` as a build dependency, so `cargo build -p server`
  alone compiles `libgit2`. A whole-workspace build does anyway — the Client pulls it in — so CI's
  cost is unchanged and only a Server-only build pays.
- Negative / trade-offs: every commit changes the version, and the version is baked by the crate
  both ends depend on, so a new commit rebuilds `opamp` and therefore both binaries. It rebuilt the
  Client and everything downstream of it anyway, which is both binaries in a workspace build; what
  is new is a Server-only build no longer being able to skip it.
- Nothing of `git2` reaches an artifact. A build dependency is compiled into the build script alone:
  `cargo tree -e normal -p server` names no `git2`, and neither binary carries a libgit2 symbol.
- Relation to ADR-0020: its three modules are a finding under its rule, not a ceiling, so a fourth
  thing entering the shared crate — this helper — is ordinary work under that ADR rather than a
  conflict with it. Its trade-off list carries the `git2` build dependency this adds.
- Follow-ups: whether the `Cargo.lock` bump that accompanies every version change should be checked
  in CI, so a release cannot be cut from a tree whose lockfile still names the previous version.
  The operator manual's self-update walkthrough and the release notes template tell an operator to
  type the release number, not the full version. Whether the *package* version of a Managed Process
  (free-form by contract, an upstream project's own numbering) should be parsed at all is
  deliberately left alone here.
