# Ticket: A fleet comes back spread out

## What to build

Every Server, one or several, gets a fleet back without a storm. The Client draws its first
reconnect after a lost connection uniformly over [0, 30 s), and later failures follow the
exponential steps. The Server takes at most `max_new_connections_per_sec` new Agent-plane
connections a second and answers the rest `503` with a `Retry-After` drawn from 1 to 30 s; both
transports wait that or the next step, whichever is longer. On SIGTERM the Server drains over
`drain_secs`, closing its sessions spread over that time.

## Acceptance criteria

- [ ] Given a lost connection, when the Client reconnects, then the first delay is drawn over [0, 30
  s) and the next ones follow the exponential steps
- [ ] Given a refused WebSocket upgrade with a `Retry-After`, when the Client retries, then it waits
  that or the next step, whichever is longer
- [ ] Given 300 new connections in one second, when they reach the Server, then 200 are admitted and
  the rest answered `503` with a `Retry-After` between 1 and 30 s
- [ ] Given 1 000 sessions and `drain_secs = 20`, when the Server gets SIGTERM, then the sessions
  are closed spread over 20 s with code `1001`
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-tls-1-3-two-planes-and-bounded-input clauses 4 and 17;
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clauses
  3 and 20
- ADR-0012, which the restatement replaces
- Golden path: the backoff tests of the protocol crate, the shutdown signal test
- Seam: the protocol crate's backoff and WebSocket driver; the Server's listener

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes the Client's transport and the Server's admission path.
