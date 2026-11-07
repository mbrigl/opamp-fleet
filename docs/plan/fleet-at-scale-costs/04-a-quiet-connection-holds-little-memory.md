# Ticket: A quiet connection holds little memory

## What to build

An instance holds about 160 to 200 KiB for each Agent-plane connection, most of it while nothing
is sent: measured by the G-20 tests at 3 400 and 6 700 connections an instance (700 to 890 and
1 300 to 1 420 MiB). A fleet of 100 000 Agents on direct connections across three instances would
take about 6 GiB in each. The largest part is likely the WebSocket read buffer, which
`tokio-tungstenite` allocates at 128 KiB for every connection at its default, beside the TLS
buffers; this was not profiled.

Each connection holds what its traffic needs: the buffers of a quiet connection are sized to the
messages an Agent sends, and grow for a large one only while it is read. The same holds for the
Client's connection and the Gateway's pool, which use the same `opamp` endpoint and driver.

It does not block G-20: the load test carries ten Agents on each connection, so that one machine
holds the simulated fleet, and meets its target so.

## Acceptance criteria

- [ ] Given an instance with 10 000 Agent-plane connections and no message in flight, when its
  memory is read, then it holds at most 64 KiB for each connection beyond what it holds with none
- [ ] Given a message up to `max_message_size`, when it arrives on a connection, then it is read
  and handled as before
- [ ] Given the integration suite and the G-20 failover test, when they run, then they pass
  unchanged

## Read first

- `crates/opamp/src/server.rs`, the `WebSocketConfig` of the endpoint, and
  `crates/opamp/src/client/ws.rs`
- `crates/fleet-server/tests/g20.rs`, the memory the tests print
- ADR-0008 clause 2 for an interface change of `opamp`

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
