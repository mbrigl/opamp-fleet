# Memory per instance

**Kind:** performance, measurement.

**Observed.** Both changes this entry asked for have landed in 0.8.5, unmeasured. Each WebSocket
reads into a 4 KiB buffer instead of `tungstenite`'s 128 KiB (`crates/opamp/src/websocket.rs`,
`websocket_config`). A presence batch is written in pieces of 1 000 Agents (`PRESENCE_PIECE` in
`crates/fleet-server/src/postgres/agents.rs`), so a pool connection's buffers no longer grow to the
largest batch. The figures they replace were measured by the G-20 tests: about 160 to 200 KiB for
each Agent-plane connection, and pool buffers of about 4.5 KiB for each Agent of the most an
instance had held at once (70 MiB at 16 700 Agents, 150 MiB at 33 000). The manual's *Running it*
(`docs/manual/server.md`), the CHANGELOG of 0.8.5 and the comment on `MALLOC_ARENAS`
(`crates/fleet-server/src/main.rs`) now say the new bound was not measured.

**Idea.** A G-20 run measures the memory of an Agent-plane connection and of the pool's buffers
again, and those three places state the figures it gives. No criterion demands it; G-20 passes
without it.

**From.** Tickets 04 and 08 of `fleet-at-scale-costs`, merged into one entry by the human
(2026-11-15); cut to the measurement once both changes landed (2026-11-19).
