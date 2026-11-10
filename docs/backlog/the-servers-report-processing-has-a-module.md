# The Server's processing of an Agent's report has a module of its own

**Kind:** idea, structure.

**Observed.** `crates/fleet-server/src/fleet.rs` is 14 645 lines on `main`. `process_presented`
(near line 3870) has shrunk to about 40 lines, and the processing of an Agent's report now sits in
`report_round`, about 530 lines.

**What the old branch had.** Revision ticket 13 of `feat/revision-2026-10-31`, open, not worked, the
first of four slices of the split of `fleet.rs` (the others:
`package-and-deployment-administration-leaves-appstate.md`,
`api-owns-the-wire-shape-of-the-views.md`, `api-is-split-by-resource.md`). It moved report
processing into a module of its own, cut into named steps a unit test can call, behind the same
`AppState` entry point. Retargeted here to `report_round`. It overlaps `main`'s ticket
`docs/plan/revision-2026-11-06/03-an-agents-holder-is-a-module-of-its-own.md` and the draft
`draft-two-use-case-layers-stand-between-the-servers-planes-and-its-core.md` on
`feat/server-drafts`; the human decides which cut leads.
