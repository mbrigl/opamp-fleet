# Ticket: A Client without a host is not re-keyed by its own reconnect

## What to build

When the Client has verified offered connection settings, its WebSocket driver
(`crates/opamp/src/client/ws.rs`, `AfterReply::Reconnect`) sends a close frame on connection 1
and dials connection 2 at once. It does not wait for the Server's close. When the first report on
connection 2 reaches the Server before the Server has run `on_closed` for connection 1, the
Server still sees the Agent carried by a live connection. A connection that presented no host
has no same-host peer, so `Carried::of` answers `ByAnotherPeer` and the Server re-keys every
Agent the Client carries (ADR-0025 clause 8). The Server then holds two records of each, the old
one disconnected. Through a plain `ws://` endpoint with `Admission::open()` this happened on CI
in `a_verified_offer_restarts_the_gateway_and_leaves_the_supervisors_running`
(`crates/fleet-agent/tests/gateway_and_supervisor_e2e.rs`). That test now follows the re-keyed
record. A 300 ms delay before `on_closed` makes the re-key happen on every run.

On a verified connection-settings change the Client waits a bounded time, 1 to 2 s, for the
Server's close of connection 1, its close frame or the end of the TCP connection, before it
dials connection 2. A Server that does not answer within the bound does not hold the Client
back.

## Acceptance criteria

- [ ] Given a Server that answers the close of connection 1 only after a delay shorter than the
  bound, when the Client reconnects on verified settings, then connection 2 is dialled only after
  that answer, and the Agent keeps its `instance_uid`. This test is written first and fails before
  the change
- [ ] Given a Server that never answers the close, when the Client reconnects on verified
  settings, then connection 2 is dialled once the bound has passed
- [ ] Given the existing driver and e2e tests, when they run, then they pass

## Read first

- ADR-0025 clause 8, on duplicate detection and the connection that presented no host
- `crates/opamp/src/client/ws.rs` `AfterReply::Reconnect` and `Served::Reconnect`;
  `crates/fleet-server/src/fleet.rs` `Carried::of`; `crates/fleet-server/src/transport.rs`
  `on_closed`
- `crates/fleet-agent/tests/gateway_and_supervisor_e2e.rs`
  `a_verified_offer_restarts_the_gateway_and_leaves_the_supervisors_running`

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. This does not block G-20. ADR-0025 clause 8 intends the re-key of an
Agent whose connection presented no host. In production the Client presents a host certificate,
and the same-host rule hands the Agent to the newer connection without a re-key.
