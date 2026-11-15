# Memory per instance

**Kind:** performance.

**Observed.** Two costs of a Server instance's memory grow with the Agents it holds:

- **Each connection.** An instance holds about 160 to 200 KiB for each Agent-plane connection,
  most of it while nothing is sent. The G-20 tests measured 700 to 890 MiB at 3 400 connections an
  instance and 1 300 to 1 420 MiB at 6 700. A fleet of 100 000 Agents on direct connections across
  three instances would take about 6 GiB in each. Both ends build their WebSocket configuration from
  `WebSocketConfig::default()` (`crates/opamp/src/server.rs`, near line 350;
  `crates/opamp/src/client/ws.rs`, near line 426). The largest part is likely the read buffer,
  which `tokio-tungstenite` allocates at 128 KiB for every connection at its default; that was not
  profiled. The Client's connection and the Gateway's pool use the same `opamp` endpoint and driver.
  These figures are carried over from the old ticket and were not measured again.
- **The presence batch.** An instance writes the presence of every Agent it holds in one
  transaction of two statements, each binding arrays of every Agent in the batch
  (`crates/fleet-server/src/postgres/agents.rs`, near lines 603 to 655). Each pool connection keeps
  its buffers at the size of the largest batch it carried. `docs/manual/server.md` (line 75) states
  the bound: about 70 MiB at 16 700 Agents an instance and 150 MiB at 33 000.

**Idea.** The buffers of a quiet connection are sized to the messages an Agent sends, on the Server,
the Client and the Gateway's pool alike. A presence batch is written in pieces of about 1 000
Agents, in the same transaction and the same order of `instance_uid`. Once the pieces land, the
manual's *Running it*, the CHANGELOG and the comment on `MALLOC_ARENAS` state the bound they give.
No criterion demands either; G-20 passes without them.

**From.** Tickets 04 and 08 of `fleet-at-scale-costs`, merged into one entry by the human
(2026-11-15).
