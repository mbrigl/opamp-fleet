# Package and Deployment administration leaves AppState

**Kind:** idea, structure.

**Observed.** Package and Deployment administration in `crates/fleet-server/src/fleet.rs` mostly
forwards to the package and Deployment stores and adds the refusal of what an assignment still
names, inside an `AppState` of many public methods (on `main`, near lines 5167 to 5280).

**What the old branch had.** Revision ticket 26 of `feat/revision-2026-10-31`, open, not worked, the
second slice of the split of `fleet.rs` after
`the-servers-report-processing-has-a-module.md`: move it into a module of its own that `api`
calls, keeping the refusal, and update the overview. It overlaps the draft
`draft-two-use-case-layers-stand-between-the-servers-planes-and-its-core.md` on
`feat/server-drafts`, which places such use cases in a layer of their own.
