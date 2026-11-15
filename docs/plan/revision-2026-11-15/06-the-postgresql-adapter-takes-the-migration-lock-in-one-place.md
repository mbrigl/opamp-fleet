# Ticket: The PostgreSQL adapter takes the migration lock in one place

## What to build

Two findings in `crates/fleet-server/src/postgres/`, one session together:

- **The migration lock is taken twice.** `Database::apply_schema` (`mod.rs`, near line 426) and
  `take_lease_within` (`lease.rs`, near line 136) both take `pg_advisory_xact_lock(MIGRATION_LOCK)`
  and then try `pg_try_advisory_xact_lock_shared(EXCLUSIVE_LOCK)`, and refuse with two texts that
  already disagree: `HELD_ALONE_BEFORE_MIGRATING` names a Server of 0.7.1 or earlier, the lease's
  inline message does not. One function takes both locks and answers one refusal.
- **`Opening::instance_id` is a public field only a test sets.** It seeds `sweeps_under`
  (`mod.rs`, near line 349), which production sets through `sweep_under` when a lease is taken
  (`lease.rs`). The only `instance_id: Some(..)` is in `postgres/packages.rs`'s tests. The
  field goes, and the test calls `sweep_under`.

## Acceptance criteria

- [ ] Given another process that holds the database alone, when an instance migrates or takes its
  lease, then both refuse with the same text, from one function.
- [ ] Given the change, when `Opening` is read, then it has no `instance_id`, and the blob sweep
  test still marks its deletions under the lease's instance.
- [ ] Given `scripts/check-store.sh` against PostgreSQL, when the change lands, then it passes.

## Read first

- Touches: `crates/fleet-server/src/postgres/mod.rs`, `crates/fleet-server/src/postgres/lease.rs`,
  `crates/fleet-server/src/postgres/packages.rs`
- ADR-0023 clause 22, ADR-0045 clause 16.
- CONVENTIONS: "A refusal is decided in one function".

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
