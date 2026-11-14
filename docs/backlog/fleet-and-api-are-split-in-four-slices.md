# `fleet.rs` and `api.rs` are split in four slices

**Kind:** idea, structure.

**Observed.** `crates/fleet-server/src/fleet.rs` is 14 074 lines and
`crates/fleet-server/src/api.rs` 4 539 on `main`. What an instance holds of the Agents it carries
has moved to `fleet::held` (revision-2026-11-06 ticket 03), and the cursor paging to
`api/paging.rs` (ADR-0031); the rest is in the two files:

1. **Report processing.** `report_round` in `fleet.rs`, about 540 lines, processes an Agent's
   report; `process_presented` (near line 2863) is about 50.
2. **Package and Deployment administration.** It mostly forwards to the package and Deployment
   stores and adds the refusal of what an assignment still names, inside an `AppState` of many
   public methods (near lines 5167 to 5280 of `fleet.rs`).
3. **The wire shape of the views.** The view types in `fleet.rs` (near lines 5928, 6084, 6110 and
   6126) derive `Serialize`, while `api.rs` mirrors each with a response type and a
   field-by-field `From`, the doc comments copied. The core's `Serialize` looks unused (ADR-0032).
4. **The REST API by resource.** Every route, view and conversion of about ten resources lives in
   `api.rs`.

**Idea.** In that order, one slice a change: report processing into a module of its own, cut into
named steps a unit test can call, behind the same `AppState` entry point; Package and Deployment
administration into a module `api` calls, keeping the refusal; the core's serialization of the
views dropped where nothing needs it, so `api` owns the wire shape alone; `api` split into one
module per resource behind the same router and OpenAPI derivation. Each slice updates the
overview.

It overlaps the draft
`draft-two-use-case-layers-stand-between-the-servers-planes-and-its-core.md` on
`feat/server-drafts`, which places such use cases in a layer of their own; the human decides which
cut leads.

**From.** Revision tickets 13, 26, 27 and 28 of `feat/revision-2026-10-31`, open, not worked;
merged into one entry from four on `feat/bugfixes` (2026-11-14).
