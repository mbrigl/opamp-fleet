# Ticket: The Operator plane reads the fleet from the database

## What to build

On PostgreSQL an instance keeps in memory only the Agents whose connection it holds, loaded at their
first report, and answers the rest from the database; on files the Server keeps the whole fleet as
today. A restarted instance loads no fleet. What reads the whole fleet from memory today reads the
database instead, through the port's `get` and an unfiltered, unpaged `query`:

- the fleet list and an Agent's view;
- the Hosts list and each Host's Agents;
- the deployment reach, which Agents each Deployment would reach;
- a Deployment's rollout, which matches every Agent against its Selector;
- the label inheritance from a Client Agent that another instance carries;
- the artifact download's check of the Agents a host speaks for.

An Agent's view read from the database carries its stored presence, connected only while its
holder holds a live lease. The assignments that answer what a deletion and a collection ask of the
pins are filled by a read of the assignments alone, not by loading the fleet; this needs nothing of
ticket 19. Filters, the cursor and paging stay ticket 07's.

## Acceptance criteria

- [ ] Given a restarted instance, when an Agent it does not carry is read on the Operator plane,
  then its record comes from the database
- [ ] Given the agent port contract, when it runs on both stores, then it passes, `load` on files
  and `get` and `query` on PostgreSQL
- [ ] Given an Agent carried by A, when the fleet list, the Hosts list and the deployment reach are
  read through B, then each names the Agent with its labels, its assignment and A's presence
- [ ] Given a Deployment whose Selector matches an Agent carried by A, when it is rolled out through
  B, then the Agent is released to, though B holds none of it in memory
- [ ] Given a Client Agent carried by A and an Agent of the same Client carried by B, when B
  composes the Agent's effective description, then it inherits the Client Agent's labels
- [ ] Given an artifact offered to an Agent carried by A, when its host fetches it through B, then B
  serves it
- [ ] Given an instance on PostgreSQL, when it starts, then it loads no Agent record and holds none
  until an Agent reports to it
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-the-fleet-record-of-agents-and-hosts clauses 1 to 8, 16, 18, 19 and 21;
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clauses
  6, 8 and 13
- ADR-draft-the-servers-state-and-its-stores clauses 3 and 4
- ADR-0026 for a rollout and the deployment reach, ADR-0027 for the artifact download
- Glossary: Agent, Client Agent, Host, Holder
- Golden path: the agent port contract tests, and the two-instance tests of labels, restarts and
  forgetting in `crates/fleet-server/tests/instances.rs`
- Seam: the port contract, the REST API on both stores, and two instances in one test process

## Blocked by

- docs/plan/several-server-instances/04-the-fleet-record-is-written-per-column.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes where every read of the fleet comes from, and what a
rollout matches.
