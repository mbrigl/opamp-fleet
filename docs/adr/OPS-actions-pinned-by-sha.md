# OPS-actions-pinned-by-sha: A third-party GitHub Action is pinned to a commit SHA with its version in a comment, a first-party one to its major tag, and Dependabot keeps both current

- **Status:** 🟢 accepted
- **Date:** 2026-10-19
- **Deciders:** Markus Brigl
- **Applies to:** every `uses:` reference in `.github/workflows/`, `.github/dependabot.yml`, `scripts/check-action-refs.sh` and `scripts/test-check-action-refs.sh`, the `actions` job of `.github/workflows/checks.yml`, and the section *Actions in the workflows* of `SECURITY.md`

## Context

Every `uses:` line in [`.github/workflows/`](../../.github/workflows/) runs code from another
repository with this repository's token. How the reference is written decides how current that
code stays, and how far a compromise of its repository reaches.

Two third-party actions run in the workflows:

- `Swatinem/rust-cache` runs in `checks.yml` (jobs `rust`, `client-windows`, `client-macos`,
  `s3-store` and `postgres-store`), `ci.yml` (job `release-build`), `interop.yml`, `g20.yml` and
  `service-smoke.yml`. It restores and saves `target/` and `~/.cargo`. So what it writes is
  compiled into the next run's test binaries.
- `taiki-e/install-action` runs in `release.yml` (job `advisories`), `audit.yml` and `fuzz.yml`.
  It downloads and runs prebuilt tools, among them the `cargo-deny` that gates every release. The
  tool versions it resolves and the checksums it verifies come from manifests inside the action's
  own repository, at whatever commit the reference points to.

Either repository can move a version tag to a commit of its choosing, and the next run executes
that commit. In March 2025 the version tags of `tj-actions/changed-files` were moved to a commit
that printed the runner's secrets into the workflow logs (CVE-2025-30066). A full commit SHA is the
only reference GitHub treats as immutable.

A bare SHA tells a reader nothing. A trailing `# vX.Y.Z` comment names the release, and Dependabot
reads that comment. When it raises a SHA-pinned action, it rewrites the SHA and the comment in the
same line (recalled, not read again for this record). The `github-actions` entry of
[`.github/dependabot.yml`](../../.github/dependabot.yml) runs weekly and groups every action update
into one pull request.

A SHA has one weakness of its own. GitHub resolves a SHA under a repository's name for a commit
that exists only in a fork of that repository, because a fork shares the object store of its
parent (recalled, not read again). So a pull request can name a SHA that no release of the action
contains, while its comment names a real release. Reading the diff does not catch this. Resolving
the commented tag does.

GitHub's own actions (`actions/checkout`, `actions/upload-artifact`, `actions/download-artifact`,
`actions/setup-go`, `actions/attest`) are maintained by the platform that runs the workflow. A
compromise of their repositories is a compromise of GitHub Actions itself, and a SHA would not
contain it. On their major tag they receive fixes without a pull request.

The specification's quality goal Q-3 asks that the supply chain be checked before a change merges.
Its threat model counts CI actions in that supply chain. An action is part of it even though
`cargo deny` does not see it.

## Decision

We will pin every third-party action to a full commit SHA followed by a `# vX.Y.Z` comment naming
its release. We keep every first-party action on its major version tag, and let Dependabot raise
both.

1. **First-party means under the GitHub account `actions`.**
   - An action or reusable workflow whose repository lives under the account `actions` is
     first-party.
   - Every other account is third-party, including `github` (for example
     `github/codeql-action`).
   - The same rule classifies a reusable workflow and an action.

2. **A first-party action is referenced by its major version tag.**
   - The reference is `actions/<name>@vN` or `actions/<name>/<path>@vN`.
   - A workflow must not reference a first-party action by a commit SHA, a branch, a full version
     tag, or no tag.

3. **A third-party action is referenced by a full commit SHA with its release in a comment.**
   - The reference is `<account>/<action>@<sha>`, or `<account>/<action>/<path>@<sha>` for an
     action or reusable workflow inside a repository.
   - `<sha>` is the full 40-character lowercase hexadecimal commit ID.
   - The same line ends in a comment `# vX.Y.Z`. It names the release tag, as three numbers, that
     the SHA was taken from.
   - A workflow must not use any of these for a third-party action:
     - a major or full version tag;
     - a branch;
     - a short SHA;
     - a SHA without the comment;
     - a comment that names only a major (`# v2`).

4. **The commented tag resolves to the pinned SHA.**
   - In the action's own repository, the tag the comment names must point to exactly the pinned
     commit.
   - For an annotated tag, that is the commit the tag object points to, not the tag object's own
     ID.
   - The check refuses a SHA that is not the commit of its commented tag, whether it comes from a
     fork or from a moved tag.

5. **Dependabot raises every update.**
   - `.github/dependabot.yml` has an entry with `package-ecosystem: github-actions`.
   - For a first-party action, Dependabot raises the pull request for a new major.
   - For a third-party action, Dependabot raises one for every new release. It rewrites the SHA and
     the comment together.
   - Each such pull request is reviewed and merged like any other change.

6. **A `docker://` image carries an explicit tag, and a local action is exempt.**
   - A `docker://` reference names `:<tag>` on its last path segment.
   - A local action (`./…`) is part of this repository and needs no reference.

**Out of scope:**

- which actions the workflows use, and whether a further third-party action may be added;
- pinning a `docker://` image by digest, which no workflow uses;
- a lockfile for the project's toolchain;
- Dependabot for Dev Container Features, which
  [NUC-container-hides-host-daemon](NUC-container-hides-host-daemon.md) leaves as a follow-up.

## Alternatives considered

- **Every action on its major version tag, kept current by Dependabot.** This is the template's
  rule. It accepts the mutability of the tag on the premise that the workflows run only
  first-party actions. It names SHA pinning with a stricter check as the answer for a project that
  runs actions it trusts less. The workflows here run two third-party actions, so the premise does
  not hold. The major tag would leave the release gate's `cargo-deny` and every CI cache to
  whoever controls two third-party repositories. A repointed tag would run in CI with the
  repository's token before anyone sees a diff. Rejected.
- **Pin every action to a SHA, `actions/*` included.** One rule with no exception. But it turns
  each patch release of `checkout`, `upload-artifact` and `download-artifact` into a pull request.
  And it does not contain the threat it addresses: whoever can move a tag in `actions/*` can also
  change the runner those actions run on. Rejected.
- **Vendor the actions into `.github/actions/`.** A local copy is immutable and needs no network
  lookup. But it gets no Dependabot, so every upstream fix is a manual port.
  `taiki-e/install-action` is mostly its tool manifests, which change several times a week.
  Rejected.
- **Pin to a full version tag (`@v2.9.3`).** It names the release readably. But a tag can be moved
  like a major tag, so this gives no immutability, and it is as frozen as a SHA. Rejected.
- **Replace the third-party actions with `run:` steps.** `cargo install --locked <tool>@<version>`
  instead of `install-action`, and `actions/cache` with explicit keys instead of `rust-cache`. This
  removes the dependency rather than pinning it. It costs build minutes in every CI job, and a
  cache key of our own is code to maintain. The release workflow already installs its packaging
  tools this way. Not chosen for CI. A later decision may still take it for a single action.

## Sources / Prior art

- GitHub, *Security hardening for GitHub Actions*, section *Using third-party actions*:
  <https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions>.
  "Pinning an action to a full length commit SHA is currently the only way to use an action as an
  immutable release", because a tag "can be moved or deleted if a bad actor gains access to the
  repository storing the action". Recalled, not read again for this record.
- GitHub Advisory GHSA-mrrh-fwg8-r2c3 / CVE-2025-30066:
  <https://github.com/advisories/GHSA-mrrh-fwg8-r2c3>, the moved tags of
  `tj-actions/changed-files`.
- GitHub, *Keeping your actions up to date with Dependabot*:
  <https://docs.github.com/en/code-security/dependabot/working-with-dependabot/keeping-your-actions-up-to-date-with-dependabot>.
  The `github-actions` ecosystem and the schedule keys. Dependabot updates actions referenced by
  SHA, and updates a version comment on the same line that names the current release. Recalled,
  not read again for this record.
- The commits of a fork are reachable through the parent repository's name, reported for Actions
  as "impostor commits" (Chainguard, 2023). Recalled, not read again for this record.
- OpenSSF Scorecard, check *Pinned-Dependencies*: it flags an action that is not pinned by SHA.
  Recalled, not read again for this record.
- The kinds of tag the two third-party repositories use, read with `git ls-remote`:
  `Swatinem/rust-cache` tags are annotated, so the check peels them to their commit.
  `taiki-e/install-action` tags are lightweight and point to the commit directly.

## Consequences

- Positive:
  - A moved tag in either third-party repository does not reach a run.
  - The `cargo-deny` version and checksum that gate a release change only by a reviewed commit.
  - A reference names the release it runs.
  - A SHA from a fork fails the check before it merges.
- Negative / trade-offs:
  - Every third-party release is a pull request. `taiki-e/install-action` releases several a week.
    The weekly grouped Dependabot run turns that into at most one pull request a week for all
    actions.
  - A fix in a third-party action arrives only when that pull request is merged.
  - The resolution check needs the network. An unreachable GitHub fails the `actions` job. A tag
    that upstream deletes or moves fails every pull request until the reference is raised. That
    failure is the signal clause 4 exists for.
  - The comment grammar `vX.Y.Z` excludes an action whose tags carry no leading `v`. Taking one on
    means changing this decision.
- Negative / trade-offs: `scripts/check-action-refs.sh` and its self-test differ from the
  template's. A template release that changes them is merged by hand. The template's major-tag
  rule does not apply here, whatever the template does with it.
- Follow-ups: whether Dependabot *alerts*, as opposed to version updates, match a SHA-pinned action
  against an advisory is not verified. Also open: a networked check that a first-party major tag
  still exists.

## Enforcement

[`scripts/check-action-refs.sh`](../../scripts/check-action-refs.sh) runs in the job `actions` of
[`checks.yml`](../../.github/workflows/checks.yml). It reads every uncommented `uses:` line of the
workflows and fails on any of these:

- an `actions/*` reference that is not a major version tag (clause 2);
- a third-party reference that is not a 40-character lowercase SHA followed by `# vX.Y.Z`
  (clause 3);
- a `docker://` image without a tag (clause 6);
- a missing `dependabot.yml`, or one without the `github-actions` ecosystem (clause 5).

With `--resolve`, which the `actions` job passes, it also runs `git ls-remote` on each third-party
repository. It fails when the commented tag does not exist or does not peel to the pinned SHA
(clause 4).

[`scripts/test-check-action-refs.sh`](../../scripts/test-check-action-refs.sh) carries a
`Verifies:` marker for this ADR. It covers these cases:

- A third-party SHA with its comment passes.
- Each of these fails with a message naming clause 3:
  - a third-party major tag;
  - a SHA without a comment;
  - a `# v2` comment;
  - a short SHA;
  - an uppercase SHA.
- `actions/checkout` on a SHA fails, and `github/codeql-action/init@v3` fails as third-party
  (clauses 1, 2).
- A reusable workflow under `.github/workflows/w.yml`, pinned by SHA with `# v1.2.3`, passes.
- Under `--resolve`, with a stub `git` on `PATH`:
  - a lightweight tag that matches passes;
  - an annotated tag whose peeled commit matches passes;
  - a tag that peels to another SHA, a SHA equal to the tag object's own ID, and a missing tag each
    fail.

**Not mechanically decidable:** these are a judgement or a repository setting, not a file
a test reads.

- whether a new third-party action is trustworthy enough to add. Review holds it;
- whether Dependabot is enabled for the repository, which is a setting. The repository settings in
  `CONTRIBUTING.md` do not list it.
