# `fleet.rs` and `api.rs` are split in four slices

**Kind:** idea, structure.

**Observed.** PR #202 split `crates/fleet-server/src/fleet.rs` into one module per context, so
the first two slices below have their module: report processing is `fleet::session`, and
Package and Deployment administration are `fleet::delivery` and `fleet::rollout`. What is left
of the two slices, report processing cut into named steps a unit test can call and the
administration as a use case `api` calls, waits for the decision of ticket 13 of
revision-2026-11-18 (service structs per context). The other two slices are open:

3. **The wire shape of the views.** The view types in `crates/fleet-server/src/fleet/views.rs`
   (`AgentView`, `PendingConfigurationView`, `PendingPackageView`, `PackageStatusView`, near lines
   384 to 593) derive `Serialize`, while `api.rs` mirrors each with a response type and a
   field-by-field `From`, the doc comments copied. The core's `Serialize` looks unused (ADR-0031).
4. **The REST API by resource.** Every route, view and conversion of about ten resources lives in
   `crates/fleet-server/src/api.rs` (4 581 lines on `main`); only the cursor paging is in
   `api/paging.rs` (ADR-0030).

**Idea.** In that order, one slice a change: the core's serialization of the views dropped where
nothing needs it, so `api` owns the wire shape alone; `api` split into one module per resource
behind the same router and OpenAPI derivation. Each slice updates the overview.

It overlaps the draft
`draft-two-use-case-layers-stand-between-the-servers-planes-and-its-core.md` on
`feat/server-drafts`, which places such use cases in a layer of their own; the human decides which
cut leads.

**From.** Revision tickets 13, 26, 27 and 28 of `feat/revision-2026-10-31`, open, not worked;
merged into one entry from four on `feat/bugfixes` (2026-11-14). Trimmed by the design revision
of 2026-11-18: the double derivation of the display values went to its ticket 01, and the
first two slices to #202 and its ticket 13.
