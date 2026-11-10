# The REST API owns the wire shape of the views

**Kind:** idea, structure.

**Observed.** The view types in `crates/fleet-server/src/fleet.rs` (on `main`, near lines 5928,
6084, 6110 and 6126) derive `Serialize`, while `crates/fleet-server/src/api.rs` mirrors each with a
response type and a field-by-field `From`, the doc comments copied. The core's `Serialize` looks
unused.

**What the old branch had.** Revision ticket 27 of `feat/revision-2026-10-31`, open, not worked:
drop the core's serialization of the views where nothing needs it, so `api` owns the wire shape
alone (ADR-0032).
