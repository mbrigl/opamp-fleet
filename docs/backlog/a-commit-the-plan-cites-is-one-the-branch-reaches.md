# A commit the plan and the backlog cite is one the branch reaches

**Kind:** idea, check.

**Observed.** Two mistakes of the merges since 2026-11-12 that a check could have caught:

- **Orphaned commit hashes.** A rework that regrouped `feat/bugfixes` left commit hashes in
  tickets 03 and 07 of `revision-2026-11-12` and in a backlog entry that named no commit of the
  branch any more. Only the review of #173 caught it, once. That a hash resolves is not enough,
  since a rewritten commit stays in the object store; whether HEAD reaches it decides.
- **A commit message implementing a superseded ADR.** Three commit messages named ADR-0025 instead
  of ADR-0045, which superseded it (#173).

**Idea.** Two checks, each with a self-test that builds its own repository fixture:

- a check reads the hashes written in `docs/plan/` and `docs/backlog/` and fails on one that HEAD
  does not reach, naming the file and the hash;
- a sensor lists the commits of a branch whose `Implements ADR-NNNN` names an ADR that is neither
  accepted nor proposed.

The work is M, not S: two checks, their repository fixtures, and a CI checkout of the full history.

**From.** The design revision of 2026-11-15, ticket 14, moved to the backlog by the human
(2026-11-15).
