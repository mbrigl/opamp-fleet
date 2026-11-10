# The REST API is split by resource

**Kind:** idea, structure.

**Observed.** `crates/fleet-server/src/api.rs` is 4 539 lines on `main` for about ten resources, and
every route, view and conversion lives in it. One part is split off already:
`crates/fleet-server/src/api/paging.rs` holds the cursor paging (ADR-0031).

**What the old branch had.** Revision ticket 28 of `feat/revision-2026-10-31`, open, not worked,
after `api-owns-the-wire-shape-of-the-views.md`: split `api` into one module per resource behind
the same router and OpenAPI derivation, and update the overview's REST views paragraph.
