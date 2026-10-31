# Ticket: A reference to a README section by its name is checked

## What to build

A README rewrite on `feat/dev` (6da09455) removed text that other files still name:
`.devcontainer/devcontainer.json` points at README → "Host container management", which that branch
removed. On `main` already, `crates/fleet-agent/tests/service_smoke.rs` cites "the README's
checklist" twice and "the manual checklist in README.md" once, and the README has no checklist.
Check 3 of `check-docs.sh` resolves links and anchors, never a section named in prose, and the
convention to search for an old wording did not prevent it.

Extend `check-docs.sh` so every `README → "<Heading>"` and `README.md → "<Heading>"` in a tracked
file names a heading of the README it means: the `README.md` next to the file when there is one,
else the root README. `observability/compose.yaml` names a heading of `observability/README.md` this
way. A reference may be wrapped over two comment or prose lines (`compose.yaml` lines 19 and 20
are), so the check joins a wrapped one before it compares; and correct the three stale references in
`service_smoke.rs` to the section that carries the smoke test's manual steps now.

## Acceptance criteria

- [ ] Given a file that names README → "A heading" the README lacks, when check-docs runs, then it
  fails and names the file, the line and the heading.
- [ ] Given the current tree, when check-docs runs, then it passes, `service_smoke.rs` and the
  wrapped reference of `observability/compose.yaml` included.
- [ ] Given a reference in `observability/`, when it is checked, then it is held to
  `observability/README.md`, not the root README.
- [ ] Given the self-test of check-docs, when it runs, then it covers a present and a missing
  heading.

## Read first

- `scripts/check-docs.sh` (header, checks 3 and 10), `scripts/test-check-docs.sh`.
- The convention "A rule changes everywhere it is stated" in `docs/CONVENTIONS.md`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
