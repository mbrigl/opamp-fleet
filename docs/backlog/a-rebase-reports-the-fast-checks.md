# A rebase reports the fast checks

**Kind:** idea, check.

**Observed.** A conflict resolved during a rebase left a 158-column prose line in
`docs/ARCHITECTURE.md`. `pre-commit` runs the checks only on the conclusion of a conflicted merge,
and `pre-merge-commit` only on a merge; the commits a rebase replays pass neither. check-docs caught
the line only at the pre-push gate. `main` has no `.githooks/post-rewrite`; the hooks and the
integration branches are described in `CONTRIBUTING.md` (the **Git hooks** and **Integration
branches** sections, near lines 123 and 157).

**What the old branch had.** Revision ticket 17 of `feat/revision-2026-10-31`, open, not worked: a
`post-rewrite` hook that, after a rebase, runs `scripts/check-docs.sh` and the structure check of
`structure-tests-run-in-a-second.md` and reports their result. Git ignores the exit code of
`post-rewrite`, so the hook refuses nothing, and the hook table and the rework step of
`CONTRIBUTING.md` say so. It waits for the structure check.
