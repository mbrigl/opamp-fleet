# ADR-0026: The release version is the one in `Cargo.toml`, and the pipeline creates the tag from it

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Supersedes the **version-source** decision of
[ADR-0009](0009-version-derivation-and-baking.md) — where the number comes from, in the pipeline and
in `build.rs` alike. Everything else that ADR decides — the strict `MAJOR.MINOR.PATCH` grammar, the
`+<hash>` build metadata, the `-dev` pre-release marker, resolution at compile time into the binary,
and the single `version()` helper every surface reads — is untouched and still binding.

## Context

[ADR-0009](0009-version-derivation-and-baking.md) made a `version/*` tag the single source of the
version and explicitly rejected `Cargo.toml`: "it makes the version a hand-maintained file that must
be kept in sync with tags". [ADR-0025](0025-release-pipeline-and-artifacts.md) built the release
pipeline on that, and a release is currently two acts by a human — bump nothing, tag the commit,
push the tag.

The maintainer has decided the other way, and the reasons are the ordinary ones: `Cargo.toml` is
where a Rust project's version lives, it is a value a reviewer sees in the diff of the release
commit rather than in a ref nobody reads, and the number should be decided once, in the source, and
not typed a second time as a tag. What tipped it is that the second typing is the failure: a tag is
written by hand, at the keyboard, at the end of a working day, and it is the thing that names the
release forever.

The forces that made ADR-0009 choose the tag have not disappeared, and the decision below has to
answer them rather than ignore them:

- **Two sources drift.** If `Cargo.toml` says one thing and a tag says another, something must
  decide which is the release — and it must not be silence.
- **The binary's identity may not become a build-environment property.** ADR-0009's whole mechanism
  is that `build.rs` resolves the version from the repository at compile time and bakes it in.
  Reading `Cargo.toml` at build time instead would work, but the version would stop carrying the
  provenance (`+<hash>`, `-dev`) that ADR-0010's install directories and ADR-0020's self-update rely
  on, and every one of those rules would have to be re-decided.
- **The self-update compares exactly.** `selfupdate::probe` refuses a staged binary whose reported
  version is not the offered one, character for character.

## Decision

We will take the release version from **`[workspace.package] version` in `Cargo.toml`** and have the
**release pipeline create the `version/*` tag from it** — before it builds, so nothing downstream
changes.

1. **`Cargo.toml` decides the number.** Bumping the version is an ordinary commit, reviewed like
   any other.

2. **The pipeline tags.** The release run reads the version, creates `version/<version>` at the
   commit being released, and pushes it with the workflow's own token — which, by GitHub's rule
   against recursive triggering, starts no second run. Only then does it build.

3. **`build.rs` takes the base from the same file, and git says only what is *around* it.** The base
   is `CARGO_PKG_VERSION`, which Cargo hands the build script from `Cargo.toml`; a `version/<base>`
   tag on HEAD means this commit *is* that release, and its absence means the build is on the way to
   it and gets the `-dev` pre-release. The commit short-hash is appended as before. So a development
   build now reports `0.1.0-dev+a1b2c3d` — the version it is heading for — where ADR-0009 had it
   report the version it descended *from*.

   This is the part that reaches furthest, and it is the point of the change: with the base in the
   tag, `Cargo.toml` could say `0.1.0` while every binary built from it said `0.0.0-dev`, and the
   file that supposedly decides the version would be one no binary reads.

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

## Alternatives considered

- **Keep ADR-0009 as it stands** — the tag as the only source. It is the safer arrangement against
  drift, and it is what the decider has weighed and set aside; recording the trade rather than
  re-arguing it is what this ADR is for.
- **Leave `build.rs` on the tag and let `Cargo.toml` decide only what the pipeline releases.** The
  smaller change, and the first shape this decision took. It was tried and dropped on the evidence:
  with `Cargo.toml` at `0.1.0` and no `version/*` tag in the repository at all, `client --version`
  answered `0.0.0-dev`, so the file said one thing and every binary built from it said another. A
  version source no binary reads is not a version source.
- **Have the pipeline write `Cargo.toml` from a tag** — the same coupling in the other direction. It
  needs the pipeline to commit to the branch during a release, which is a worse thing to automate
  than a tag: a tag is immutable and inspectable, a commit changes the history a release was cut
  from.
- **Read `Cargo.toml` but leave the tagging to a human.** Half the change, and it keeps exactly the
  step that motivated it — the hand-typed tag — while adding the file that can disagree with it.

## Sources / Prior art

- **GitHub Actions documentation** on recursive triggering: "events triggered by the `GITHUB_TOKEN`
  will not create a new workflow run", so a tag the pipeline pushes cannot start the pipeline again.
  This is what lets one run tag *and* publish.
  <https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow>
- **`cargo metadata`** as the way to read the version, rather than parsing TOML in shell: it answers
  with the resolved package version, so a workspace inheriting `version.workspace = true` is read
  correctly.
  <https://doc.rust-lang.org/cargo/commands/cargo-metadata.html>
- **This project's ADR-0009 and ADR-0025**, whose mechanisms this decision deliberately keeps.

## Consequences

- Positive: one number, in the file a Rust developer already looks at, changed in a reviewed commit
  rather than typed into a ref. A release is "merge the bump, run the pipeline".
- Positive: a mistyped tag can no longer mint a wrong release, because no human types one.
- Positive: `-dev` now reads forwards. A development build says which release it is *heading for*
  rather than which one it descends from, which is what an operator reading a fleet view assumes it
  means — and it retires the ordering nuance ADR-0009 had to warn about, since `0.1.0-dev` sorting
  before `0.1.0` is now simply true rather than a trap.
- Negative / trade-offs: **every build's self-report changes.** A host that reported `0.0.0-dev`
  yesterday reports `0.1.0-dev` today from the same commit lineage, and ADR-0010's install directory
  is named from it. Nothing migrates: a Client that updates lands in a directory named after the new
  version, which is what that layout is for.
- Negative / trade-offs: **the drift ADR-0009 designed away is now real** — `Cargo.toml` and the
  tags are two places a version appears. It is caught rather than prevented: a version that was
  already released fails on the guard, before a single target is built, instead of quietly
  re-releasing. A *forgotten* bump is therefore a failed run, which is the outcome to prefer, but it
  is a failure where ADR-0009 had nothing to fail.
- Negative / trade-offs: **a release cannot be re-run.** Once the tag is pushed the number is spent,
  so a run that dies in `publish` — after the tag but before the artifacts — leaves a release that
  has to be finished by hand or a version that has to be skipped. That is the price of the guard
  above, and it is paid rarely.
- Negative / trade-offs: the release run now needs `contents: write` to push a tag. A pipeline that
  can write to the repository is a larger blast radius than one that only reads it, and the token is
  the workflow's own rather than a human's.
- Negative / trade-offs: ADR-0009 keeps its status while one of its decisions no longer holds, which
  is a thing a reader has to notice. That is why this ADR names the superseded part in its first
  line.
- Follow-ups: whether the `Cargo.lock` bump that accompanies every version change should be checked
  in CI, so a release cannot be cut from a tree whose lockfile still names the previous version. And
  whether the Server, published by no pipeline today, should follow the same rule when it is.
