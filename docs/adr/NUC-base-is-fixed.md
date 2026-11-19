# NUC-base-is-fixed: The template's `NUC-` ADRs are a fixed base that a project extends or narrows but never edits

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** the `NUC-` ADRs, the `Extends:`, `Narrows:`, and `Role:` fields of project ADRs, the blob list `docs/adr/NUC.blobs`, `AGENTS.md` in a derived project, checks 8, 12, and 13 of `scripts/check-docs.sh`, and `adr_blobs` of `scripts/lib/adr.sh`

## Context

A derived project accepted an inherited ADR by adding its decider to it, and some projects added
clauses as well. After that edit the project owned the text. A take-up of a release could not
replace the files; it merged them through a normalization. A check on the template's text failed
on the project's edits, which is why the check on avoided terms stayed a sensor.

The template also refers to choices each project makes, such as its harness and its toolchain.
A project names those ADRs freely, so the template cannot cite them by identifier.

Several forces bound the answer. The checks are bash and must keep working on macOS, Linux, and
Windows under Git Bash. Each choice the template leaves open must stay visible as a choice.

## Decision

We will ship the template's `NUC-` ADRs as a fixed base that a project extends or narrows but
never edits.

1. **Fixed base.** The `NUC-` ADRs lie in `docs/adr/` next to the project's ADRs.
   - A derived project must not edit a `NUC-` file, and no project ADR may supersede one.
   - A project ADR may extend or narrow a `NUC-` clause, named in its `Extends:` or `Narrows:`
     field.
   - A derived project must not edit `AGENTS.md` either. It narrows a rule there by an ADR of its
     own.
   - A project that must contradict a `NUC-` clause forks the template. The fork removes the blob
     list and names the release it forked in the **Template** section of its README. It writes
     `, forked` after the version on the release line.
   - A project accepts the base by naming the release it carries. It takes up a release by the
     steps of [NUC-releases-are-semver-tags §6](NUC-releases-are-semver-tags.md).
2. **Blob list.** The blob list `docs/adr/NUC.blobs` names each `NUC-` file and `AGENTS.md` with
   its git blob identifier. A change to one of these files rewrites the list in the same commit.
   In every repository, each of them must match its line in the list.
3. **Roles.** The required roles are `harness` and `toolchain`. A project ADR that fills one
   carries the header field `Role:` naming it. Once the template setup is done, each role must
   have exactly one accepted ADR. The template refers to a role, never to a project's identifier.

**Out of scope:**

- the correction of a `NUC-` ADR in the template repository
  ([NUC-decisions-are-proposed-then-accepted §6](NUC-decisions-are-proposed-then-accepted.md));
- what a project picks for each role (its role ADR).

## Latitude

A project may choose:

- extensions and narrowings of any `NUC-` clause, each in an ADR of its own;
- the concepts of each required role, from the entries `harness` and `toolchain` of the
  [catalogue](../../.agents/skills/propose-adr/concepts/README.md);
- a fork of the template, when it must contradict a `NUC-` clause.

## Alternatives considered

- **Inherited ADRs that the project edits, as before.** A project sees the template's text as its
  own and may adapt it freely. Every take-up merges by hand, and no check can tell the template's
  text from the project's.
- **The `NUC-` ADRs in a directory of their own.** The base would be visibly apart. The readers
  would need two paths, and the prefix already marks the base.
- **A SHA-256 list instead of git blob identifiers.** SHA-256 is the common checksum. It needs a
  tool that differs between macOS, Linux, and Windows, and it changes with the line endings. Git
  computes the blob identifier on every platform, from the content git stores.

## Sources / Prior art

- The Git object model (Pro Git, chapter "Git Internals – Git Objects"): a blob's identifier is
  the hash of its stored content, the same on every platform.

## Consequences

- Positive: a take-up of a release replaces files and runs the checks.
- Positive: a check can spare the template's files, because no project edits them.
- Negative: a project can no longer adapt the template's text; it narrows by an ADR or forks.

## Enforcement

`scripts/check-docs.sh` holds these clauses, and its self-test cites this ADR:

- no supersession of a `NUC-` ADR, existing `Extends:` and `Narrows:` clauses, and the blob list
  (clauses 1 and 2);
- one accepted ADR per required role after the setup (clause 3).

**Not mechanically decidable:** the review judges whether a narrowing contradicts its clause
(clause 1).
