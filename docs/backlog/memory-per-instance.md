# Memory per instance

**Kind:** performance.

**Observed.** An instance writes the presence of every Agent it holds in one transaction of two
statements, each binding arrays of every Agent in the batch
(`crates/fleet-server/src/postgres/agents.rs`, near lines 603 to 655). Each pool connection keeps
its buffers at the size of the largest batch it carried. `docs/manual/server.md` (line 75) states
the bound: about 70 MiB at 16 700 Agents an instance and 150 MiB at 33 000.

The memory of each Agent-plane connection was the other half of this entry. The G-20 tests
measured about 160 to 200 KiB for each, most of it while nothing is sent, carried over from the
old ticket and not measured again. Both ends now read into a 4 KiB WebSocket buffer instead of
`tungstenite`'s 128 KiB (`crates/opamp/src/frame.rs`, `websocket_config`); what that saves an
instance was not measured.

**Idea.** A presence batch is written in pieces of about 1 000 Agents, in the same transaction and
the same order of `instance_uid`. Once the pieces land and the memory of a connection is measured
again, the manual's *Running it*, the CHANGELOG and the comment on `MALLOC_ARENAS` state the bound
they give. No criterion demands it; G-20 passes without it.

**From.** Tickets 04 and 08 of `fleet-at-scale-costs`, merged into one entry by the human
(2026-11-15).
