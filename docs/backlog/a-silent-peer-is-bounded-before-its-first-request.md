# A silent peer is bounded before its first request

**Kind:** security, idea.

**Observed.** ADR-0012 bounds connection setup: the TLS handshake by `TLS_HANDSHAKE_TIMEOUT`
(clause 16), the request head by `header_read_timeout` (clause 15), the sockets by the connection
cap (clause 19). Between the handshake and the head lies hyper-util's protocol detection: its auto
builder reads the first bytes to tell HTTP/1 from HTTP/2 (`ReadVersion`,
`hyper-util` 0.1.21, `server/conn/auto/mod.rs` near lines 305 to 360), with no timer of its own,
and `header_read_timeout` starts only after it. A peer that completes the TCP connection, or the TLS
handshake, and then sends nothing holds a task and a slot of `max_connections` until it closes or
the listener shuts down. Enough of them fill the cap. Read from the source, not reproduced at
runtime.

**Idea.** axum-server 0.9's `Server::first_request_timeout` closes a connection that sends no
request in time, the detection read included; `crates/opamp/src/server/listen.rs` sets it beside
`header_read_timeout` in `bounded`, with a test of a peer that falls silent after the handshake.
Its `TlsInfo`, which carries the client certificates in each request's extensions, could also
replace the listener's own reading of the peer certificate, if that turns out simpler.

**From.** The review of the axum-server 0.9 bump (2026-11-16), which deleted the entry that named
`first_request_timeout`.
