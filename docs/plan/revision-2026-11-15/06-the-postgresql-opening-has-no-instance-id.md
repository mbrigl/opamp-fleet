# Ticket: The PostgreSQL opening has no instance id

## What to build

`Opening::instance_id` (`crates/fleet-server/src/postgres/mod.rs`, near line 198) is a public field
that only a test sets. It seeds `sweeps_under` (near line 349). Production sets that value through
`sweep_under` when a lease is taken (`lease.rs`). The only `instance_id: Some(..)` is in the tests
of `postgres/packages.rs`. The field goes, and the test calls `sweep_under`.

**Effort.** S.

## Acceptance criteria

- [ ] Given the change, when `Opening` is read, then it has no `instance_id`.
- [ ] Given the blob sweep test, when it runs, then it still marks its deletions under the lease's
  instance.

## Read first

- Touches: `crates/fleet-server/src/postgres/mod.rs`, `crates/fleet-server/src/postgres/packages.rs`
- ADR-0045 clause 16.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
