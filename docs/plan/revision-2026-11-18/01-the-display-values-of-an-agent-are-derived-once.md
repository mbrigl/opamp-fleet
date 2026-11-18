# Ticket: The display values of an Agent are derived once

## What to build

`listing::derive` (`crates/fleet-server/src/listing.rs`, near line 243) and the build of
`AgentView` in `crates/fleet-server/src/fleet/views.rs` (near lines 720 to 770) each derive the
display values of an Agent. The attribute split (`fleet::attr_map`) and the platform
(`Platform::reported`) already go through one function each. Written twice are the attribute
lookups, the OS description's fallback to `os.type`, the match from a remote configuration
status to its name, and the platform's `.tag()` with `unwrap_or_default()`. The copy has
drifted twice already: the fleet search read another platform than the offer (110dcf853), and
the Agents tab did too (707ee58e4), each fixed on its own path. Ticket 02 of
revision-2026-11-15 asked for one place as its third criterion; the change that landed it fixed
the platform and left the two copies.

One function derives the values both read, and the list and the view take them from it. The
list and the view keep their shapes and their tests.

## Acceptance criteria

- [ ] Given an Agent's record, when it is listed and when its view is read, then the service
  name, the instance name, the version, the OS, the platform and the configuration status are
  equal, decided by one test over both.
- [ ] Given the change, when `listing.rs` and `fleet/views.rs` are read, then the attribute
  lookups and the status names are written once.
- [ ] Given the change, when the fleet's unit tests and the Server's suites run, then every
  existing test passes under its name, and the one test of the first criterion is the only one
  added.

## Read first

- Touches: `crates/fleet-server/src/listing.rs`, `crates/fleet-server/src/fleet/views.rs`
- `docs/CONVENTIONS.md`: "A rule two paths apply is one function".
- ADR-0030 (what a page derives from each record), ADR-0037 (the version a reader is shown).
- The backlog entry
  `docs/backlog/the-cores-views-lose-their-wire-shape-and-api-is-split-by-resource.md` named the
  same copy; the revision moved that paragraph here.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
