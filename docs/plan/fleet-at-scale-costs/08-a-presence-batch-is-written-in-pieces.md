# Ticket: A presence batch is written in pieces

## What to build

An instance writes the presence of every Agent it holds in one transaction of two statements, a
`SELECT … FOR UPDATE` and an `UPDATE … FROM unnest(…)`, each binding arrays of every Agent in the
batch (`postgres/agents.rs`). Each of the pool's connections keeps the buffers it encoded a
message in, at the size of the largest message it has carried: the client's bind buffer and the
connection's write buffer, each up to twice the largest batch. Under the G-20 load test at
16 700 Agents an instance they held about 58 MiB after four hours, of a bound of about 70 MiB; at
33 000 Agents an instance the bound is about 150 MiB. They follow the most Agents an instance has
held at once and `pool_size`, not time.

A presence batch is written in pieces of about 1 000 Agents for each statement, in the same
transaction and in the same order of `instance_uid`, so that a connection's buffers stay at the
size of one piece.

It does not block G-20.

## Acceptance criteria

- [ ] Given a presence batch of more Agents than a piece holds, when it is written, then each
  statement binds at most a piece, every row is locked in the order of its `instance_uid` across
  the pieces, and the batch commits or rolls back as one transaction
- [ ] Given the G-20 load test at 50 000 Agents for an hour, when the pool's buffers are read per
  allocation site, then they hold at most about 5 MiB an instance
- [ ] Given G-20 at 100 000 Agents and both failovers, when they run, then they pass with the
  rates and bounds they pass with today
- [ ] Given the integration suite and the store checks, when they run, then they pass unchanged
- [ ] Given the pieces have landed, when the manual's *Running it*, the CHANGELOG and the comment
  on `MALLOC_ARENAS` are read, then they state the bound the pieces give in place of today's

## Read first

- `docs/manual/server.md`, *Running it*: the bound of the pool's buffers and the act table
- `crates/fleet-server/src/postgres/agents.rs`, the presence batch and its two statements
- `crates/fleet-server/src/postgres/mod.rs`, `run_writing` and the pool's `Cached` statements
- `crates/fleet-server/tests/g20.rs`, `FLEET_G20_STEADY` and the load test's acts
- ADR-0048, its clauses on presence batches

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
