# Ticket: The fleet record is read per Agent and written per column

## What to build

On PostgreSQL an instance keeps in memory only the Agents whose connection it holds, loaded at their
first report, and answers the rest from the database; on files the Server keeps the whole fleet as
today. A report writes only what a report decides; a label change, a requested restart and a rollout
write their own columns, so a write by the carrying instance never undoes what another instance set.
Presence, with its holder, is written in batches every `presence_interval_secs`. A write that
touches several Agents' rows, a rollout's release and a presence batch among them, takes them in
the order of their `instance_uid`, so two such writes on two instances do not deadlock; today a
rollout's release writes them unsorted. A label change, a restart request and a forgotten Agent
are each announced in ticket 05's notification format, in the transaction that writes them, and
the instance that carries the Agent takes them up from those columns; labels place the Agent in
its groups again.

## Acceptance criteria

- [ ] Given an Agent carried by A, when its labels are set through B and A writes a report before
  the notification arrives, then the labels stay as B set them
- [ ] Given a restart requested through B, when A writes the Agent's next report, then the request
  stays until it is carried out, and is cleared only if no later request set it again
- [ ] Given an Agent carried by A, when its labels are set, a restart is requested or it is
  forgotten through B, then A acts on it within one second: it places the Agent in its groups
  again, restarts it, or ends its session
- [ ] Given an instance carrying 1 000 Agents at a 30-second heartbeat, when they report, then
  presence is written in batches and never once per heartbeat
- [ ] Given a restarted instance, when an Agent it does not carry is read on the Operator plane,
  then its record comes from the database
- [ ] Given the agent port contract, when it runs on both stores, then it passes, `load` on files
  and `get` and `query` on PostgreSQL
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-the-fleet-record-of-agents-and-hosts clauses 1 to 8 and 21;
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clauses
  4 to 8
- ADR-draft-the-servers-state-and-its-stores clauses 3 and 4
- Glossary: Agent, Client Agent, Host; the holder term lands here
- Golden path: the agent port contract tests and the rollout's whole-or-nothing tests
- Seam: the port contract, plus two instances with a notification held back

## Blocked by

- docs/plan/several-server-instances/01-two-instances-share-one-database.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes the record every decision about an Agent rests on, and the
schema.
