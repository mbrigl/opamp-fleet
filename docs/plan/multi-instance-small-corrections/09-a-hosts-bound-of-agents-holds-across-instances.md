# Ticket: A host's bound of Agents holds across instances

## What to build

A host speaks for at most 256 Agents (ADR-0012, *A host speaks for at most 256 Agents*;
`MAX_UIDS_PER_HOST`). `Revocations::admits` counts it from the instance's memory: the
`instance_uid`s its copy of the Host lists, and the bindings it is making for the Host. On
PostgreSQL `bind_seen` writes a binding without counting the Host's bindings in the database. So a
host whose connections reach several instances can be bound up to 256 Agents on each before one
instance's copy shows the others' bindings; the bound holds per instance, not for the fleet.

The window is the time an instance takes to take up another's notice of a binding. Since ticket 40
of several-server-instances that notice follows the commit, and a lost one is taken up at the next
lease renewal, so the window can be one renewal long.

`bind_seen` already holds the Host's row `FOR NO KEY UPDATE`, so two binders of one Host wait for
each other. It counts the Host's bindings there and refuses a binding past the bound, so the bound
is counted in the transaction that would pass it, as ADR-0025 clause 19 does for the
other bounds.

## Acceptance criteria

- [ ] Given a Host with 255 bindings made through instance A, when instance B, which has not
  taken them up, binds two more `instance_uid`s to it, then one is bound and the other refused
  naming the bound
- [ ] Given two instances binding to one Host at once, when the bound is reached, then the
  database holds 256 bindings for it and no more

## Read first

- ADR-0012, *A host speaks for at most 256 Agents*
- ADR-0025
  clause 19
- `revocation.rs` `admits`, `check_report_by_act`; `postgres/ledger.rs` `bind_seen`

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
