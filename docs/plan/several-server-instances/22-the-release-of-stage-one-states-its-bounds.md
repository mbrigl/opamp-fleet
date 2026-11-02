# Ticket: The release of stage one states its bounds

## What to build

Stage one of this plan is released before G-20 is decided, so the operator reads what it does and
what it does not yet do. The manual's Server pages and the release notes say: several instances
share one PostgreSQL database and one bucket, and each failover is described as it runs; instances
are upgraded together, stopped and started again, since two versions side by side come with stage
two; no load is proven yet; and an instance that holds a CA key as a file runs alone, so with such a
key failover is a standby that takes over once the lease has lapsed. The order of moving one Server
on files to several instances on PostgreSQL and S3 is given as far as stage one reaches.

Two bounds of label inheritance across instances are named too. An Agent behind a Gateway does not
inherit, in the memory of the instance that carries it, the labels of a Client Agent another
instance carries, until ticket 07's host filter; what the Operator plane shows inherits them. And a
Client Agent bound to its Host on one instance just before another instance first places a fellow
Agent of that Host is inherited only once that group is placed again: at a change of the Client
Agent's labels, a forget, a change of the Agent's report, or a missed notification.

## Acceptance criteria

- [ ] Given the manual, when an operator follows it from one Server on files, then they reach two
  instances on PostgreSQL and S3, upgraded together, in the order it gives
- [ ] Given a Server whose configuration names a CA key file, when the manual describes several
  instances, then it says such an instance runs alone and how its standby takes over
- [ ] Given the release notes of the stage, when an operator reads them, then they name the three
  bounds: upgrade together, no load proven, a CA key file means a standby
- [ ] Given the manual, when an operator reads about labels on several instances, then it names
  the two bounds of inheritance: behind a Gateway, and a Client Agent bound just before its fellow
  Agent is first placed
- [ ] Given the manual check, when it runs, then it passes

## Read first

- The spec's *Stages*; the clauses of the drafts that stage one implements
- The manual's conventions: no ADR cited, every configuration key in its table
- Golden path: the manual's Server pages on the stores and the container

## Blocked by

- docs/plan/several-server-instances/19-what-an-assignment-pins-is-decided-in-the-transaction.md
- docs/plan/several-server-instances/20-the-audit-record-on-postgresql-is-kept-for-retain-days.md
- docs/plan/several-server-instances/21-the-operator-plane-reads-the-fleet-from-the-database.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero; the human releases what it describes.
