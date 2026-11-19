# Ticket: An Agent's display values live in `listing` alone

## What to build

`listing` and `fleet::views` depend on each other. `listing.rs` takes `attr_map`,
`display_version`, `capability_names` and `PackageStatusView` from `crate::fleet` (near lines
288, 305, 394 to 399), which `fleet.rs` re-exports for it alone (near line 45). `fleet/views.rs`
calls `listing::display_values` and `listing::names` back. So what an Agent is shown as is split
over two files, and `listing`'s search text is built from an API view type. `listing::names`
takes two whole maps where it reads two keys, so `hosts_of` builds both maps to show two names
(`fleet/views.rs` near lines 51 to 54). The review of eef2c090b found exactly that cost.

The display values and the helpers they need move to `listing`, which then depends on nothing
in `fleet`. `views` keeps the view types and calls `listing`. `names` takes the description.

## Acceptance criteria

- [ ] Given the change, when `crates/fleet-server/src/listing.rs` is read, then it names nothing
  under `crate::fleet`, and `fleet.rs` has no re-export for it.
- [ ] Given an Agent's record, when it is listed and when its view and its Host's view are read,
  then the values are equal, decided by the test that holds them so today (`fleet/views.rs`
  near lines 1487 to 1556).
- [ ] Given the change, when the fleet's unit tests and the Server's suites run, then they pass
  unchanged in number and name.

## Read first

- Touches: `crates/fleet-server/src/listing.rs`, `crates/fleet-server/src/fleet/views.rs`,
  `crates/fleet-server/src/fleet.rs`
- API-lists-paged-by-cursor (what a page derives from each record).
- `docs/CONVENTIONS.md`: "An item lives in the context it changes, and a test with the context it
  verifies".

## Blocked by

- docs/plan/revision-2026-11-19/01-an-agents-attributes-are-read-through-one-lookup.md

## Mode

with the human: DEV-maintainer-decides-agents-implement §5. It shares `listing.rs` and
`fleet/views.rs` with ticket 01, so it is not for a parallel worktree.
