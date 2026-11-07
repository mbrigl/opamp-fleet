# Ticket: The mirror columns are removed and the arrays no longer written

## What to build

Release 0.8.0 writes the Deployments' mirror columns (`mirror` in `postgres/deployments.rs`) and
the Hosts' `instance_uids` arrays (`append_to_array` and `unbind_in` in `postgres/ledger.rs`).
The release after 0.8.0 no longer writes the mirror columns and adds nothing to the arrays. It
still takes a released `instance_uid` out of the arrays, because a Server of 0.8.0 reads them for
an `instance_uid` that has no binding. The release after it removes the mirror columns by a
migration, together with the comments that keep them, and stops taking `instance_uid`s out of
the arrays. The arrays themselves go with ticket 02. This follows the stores draft's clause 22.

## Acceptance criteria

- [ ] Given a database migrated by the second release after 0.8.0, when the schema is read,
  then the mirror columns do not exist, and the migration names the release after 0.8.0 as the
  oldest Server
- [ ] Given a binding released by this release, when the arrays are read, then nothing in them
  changed
- [ ] Given a live lease of 0.8.0, when an instance of this release starts, then it refuses as
  the rule of side by side decides
- [ ] Given the store checks, when they run, then they pass

## Read first

- The stores draft,
  ADR-0050
  clause 22;
  ADR-0048
  clause 16
- `postgres/schema.rs`, `postgres/ledger.rs`, `postgres/deployments.rs`

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It is released only in the second release after 0.8.0, which the
human cuts.
