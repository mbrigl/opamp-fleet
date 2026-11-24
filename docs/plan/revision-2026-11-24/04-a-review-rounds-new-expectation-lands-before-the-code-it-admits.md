# Ticket: A review round's new expectation lands before the code it admits

## What to build

`scripts/sensor-red-commits.sh`, run on the human's instruction for this revision, found two
commits of #221 (chore/revision-after-50) red alone. It reads the ten newest merges of a pull
request only (#218 to #227), so #216 and #217, in the revision's scope, went unchecked:

- 5a15144e4, the revert of "the issuing and renewal of a certificate on a report's connection
  move to session";
- 6c6ccc987, "what a report is answered with moves from session to the module root".

Rerun one by one with `scripts/check-all.sh`, both fail the same test,
`each_fleet_context_names_only_the_contexts_ranked_below_it` in
`crates/fleet-core/tests/dependency_direction.rs`:
`fleet::session:<line>: calls issue of fleet::approval`. The code of both is what the review
round decided. The test's ranks were not yet revised: 932481ae1, "approval ranks below session",
landed two commits later. The review round changed SRV-fleet-contexts-call-by-rank, the code,
and the test that decides it, and it committed the code first. Each commit between was red. The
pre-push `scripts/check-all.sh` sees only the tip, so nothing caught it. #221 took 25 commits
and three rounds of review (`scripts/sensor-work-measures.sh`).

A third commit, 4bed7df7d (a docs-only change to SRV-fleet-contexts-call-by-rank), was reported
red alone too, but its `scripts/check-all.sh` passes on a rerun (613 s). It is not a finding of
the project. The sensor keeps no output, so its cause is unknown. That is a draft about the
template, kept apart.

A check on each commit would go into `.githooks/pre-push`, which the template ships and
NUC-main-only-via-pr §6 binds; it is proposed in a draft about the template, not here. Choose
one:

1. An entry in `docs/CONVENTIONS.md`: a commit that changes what a structural test expects lands
   before, or in, the commit whose code needs it. A review round that reverses a move rewrites
   its fixups into that order before the hand-off.
2. Neither: two reds in one branch of 25 commits are accepted as the cost of a review round.

## Acceptance criteria

- [ ] Given the human's choice, when the change lands, then `docs/CONVENTIONS.md` holds the
  entry (1), or this ticket is deleted with the reason in the commit (2).
- [ ] Given the change, when `scripts/check-all.sh` runs, then it passes.

## Read first

- Touches: `docs/CONVENTIONS.md` (1)
- `crates/fleet-core/tests/dependency_direction.rs`, `FLEET_RANKS`;
  `docs/adr/SRV-fleet-contexts-call-by-rank.md`; commits 5a15144e4, 6c6ccc987, 932481ae1.

## Blocked by

- None (can start immediately)

## Mode

with the human: the human chooses between a convention and neither, and
DEV-maintainer-decides-agents-implement §5 keeps every part of the system with the human.
