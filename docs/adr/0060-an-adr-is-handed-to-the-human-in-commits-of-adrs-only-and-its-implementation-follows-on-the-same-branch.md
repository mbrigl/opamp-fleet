# ADR-0060: An ADR is handed to the human in commits of ADRs only, and its implementation follows on the same branch

- **Status:** 🟡 proposed
- **Date:** 2026-10-26
- **Deciders:** Markus Brigl
- **Applies to:** rules 3 and 4 of the ADR rules in `AGENTS.md`, the submit step of the `propose-adr` skill under `.agents/skills/`, the ADR line and the `ADRs reviewed at:` line of `.github/PULL_REQUEST_TEMPLATE.md`, the check for commits mixing the text of an ADR with other paths in `scripts/sensor-review-binding.sh` and `scripts/test-sensor-review-binding.sh`, the sensors job of `.github/workflows/checks.yml` and `.github/workflows/review-binding.yml` that run it, and every branch and pull request that carries the text of an ADR

## Context

The ADR rules of [`AGENTS.md` §3](../../AGENTS.md#3-adr-rules) are the one block of the rule file
with no ADR behind it. [ADR-0001](0001-agent-governance-model.md) accepts them as they stand and
makes them reversible by an ADR on the ADR process. This is that ADR, for rules 3 and 4.

Rules 3 and 4 put an ADR in a pull request of its own, never mixed with implementation, and each
revision in a pull request of ADRs only. A decision and its implementation are then at least two
pull requests, and every revision a further one. The implementation cannot start from `main`,
where the ADR has not landed, so it is stacked on the ADR's branch, and a revision on that branch
reaches the implementation only by a merge or a rebase. For a hardening change that is three
proposed ADRs with their implementations, this is six pull requests, six CI runs per push and
merges between branches that exist only to carry the separation.

What the separation protects is that the human reviews the decision before anything is built
against it, and sees the decision's text apart from the code. A pull request is one way to keep
the two apart; a commit is another, and the text of an ADR is a path git can tell from every
other.

## Decision

We will hand an ADR to the human in commits of ADRs only, before anything is implemented against
it, and implement it after those commits on the same branch or on a branch stacked on the ADR's,
with each revision a commit of ADRs only.

1. An ADR is submitted with status `proposed` in commits of ADRs only, and those commits are
   handed to the human before anything is implemented against them.
2. The commits go on the branch of the work they belong to, or on a branch of their own for an
   ADR that waits for its decision while other work goes on.
3. ADRs the same human decides may travel together in those commits; each stays one decision.
4. An ADR the human sends back leaves the work's branch for a branch of its own, so the work's
   pull request does not carry it, and nothing is implemented against it until it is settled.
5. The implementation follows after the ADR's commits on the same branch, or on a branch stacked
   on the ADR's.
6. A commit that changes the text of an ADR changes no path outside `docs/adr/`, and a sensor
   lists one that does. An ADR's `Status`, `Date` and `Deciders` lines are not its text, so its
   status flip may travel with code.
7. Review findings return as revisions, each a commit of ADRs only.
8. The pull request names, on its `ADRs reviewed at:` line, the commit of ADRs the human
   reviewed before the implementation.
9. Only a human changes the status. The flip to `accepted`, a header line and no text, may land
   with the implementation.

**Out of scope:** when an ADR is needed (rule 2), that accepted ADRs are immutable (rule 5), and
the fresh-context review of a change and the commit it is bound to
([`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done)). The merge
method of `main` and the git hooks are not changed by this decision.

## Alternatives considered

- **Keep a pull request per ADR.** It keeps the separation, at the price of the pull requests,
  CI runs and branch merges above, and gives the human nothing a commit of ADRs only does not.
- **A branch per ADR, merged into the work's branch.** It keeps the ADR's own history, but brings
  back the merges between branches the pull request rule needed, now inside one pull request.
- **The decision in the pull request description only.** It would leave the record outside the
  tree, where no check reads it and no later session finds it.

## Sources / Prior art

- NUC, the template this repository derives from: release
  [v0.3.0](https://github.com/hivevm/nuc/releases/tag/v0.3.0) moves ADRs into commits of ADRs
  only on a branch of their own and binds the review to the commit it saw; release
  [v0.4.0](https://github.com/hivevm/nuc/releases/tag/v0.4.0) lets them go on the branch of the
  work, with the implementation after them or stacked, and names a returned ADR's way off the
  branch. Both read from the release notes on GitHub.
- NUC's own [ADR-0001](https://github.com/hivevm/nuc/blob/v0.4.0/docs/adr/0001-agent-governance-model.md)
  at v0.4.0, which has a derived project change the ADR rules by an ADR on the ADR process.
- `scripts/sensor-review-binding.sh` and `scripts/lib/selftest.sh` as the template has them at
  v0.4.0, and its self-test with the marker that cites this ADR.

## Consequences

- Positive: one pull request per decision and its implementation; a revision is a commit on the
  branch the implementation already sits on, so no merge between branches carries it.
- Positive: the human reads the decision in its own commits, and the sensor shows when a commit
  mixes it with code.
- Negative: the separation now rests on commits, which a rebase can fold together; the sensor
  reports such a commit and fails nothing, so the reviewer and the human hold it.
- Negative: a pull request that carries both is larger to review than either half; its
  `ADRs reviewed at:` line names the commit of ADRs so the decision can be read on its own.

## Enforcement

- The self-test of [`scripts/sensor-review-binding.sh`](../../scripts/sensor-review-binding.sh),
  `scripts/test-sensor-review-binding.sh`, cites this ADR: a commit changing the text of an ADR
  together with another path is listed, inside a merged branch too; one changing only paths under
  `docs/adr/`, or only an ADR's `Status`, `Date` and `Deciders` lines beside code, is not. The
  sensor runs in `scripts/check-all.sh` and the sensors job of `.github/workflows/checks.yml`.
- **Not mechanically decidable:** that the human reviewed the ADR's commits before the
  implementation was written. The pull request's `ADRs reviewed at:` line names that commit,
  and the human holds it.
