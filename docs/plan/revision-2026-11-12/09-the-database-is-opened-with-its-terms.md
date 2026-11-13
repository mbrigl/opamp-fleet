# Ticket: The database is opened with the terms its leases record

## What to build

`postgres::Database` (`crates/fleet-server/src/postgres/mod.rs`) is opened first and given what
its leases record afterwards, each by a setter: the archived file record (`offer_archive`), the
`[audit] key_label` (`set_audit_key_label`), the export sink (`export_audit_to`) and the id its
sweeps mark under (`sweeps_under`). `postgres::lease` reads them when it takes a lease. Nothing
but the order of the calls in `lib.rs` and `main.rs` keeps a lease from being taken before
they are set. Beside that, two ladders of constructors grew one parameter at a time for tests:
`open`, `open_in`, `open_as`, `connect`, `connect_within`, `connect_as`, two of them
`#[doc(hidden)] pub`; and `verify_on`, `verify_in`, `verify_checked_on`, `verify`,
`verify_checked` in `postgres/audit.rs`.

Pass the terms a lease records as one value where the database is opened or the lease taken, so a
lease cannot be taken without them. Fold each ladder into one function with its options.

## Acceptance criteria

- [ ] Given the Server's startup, when it takes its first lease, then the lease records the
  `[audit] key_label`, the archive and the release, which no call order can leave unset; the
  existing lease tests decide it.
- [ ] Given the constructors of `Database` and the verifications of `postgres::audit`, when the
  change lands, then each is one function with its options, and the tests that used the others
  pass.

## Read first

- Touches: `crates/fleet-server/src/postgres/mod.rs`, `crates/fleet-server/src/postgres/lease.rs`,
  `crates/fleet-server/src/postgres/audit.rs`, `crates/fleet-server/src/lib.rs`,
  `crates/fleet-server/src/main.rs`
- ADR-0045 clauses 2 and 16, ADR-0029, ADR-0043 clauses 9 and 14.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
