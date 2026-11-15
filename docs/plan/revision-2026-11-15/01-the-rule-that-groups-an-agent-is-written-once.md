# Ticket: The rule that groups an Agent under its Client Agent is written once

## What to build

How an Agent is grouped for placement (ADR-0052 clause 19) is written in three places, with two
shapes for the result:

- `AppState::group_of` (`crates/fleet-server/src/fleet/placement.rs`, near line 461) decides
  gateway or direct and builds a `Group`.
- `place_on_host(gateway: bool, agents)` (near line 84) applies the rule again in a closure of its
  own, keyed by `String`, with a boolean flag the page reader fills from its own SQL
  (`crates/fleet-server/src/postgres/agent_page.rs`, near line 561).
- `PgAgentStore::write_report` (`crates/fleet-server/src/postgres/agents.rs`, near line 410)
  decides in the adapter what counts as a Client Agent arriving over a Host
  (`is_client_agent && machine`), so the domain no longer says what an arrival is (ADR-0009
  clauses 1 and 4).

`pub type Placed = (Option<InstanceUid>, bool, BTreeMap<String, String>)` (near line 75) repeats
three fields of `struct Placement`, which `inherit` copies one by one.

One pure function of `fleet::placement` groups an Agent; `group_of`, `place_on_host` (keyed by
`Group`) and the arrival check call it, the last through a named helper the adapter calls.
`Placed` becomes a struct that `Placement` holds.

## Acceptance criteria

- [ ] Given the change, when `placement.rs` and `postgres/` are searched for `labels::machine`
  and `is_client_agent`, then the grouping rule appears once, in `fleet::placement`.
- [ ] Given the existing placement tests, the instance tests and the store checks, when the change
  lands, then they pass unchanged.

## Read first

- Touches: `crates/fleet-server/src/fleet/placement.rs`, `crates/fleet-server/src/fleet.rs`,
  `crates/fleet-server/src/postgres/agents.rs`, `crates/fleet-server/src/postgres/agent_page.rs`
- ADR-0052 clause 19, ADR-0045 clauses 8 and 9, ADR-0009.
- CONVENTIONS: "A refusal is decided in one function".

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
