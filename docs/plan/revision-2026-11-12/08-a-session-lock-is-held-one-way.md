# Ticket: A session-level advisory lock is held one way

## What to build

`crates/fleet-server/src/postgres/` holds a session-level advisory lock four ways:

- the exclusive lock of an instance that must be alone: a client of its own and a `lost` watch
  (`mod.rs`);
- `BLOB_LOCK`: `lock_session` and `LockHold`, which spawns the unlock on drop (`mod.rs`);
- `PRUNE_LOCK`: a pooled connection taken out of the pool and `Unlocking`, a copy of
  `LockHold::drop` (`audit_prune.rs`);
- the signer's lock of the checkpoints: an `Option<Client>` probed with `SELECT 1`, with no watch
  (`checkpoint.rs`).

3c1c13446 had to let go of two of them separately when their holder ended. Write one session lock
in `postgres/mod.rs`: its own connection, a try and a wait, the unlock on drop, and the watch that
says the session and the lock are gone. The four uses take it.

## Acceptance criteria

- [ ] Given each of the four locks, when its holder ends or drops it, then another session can take
  it at once, decided by one test per lock against the database.
- [ ] Given a held lock whose session the database ends, when its holder next asks, then it learns
  the lock is gone, for each of the four.
- [ ] Given `scripts/check-store.sh postgres`, when it runs, then it passes.

## Read first

- Touches: `crates/fleet-server/src/postgres/mod.rs`,
  `crates/fleet-server/src/postgres/audit_prune.rs`,
  `crates/fleet-server/src/postgres/checkpoint.rs`
- ADR-0023 clauses 12 and 14, ADR-0045 clause 2, ADR-0041; commit 3c1c13446.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
