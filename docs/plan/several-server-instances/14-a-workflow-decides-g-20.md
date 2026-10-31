# Ticket: A workflow decides G-20

## What to build

Two tests decide G-20. The first stops one of three instances without warning under 10 000 simulated
Agents and checks that every Agent it carried is connected to another within 90 s, and that a
revocation committed on any instance ends its session on every instance within 10 s. The second
carries the presence and notifications of 100 000 Agents, about 3 300 reports a second, on three
instances for ten minutes, with no batch late and nothing unread. Both need PostgreSQL, S3 and a
layer-4 balancer that keeps the peer address. A workflow of their own runs them weekly and on
demand; one command runs them locally. To fit check 9 of the documentation check, the workflow runs
one script under `scripts/`, and `check-all` runs the same script, which skips saying so unless the
stack is named in its environment, as the service smoke test and the store checks do.

## Acceptance criteria

- [ ] Given the stack, when the failover test runs, then every Agent of the stopped instance is
  connected to another within 90 s and none reconnects before its drawn delay
- [ ] Given the stack, when a revocation is committed during the failover, then its session ends on
  every instance within 10 s
- [ ] Given the stack, when the load test runs for ten minutes, then no presence batch is late and
  no notification is left unread
- [ ] Given an ordinary `check-all`, when it reaches the script, then it reports skipped, and in a
  CI run with the stack missing it fails
- [ ] Given the documentation check, when it compares the workflows with `check-all`, then it passes

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
  Enforcement, the two G-20 tests; G-20 in the specification
- ADR-0002 for the citation; check 9 of the documentation check
- Golden path: the service smoke workflow and its script, the store check
- Seam: real instances behind a real balancer, started by the script

## Blocked by

- docs/plan/several-server-instances/05-every-change-reaches-every-instance.md
- docs/plan/several-server-instances/06-the-newest-connection-takes-an-agent.md
- docs/plan/several-server-instances/08-the-audit-record-is-one-chain-per-lease.md
- docs/plan/several-server-instances/09-an-instance-on-postgresql-and-s3-needs-no-disk.md
- docs/plan/several-server-instances/10-a-fleet-comes-back-spread-out.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It decides G-20, which the human lands; the five conditions of
[`AGENTS.md` §4](../../../AGENTS.md#4-working-style) would otherwise hold, with the stop at two
failed attempts at a green run.
