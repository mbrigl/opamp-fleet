# Ticket: Cross-instance tests take the real routes

## What to build

Some cases of several instances are tested through a seam where the route an operator or an Agent
takes is not, or not at all:

- `a_revocation_on_one_instance_ends_the_session_on_the_other` in `tests/instances.rs` revokes
  through `revocations.revoke_certificate`, not the Operator plane's route
- the `Deciding` answer, `409` while another instance holds a request, has no test through the
  REST route (`deciding()` in `api.rs`)
- a Gateway serving its cached revocation list and floors across a failover: the Gateway test only
  checks that a new downstream peer is admitted afterwards
- the takeover signal on the re-take path (`instance.rs`, a new lease after a lost one whose take
  removed lapsed leases) is reached by no test
- the bound of the enrolment queue counted across instances, which ADR-0025's Enforcement lists,
  has no test in `tests/instances.rs` that could be found

Each gets a test through the route, beside or instead of the seam.

## Acceptance criteria

- [ ] Given two instances, when a certificate is revoked through A's Operator plane, then the
  session on B ends
- [ ] Given two instances deciding one request, when the second asks through REST, then it is
  answered `409` as the manual says
- [ ] Given a Gateway on A and A stopped, when the Gateway connects to B, then it serves the list
  and the floors B holds, a revocation committed meanwhile included
- [ ] Given an instance that lost its lease and takes a new one removing a lapsed lease, when it
  does, then the takeover's staging cleanup runs
- [ ] Given two instances queueing requests, when the bound is reached, then it counts both

## Read first

- ADR-0025 clauses 3, 9 and 10 and its Enforcement; ADR-0040 on the Gateways' list
- Golden path: `tests/instances.rs` and its two-instance harness

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
