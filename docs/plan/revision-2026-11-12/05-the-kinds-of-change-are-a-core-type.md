# Ticket: The kinds of change an instance hears of are a type of the core

## What to build

What changed, as one instance tells the others, is a vocabulary of strings in the adapter
(`crates/fleet-server/src/postgres/notify.rs`), and its meaning is mapped in the composition root
(`Due::of` in `crates/fleet-server/src/instance.rs`). A new kind touches the adapter's constants,
a field of `Due`, `Due::of`, `Due::and`, `Due::EVERYTHING` and the read that follows. The core
port widened for it too: `AgentStore::write_report` gained `arrived: Option<&str>` in 06efe2866
only so the PostgreSQL adapter can announce a Client Agent's first arrival behind a Gateway, a
parameter the filesystem adapter ignores and the port's surface in ADR-0030 clause 2 does not
name.

Name the kinds of change as one type of the core, which the adapters announce and `follow` maps
once, so a new kind is one variant and a match that misses it does not compile. An arrival becomes
a change the adapter derives from the write, not a parameter of `write_report`. The kinds are those
ADR-0045 lists in its clause 9; this ticket adds none.

## Acceptance criteria

- [ ] Given a kind of change, when it is added, then the compiler names every place that maps it.
- [ ] Given `AgentStore::write_report`, when the change lands, then it has the surface ADR-0030
  clause 2 names, and the arrival of a Client Agent over a Gateway's Host is still announced,
  decided by the existing test of 06efe2866.
- [ ] Given `tests/instances.rs` and the store checks, when the change lands, then they pass.

## Read first

- Touches: `crates/fleet-server/src/postgres/notify.rs`, `crates/fleet-server/src/instance.rs`,
  `crates/fleet-server/src/agent_store.rs`, `crates/fleet-server/src/postgres/agents.rs`,
  `crates/fleet-server/src/fleet.rs`, `crates/fleet-core/tests/dependency_direction.rs`
- ADR-0009, ADR-0030 clause 2, ADR-0045 clauses 9 and 10.

## Blocked by

- None (can start immediately). It decides nothing ADR-0045 does not decide.

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
