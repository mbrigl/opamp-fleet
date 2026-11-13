# Ticket: The enrolment port loses `decide`, which nothing but tests calls

## What to build

64e980e66 replaced the one act that decided an enrolment request with `approve` and `reject`.
`EnrolmentStore::decide` stayed in the port (`crates/fleet-server/src/enrolment.rs`) and in its
PostgreSQL adapter (`crates/fleet-server/src/postgres/enrolment.rs`), with no caller outside
tests. Its tests pin a third locking rule, a `FOR UPDATE` that waits, beside `reject`'s queue lock
and `NOWAIT`, and 9c17752bc had to explain a failure of one of them as a test defect.

Remove `decide` from the port and both adapters, and move what its tests decide onto `approve` and
`reject`, where the product decides it.

## Acceptance criteria

- [ ] Given the enrolment port, when the change lands, then it has no method that no product code
  calls.
- [ ] Given each behaviour a `decide` test pinned, when the change lands, then a test of
  `approve` or `reject` decides it, or the hand-off names why it no longer applies.
- [ ] Given the enrolment contract on both stores, when it runs, then it passes.

## Read first

- Touches: `crates/fleet-server/src/enrolment.rs`, `crates/fleet-server/src/postgres/enrolment.rs`
- ADR-0027, ADR-0023; commit 9c17752bc.
- The backlog entry a-rejection-after-a-failed-approval-is-answered-deciding-for-a-moment, whose
  idea copies the ending of `decide` and `reject`; it stays with `reject` once `decide` is gone.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
