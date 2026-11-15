# The listener takes up axum-server 0.9

**Kind:** dependency, structure.

**Observed.** Dependabot's #189 bumps `axum-server` from 0.8 to 0.9.2, and the workspace no longer
builds: `crates/opamp/src/server/listen.rs` (near line 659) hands the service a TLS acceptor
returns to `with_connection`, which attaches the connection's extensions (`PeerCertificate`,
`ConnectInfo`, `Pace`) with `Router::layer`. Since 0.9.0 a TLS acceptor returns a
`tls::TlsService` wrapper instead of the `Router`, and the wrapper does not hand out what it wraps.
That is the path admission by client certificate reads.

The release also brings what the listener could use: `Server::first_request_timeout`, the
cancellation of handshakes still running at shutdown, and `TlsInfo` with the client certificates
in the request extensions. Its `tls-rustls` feature now enables TLS 1.2 and its default provider
is aws-lc-rs; the workspace asks for `tls-rustls-no-provider`, builds its own rustls configuration
on ring with TLS 1.3 alone, and `deny.toml` refuses aws-lc, so neither change reaches it as long as
the feature stays.

**Idea.** The Agent plane's acceptor wraps `TlsService<Router>` in a service of its own that inserts
the connection's extensions into each request, or reads the client certificate from `TlsInfo`, and
the tests of admission (`crates/fleet-server/tests/mutual_tls.rs`, `crates/opamp/tests/`) hold it.
Then #189's bump of `axum-server`, with `fuzz/Cargo.lock`, lands with it.

**Effort.** M.

**From.** The human split #189 on 2026-11-15: `sevenz-rust2` 0.25 went into 0.8.3, `axum-server`
0.9 waits here.
