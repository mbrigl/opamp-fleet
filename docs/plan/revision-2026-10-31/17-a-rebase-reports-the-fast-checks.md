# Ticket: A rebase reports the documentation and structure checks when it finishes

## What to build

A conflict resolved during a rebase left a 158-column prose line in `docs/ARCHITECTURE.md`.
`pre-commit` runs the checks only on the conclusion of a conflicted merge and `pre-merge-commit`
only on a merge, and the commits a rebase replays pass neither, `--rebase-merges` included; so
check-docs caught the line at the pre-push gate, after four branches had been pushed through a long
check-all each. Step 4 of the README's Integration branches, the rework by rebase, has no gate
before the push.

Add a `post-rewrite` hook that, after a rebase, runs `scripts/check-docs.sh` and the structure check
of docs/plan/revision-2026-10-31/16-structure-tests-run-in-a-second.md and reports their result. Git
ignores the exit code of `post-rewrite`, so the hook reports and refuses nothing; the README's hook
table and step 4 say so.

## Acceptance criteria

- [ ] Given a rebase whose resolved conflict breaks check-docs, when the rebase finishes, then the
  hook prints the failing check and the command that shows it.
- [ ] Given a clean rebase, when it finishes, then the hook prints one line and takes no more than a
  few seconds.
- [ ] Given `scripts/test-git-hooks.sh`, when it runs, then it exercises the new hook, and the
  README's Git hooks table has its row.

## Read first

- README, Git hooks and Integration branches.
- `.githooks/pre-merge-commit`, `.githooks/pre-commit`.
- `scripts/test-git-hooks.sh`.

## Blocked by

- docs/plan/revision-2026-10-31/16-structure-tests-run-in-a-second.md

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
