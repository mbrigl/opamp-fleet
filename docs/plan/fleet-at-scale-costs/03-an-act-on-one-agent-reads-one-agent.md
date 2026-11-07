# Ticket: An act on one Agent reads one Agent

## What to build

Operator acts on one Agent or one group read the whole fleet on PostgreSQL:

- `deployment_reach()` reads the whole fleet on every `GET /deployments` and after every write
  that answers with its reach; `host_agents()` is a whole `query(None)`
- one instance's Package writes wait behind an upload's bucket check under the `writes` mutex

Each reads the Agents it concerns, outside the fleet lock where it can. A report reads the Client
Agents of its group held elsewhere before it takes the fleet lock; the other placements read them
under it (`client_agents_elsewhere`). It blocks no stage: the G-20 load test met its target at
100 000 Agents without it.

## Acceptance criteria

- [ ] Given `GET /deployments` or a write that answers with a Deployment's reach, when it runs,
  then it reads the Agents the Deployment's selector concerns, not the whole fleet
- [ ] Given a Package write on an instance whose upload checks the bucket, when it runs, then it
  does not wait behind the bucket check
- [ ] Given the integration suite and the store checks, when they run, then they pass unchanged

## Read first

- ADR-0048 clause 13; ADR-0043 clauses 16 and 19
- `fleet.rs` `deployment_reach`, `host_agents`, and `arrived_over`,
  the read of the Agents behind a Gateway through the port's `query_page`, which `stored_agent`
  and a Gateway's download already use

## Blocked by

- docs/plan/fleet-at-scale-costs/02-a-pin-is-counted-by-index.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
