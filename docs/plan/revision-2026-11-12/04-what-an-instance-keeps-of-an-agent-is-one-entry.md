# Ticket: What an instance keeps of one Agent is one entry, moved and dropped as one

## What to build

`AppState` keeps what it knows of one Agent in parallel maps keyed by its `instance_uid`: `fleet`,
`written`, `presence_written`, `unsure_rekeys`, `unput_restarts` and `taking`, beside the `groups`
index (`crates/fleet-server/src/fleet.rs`). A forget, a re-key and a take-up each list the maps
by hand (`drop_forgotten_read` in `fleet/held.rs`, `follow_move` in `fleet.rs`). afe4fec00 was
needed because the re-key missed `unput_restarts`; a map added later is missed the same way, and no
test lists the maps.

Keep what an instance holds of one Agent as one entry of `fleet::held`, which a re-key moves and
a forget drops as a unit, and keep the lock order the fields' docs state. The cross-instance case of
`docs/backlog/a-kept-restart-is-lost-across-a-rekey-through-another-instance.md` stays a backlog
entry; this ticket is the in-process half.

## Acceptance criteria

- [ ] Given an Agent with a kept restart, an unsure re-key and written presence, when it is
  re-keyed on the same instance, then each moves to the new identity, decided by one test.
- [ ] Given the same Agent, when it is forgotten, then nothing of it is left in the instance's
  memory, decided by one test that does not name the maps.
- [ ] Given the existing tests and the store checks, when the change lands, then they pass.

## Read first

- Touches: `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/held.rs`
- ADR-0030 clauses 10 and 14, ADR-0045 clause 8; the docs of the fields of `AppState`, which state
  the lock order.
- `docs/plan/revision-2026-11-12/03-a-missed-notification-is-caught-up-from-one-read.md` touches
  the same module; work them one after the other.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
