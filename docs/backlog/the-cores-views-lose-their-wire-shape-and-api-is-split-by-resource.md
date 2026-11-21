# The core's views lose their wire shape, and `api.rs` is split by resource

**Kind:** idea, structure.

**Observed.** Two slices are open of a four-slice split of `fleet.rs` and `api.rs`. The other
two, report processing and Package and Deployment administration, got their modules with PR
#202 (`fleet::session`, `fleet::delivery`, `fleet::rollout`). What is left of them, report
processing cut into named steps, is not taken now (see *Not now* below); the administration
`api` calls now stands behind `operator_cases` (SRV-two-use-case-layers).

1. **The wire shape of the views.** The view types in `crates/fleet-server/src/fleet/views.rs`
   (`AgentView`, `PendingConfigurationView`, `PendingPackageView`, `PackageStatusView`, near lines
   384 to 593) derive `Serialize`, while `api.rs` mirrors each with a response type and a
   field-by-field `From`, the doc comments copied. The core's `Serialize` looks unused
   (SRV-configurations-compose-one-map).
2. **The REST API by resource.** Every route, view and conversion of about ten resources lives in
   `crates/fleet-server/src/api.rs` (4 376 lines); only the cursor paging is in
   `api/paging.rs` (API-lists-paged-by-cursor).

**Idea.** In that order, one slice a change: the core's serialization of the views dropped where
nothing needs it, so `api` owns the wire shape alone; `api` split into one module per resource
behind the same router and OpenAPI derivation. Each slice updates the overview.

Neither slice depends on how the Server's use cases are cut: the wire shape and the split of
`api` by resource hold under any cut. A split by resource may split `operator_cases` along the same
lines, behind the one type `OperatorCases`.

**Not now: service structs per context.** The human decided ticket 13 of revision-2026-11-18 on
2026-11-18: no context of `fleet` gets a service struct that owns its own state yet, and no spec
is planned for it. No feature asks for it. The use cases stand in a layer of their own since
SRV-two-use-case-layers: `agent_cases` and `operator_cases` call the one `AppState`, which still
holds every context's state. A context that owns its state behind its own service struct comes
with the feature that needs it.

**From.** Revision tickets 13, 26, 27 and 28 of `feat/revision-2026-10-31`, open, not worked;
merged into one entry from four on `feat/bugfixes` (2026-11-14) as
`fleet-and-api-are-split-in-four-slices`. Trimmed and retitled by the design revision of
2026-11-18: the double derivation of the display values went to its ticket 01, and the first
two slices to #202 and its ticket 13.
