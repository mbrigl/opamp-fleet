# The Server's processing of an Agent's report has a module of its own

**Kind:** idea, structure.

**Observed.** `crates/fleet-server/src/fleet.rs` is 14 074 lines. `process_presented` (near line
2863) is about 50 lines, and the processing of an Agent's report sits in `report_round`, about 540
lines. What an instance holds of the Agents it carries has already moved to `fleet::held`
(revision-2026-11-06 ticket 03).

**What the old branch had.** Revision ticket 13 of `feat/revision-2026-10-31`, open, not worked, the
first of four slices of the split of `fleet.rs` (the others:
`package-and-deployment-administration-leaves-appstate.md`,
`api-owns-the-wire-shape-of-the-views.md`, `api-is-split-by-resource.md`). It moved report
processing into a module of its own, cut into named steps a unit test can call, behind the same
`AppState` entry point. Retargeted here to `report_round`. It overlaps the draft
`draft-two-use-case-layers-stand-between-the-servers-planes-and-its-core.md` on
`feat/server-drafts`; the human decides which cut leads.
