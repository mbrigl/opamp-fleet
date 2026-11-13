# Ticket: A wait the Server asks for is read and floored one way

## What to build

The rule that a Client waits what the Server's `Retry-After` asks, and at least a step of its
backoff (ADR-0012 clause 4), is written three times:

- the WebSocket driver's `next_delay` (`crates/opamp/src/client/ws.rs`);
- the plain-HTTP driver's `at_least_a_step` (`crates/opamp/src/client/http.rs`), which 6e3656376
  added after the WebSocket driver already had it;
- the package download (`crates/fleet-agent/src/packages.rs`), with its own parser of
  `Retry-After` and a floor of 1 s, because `opamp::client::retry_after_seconds` is `pub(crate)`.

Give `opamp::client::backoff` one function that floors a named wait at the next step, make the
parser public, and have the three call them. The 30 s fallback stays with plain HTTP, the one the
Baseline asks it of.

## Acceptance criteria

- [ ] Given a `Retry-After` of zero, when the WebSocket driver, the plain-HTTP driver and the
  package download each get it, then each waits at least a step of its backoff, decided by their
  existing tests.
- [ ] Given the three, when the change lands, then one parser and one floor serve them.

## Read first

- Touches: `crates/opamp/src/client/backoff.rs`, `crates/opamp/src/client/mod.rs`,
  `crates/opamp/src/client/ws.rs`, `crates/opamp/src/client/http.rs`,
  `crates/fleet-agent/src/packages.rs`
- ADR-0012 clause 4, ADR-0045 (the restated transport rule), ADR-0034 clause 47, ADR-0008 clause 2
  (`opamp` is published; a public function is part of its interface).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
