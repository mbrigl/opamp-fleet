# Ticket: Check 18 counts no clause inside a fence

## What to build

Check 18 of `scripts/check-docs.sh` counts the clauses of an ADR's Decision with an awk of its own
(near lines 1422 to 1427). That awk ignores fenced code: an `N. ` line inside a fence counts as a
clause, and a `## ` line there ends the section. `scripts/lib/adr.sh` provides `ADR_FENCE_AWK` so
that every reader of an ADR's sections ends a section at the same line.

`ADR_FENCE_AWK` is prepended to check 18's count, and one self-test case covers a fenced line.

The defect is latent: no ADR on `main` has a numbered line inside a fence in its Decision. The
normalization leans on check 18, so it must be fixed before then.

**Effort.** S.

## Acceptance criteria

- [ ] Given an ADR whose Decision holds a fenced block with a line `1. x`, when check 18 counts its
  clauses, then the fenced line is not counted (a new self-test case).
- [ ] Given `scripts/test-check-docs.sh`, when the change lands, then it passes, and `check-docs.sh`
  passes on the tree.

## Read first

- Touches: `scripts/check-docs.sh`, `scripts/test-check-docs.sh`, `scripts/lib/adr.sh`
- CONVENTIONS: "A self-test asserts what it builds".
- Pull request #180 edits the header of check 18; work this after it lands.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
