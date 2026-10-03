# ADR-0032: One OpAMP server endpoint for every server surface — the endpoint carries the communication, the application only answers

- **Status:** ⚪ superseded by [ADR-0036](0036-the-whole-opamp-communication-layer-in-the-opamp-crate.md)
- **Date:** 2026-10-03
- **Deciders:** Markus Brigl
- **Applies to:** `crates/opamp/src/server.rs`, and the OpAMP endpoint of each server surface: `crates/fleet-server/src/transport.rs`, `crates/fleet-agent/src/gateway/mod.rs` and `crates/fleet-agent/src/supervisor/endpoint.rs`

## Context

This project serves the Server side of OpAMP in three places: the Server (`crates/fleet-server`, axum,
both transports), the Gateway downstream (`crates/fleet-agent/src/gateway`, axum, both transports) and
the Supervisor Endpoint (`crates/fleet-agent/src/supervisor/endpoint.rs`, WebSocket only). Each wrote the
communication itself — telling an upgrade from a POST, the media type, gzip with the limit after
decompression, the receive limit, the framing, the 1009 close, never sending an oversized reply —
and the copies had already drifted once: the Gateway hung up on an oversized message without the
1009 status.

What differs is only what happens *with* a message: the Server answers from its fleet state and
pushes when the desired state changes; the Gateway forwards upstream and routes replies back later;
the Supervisor Endpoint answers with its capability set and folds the content into its Agent.
[ADR-0011](0011-workspace-crates-and-configuration.md) deferred exactly this abstraction until a
third server surface appeared; the Supervisor Endpoint is that third.

The endpoint lives in the `opamp` crate behind its `server` feature
([ADR-0031](0031-one-opamp-crate-a-publishable-wire-layer-with-client-and-server-features.md)).

## Decision

We will implement the server side of the communication **once**, in `opamp::server`, generic over a
**handler** the application implements, and run all three surfaces on it.

1. **The endpoint implements the communication**: both transports on one path, the media type and
   gzip via `opamp::endpoint`, the limit in both directions, framing via `opamp::frame`, the 1009
   close, and the per-connection loop. Nothing in it knows what an Agent is.
2. **The application implements `Handler`**, the shape `opamp-go`'s server uses:
   `on_connecting` (accept or refuse, creating the application's state for the connection),
   `on_message` (the reply, nothing, or a refusal), `outbound` and `on_outbound` (what a connection
   is sent unasked), `on_unreadable`, and `on_closed`.
3. **It hands out an axum `Router`, never a listener** — `opamp-go`'s `Attach`, not its `Start`.
   Listeners, TLS, admission layers and neighbouring routes stay with each surface
   ([ADR-0012](0012-transports-tls-and-the-servers-two-planes.md),
   [ADR-0017](0017-admission-and-authentication.md)); `Settings` carry the size limit, the
   transports served, and whether any path is answered.
4. **The three surfaces keep their behaviour.** The Server's `process()` becomes its handler's
   `on_message`; the Gateway still never speaks in an Agent's name
   ([ADR-0009](0009-client-modes-and-the-gateway.md)); the Supervisor Endpoint stays WebSocket-only
   on any path ([ADR-0015](0015-supervisor-mode-and-its-kinds.md)).

**Out of scope:** the Client's side of the connection
([ADR-0033](0033-an-agents-side-of-opamp-is-one-reusable-client.md)).

## Alternatives considered

- **Leave the three loops where they are** — rejected: one thing written three times, already
  drifted once.
- **A core without I/O first** — not chosen for the server side: its protocol obligations are almost
  all transport, and sequence numbers and `ReportFullState` are the application's, as in `opamp-go`.
- **Own the listener** — rejected: the Server mounts other routes and its own TLS acceptor on the
  same listener, the Gateway terminates TLS per hop.

## Sources / Prior art

- [`opamp-go` server](https://github.com/open-telemetry/opamp-go/tree/main/server) — `OnConnecting`
  returning a `ConnectionResponse`, per-connection `OnConnected`, `OnMessage`, `OnConnectionClose`,
  `OnReadMessageError`, a `Connection` with `Send`, and `Attach` beside `Start`. Read 2026-10-01.
- [axum](https://docs.rs/axum) `Router` and `WebSocketUpgrade`.

## Consequences

- Positive: a Baseline rule on the endpoint is implemented once and reaches all three surfaces; a
  new server surface is a handler, not a fourth loop.
- Negative / trade-offs: the Supervisor Endpoint serves connections concurrently rather than one at
  a time; only one local process connects, so nothing observable changes.
- Negative / trade-offs: a trait with an associated connection type and an outbound side is more
  abstract than three explicit loops; it earns that only while all three use it.

## Enforcement

- `crates/opamp/tests/server_endpoint.rs`: `a_plain_http_exchange_is_answered_by_the_handler`,
  `plain_http_refusals_and_empty_replies_keep_their_shape`, `the_body_rules_are_the_specifications`,
  `a_refused_connection_gets_the_handlers_status_and_headers`,
  `a_websocket_carries_replies_and_pushes`, `an_oversized_websocket_message_is_closed_with_1009`,
  `a_websocket_only_endpoint_on_any_path_serves_no_plain_http` (clauses 1–3).
- The Server's, the Gateway's and the Supervisor Endpoint's own suites run unchanged on the
  endpoint (clause 4).
