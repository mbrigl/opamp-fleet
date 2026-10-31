# Ticket: The store check tells a forward with no store behind it from a failing store

## What to build

The Dev Container forwards the host's PostgreSQL (5432) and S3 store (9100) with socat listeners
(`.devcontainer/forward-otlp.sh`) that accept a TCP connection even while the host stack is down,
and close it at once. A probe by TCP connect said "up"; the store tests then failed with "connection
closed" while creating the schema, which read as a code error. The script's header says the
opposite: "a connection made while it is down fails".

Before `scripts/check-store.sh` runs the tests, probe the named store at its protocol — a PostgreSQL
startup exchange, an HTTP request to the S3 endpoint — and fail with "the store at <address> does
not answer" when nothing answers behind the listener. Correct the header of `forward-otlp.sh`.

## Acceptance criteria

- [ ] Given a socat forward with the host stack down, when `check-store.sh postgres` or `s3` runs,
  then it fails before cargo test with a message naming the address and the forward.
- [ ] Given a running store, when the check runs, then the probe passes and the tests run as before.
- [ ] Given `forward-otlp.sh`, when its header is read, then it says a connection is accepted and
  closed while the stack is down.

## Read first

- ADR-0029 (the store tests), ADR-0004.
- `scripts/check-store.sh`, `.devcontainer/forward-otlp.sh`, `observability/compose.yaml`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
