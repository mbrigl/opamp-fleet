# ADR-0026: The release version is the one in `Cargo.toml`, and the pipeline creates the tag from it

- **Status:** 🟡 proposed
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Supersedes the **version-source** decision of
[ADR-0009](0009-version-derivation-and-baking.md). Everything else that ADR decides — the grammar,
the `+<hash>` build metadata, the `-dev` pre-release, the compile-time bake-in, and the single
`version()` helper every surface reads — is untouched and still binding.

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

3. **`build.rs` is not touched.** Because the tag exists on HEAD before the first `cargo build`, the
   ADR-0009 resolution finds it exactly as it does for a hand-made tag and bakes
   `<version>+<hash>`, with no `-dev`. The binary's identity, the install-directory names, and the
   self-update's probe keep working unchanged and for unchanged reasons. This is what makes the
   change small: *who decides the number* moves, *how a binary learns it* does not.

4. **Drift is refused, never resolved.** Three guards, each failing the release rather than guessing:

   | Situation | Answer |
   |---|---|
   | `version/<v>` already exists on the commit being released | reuse it; re-running a release is not an error |
   | `version/<v>` exists on a **different** commit | **fail** — a release tag is never moved |
   | the run was started by a hand-pushed `version/*` tag | the tag and `Cargo.toml` must agree, or **fail** |

   And after building, the binary must report exactly `<version>+<hash>` — the check that the tag
   the pipeline made is the one the compiler saw.

5. **A dry run neither tags nor publishes.** `workflow_dispatch` takes a `dry-run` input, true by
   default: it builds and packs all targets and stops. A `-dev` version is expected there, and only
   there.

## Alternatives considered

- **Keep ADR-0009 as it stands** — the tag as the only source. It is the safer arrangement against
  drift, and it is what the decider has weighed and set aside; recording the trade rather than
  re-arguing it is what this ADR is for.
- **Read `Cargo.toml` in `build.rs`** instead of git, which is what "the version comes from
  `Cargo.toml`" most literally means. It would drop the `+<hash>` provenance and the `-dev` marker,
  or force `build.rs` to re-derive them from git anyway — and ADR-0010's install directories,
  ADR-0009's ordering rules, and the self-update's probe all read that string. A much larger change
  for a smaller reason: what was asked for is where the *number* is decided, not what a binary
  reports about itself.
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
- Negative / trade-offs: **the drift ADR-0009 designed away is now real** — `Cargo.toml` and the
  tags are two places a version appears. It is caught rather than prevented: a version that was
  already released fails on the tag guard instead of quietly re-releasing. A *forgotten* bump is
  therefore a failed run, which is the outcome to prefer, but it is a failure where ADR-0009 had
  nothing to fail.
- Negative / trade-offs: the release run now needs `contents: write` to push a tag. A pipeline that
  can write to the repository is a larger blast radius than one that only reads it, and the token is
  the workflow's own rather than a human's.
- Negative / trade-offs: ADR-0009 keeps its status while one of its decisions no longer holds, which
  is a thing a reader has to notice. That is why this ADR names the superseded part in its first
  line.
- Follow-ups: whether the `Cargo.lock` bump that accompanies every version change should be checked
  in CI, so a release cannot be cut from a tree whose lockfile still names the previous version. And
  whether the Server, published by no pipeline today, should follow the same rule when it is.
