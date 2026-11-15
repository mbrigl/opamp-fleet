# Ticket: A commit the plan and the backlog cite is one the branch reaches

## What to build

Two mistakes of the merges since 2026-11-12 that a check could have caught:

- **Orphaned commit hashes.** A rework that regrouped `feat/bugfixes` left two commit hashes in
  tickets 03 and 07 of `revision-2026-11-12` and in a backlog entry that no longer named a commit
  of the branch (#173, must-fix findings of its review). A check reads the hashes written in
  `docs/plan/` and `docs/backlog/` and fails on one that HEAD does not reach. That a hash resolves
  is not enough, since a rewritten commit stays in the object store; whether HEAD reaches it
  decides.
- **A commit message implementing a superseded ADR.** Three commit messages named ADR-0025 instead
  of ADR-0045, which superseded it (#173). A sensor lists the commits of a branch whose
  `Implements ADR-NNNN` names an ADR neither accepted nor proposed.

## Acceptance criteria

- [ ] Given a backlog entry citing a hash HEAD does not reach, when `check-docs.sh` (or a check of
  its own) runs, then it fails and names the file and the hash.
- [ ] Given a branch with a commit message `Implements ADR-NNNN` of a superseded ADR, when the
  sensor runs, then it lists the commit.
- [ ] Given each, a self-test builds its own repository fixture.

## Read first

- Touches: `scripts/check-docs.sh`, `scripts/test-check-docs.sh`, `scripts/` (a new sensor and its
  self-test), `scripts/check-all.sh`, `.github/workflows/checks.yml`
- [`AGENTS.md` §4](../../../AGENTS.md#4-working-style) (commits name the ADR they
  implement), ADR-0006 (the plan and the backlog).
- CONVENTIONS: "A self-test asserts what it builds".

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
