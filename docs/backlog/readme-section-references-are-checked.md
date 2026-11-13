# A reference to a README section by name is checked

**Kind:** defect, check.

**Observed.** Check 3 of `scripts/check-docs.sh` resolves links and anchors, never a section named
in prose. On `main`, `crates/fleet-agent/tests/service_smoke.rs` line 26 cites "the manual checklist
in `README.md`", and the README has no checklist. Part of the old ticket is done: the hook and
integration text it named moved to `CONTRIBUTING.md`, whose headings a check would read the same
way.

**What the old branch had.** Revision ticket 20 of `feat/revision-2026-10-31`, open, not worked:
extend `check-docs.sh` so every `README → "<Heading>"` and `README.md → "<Heading>"` in a tracked
file names a heading of the README it means, the one beside the file, else the root README,
joining a reference wrapped over two lines; and correct the stale references in
`service_smoke.rs`.
