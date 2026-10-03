# ADR-0036: The whole OpAMP communication layer lives in the `opamp` crate — wire layer, both sides, their TLS and their listener — built from material the application hands it

- **Status:** 🟡 proposed
- **Date:** 2026-10-03
- **Deciders:** Markus Brigl
- **Applies to:** `crates/opamp/` (its manifest and `[features]`, `build.rs`, `src/`, `LICENSE`, `NOTICE` and `README.md`), every OpAMP connection and listener of `crates/fleet-server/` and `crates/fleet-agent/` (the Server's two planes, the Client's upstream connection and its verification probe, the Gateway's downstream endpoint and upstream pool, the Supervisor Endpoint), `AgentState` in `crates/fleet-agent/src/supervisor/agent.rs`, the Client's `Session` in `crates/fleet-agent/src/transport/`, and the per-feature lint in `.github/workflows/ci.yml` and `README.md`
- **Supersedes:** [ADR-0031](0031-one-opamp-crate-a-publishable-wire-layer-with-client-and-server-features.md), [ADR-0032](0032-one-opamp-server-endpoint-for-every-server-surface.md), [ADR-0033](0033-an-agents-side-of-opamp-is-one-reusable-client.md)

## Context

Three accepted ADRs put the OpAMP communication into `crates/opamp`. ADR-0031 made it one
publishable crate with a `client` and a `server` feature. ADR-0032 gave every server surface one
endpoint around a handler. ADR-0033 gave an Agent one state machine and two drivers around a
session. They are one decision by now, and every change to the crate touches all three.

All three stopped at the same line: the material a connection is built from. ADR-0032 hands out a
router and never a listener. ADR-0033 lets the application build the TLS connector, the HTTP
client and the headers. The result is that each surface still writes its own transport plumbing.

- **Server TLS twice.** `fleet-server/src/tls.rs` and `fleet-agent/src/gateway/mod.rs` each build
  a rustls `ServerConfig` from a certificate, a key and an optional client CA, with the same ALPN
  fix.
- **Listener bounds once.** `fleet-server/src/listen.rs` bounds the header read and the TLS
  handshake ([ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)). The Gateway's
  downstream listener and the Supervisor Endpoint have neither bound.
- **Client connect three times.** The drivers connect, `connection::verify` connects again to
  prove an offer ([ADR-0018](0018-connection-settings-and-server-capabilities.md)), and the
  Gateway's upstream pool connects a third time. Each one builds the WebSocket request, the
  `Authorization` header and the TLS connector by hand.
- **The ring provider and the PEM readers.** Both ends install the provider with the same function.
  The PEM readers sit in `fleet-core` only because both ends need them for TLS.

The maintainer wants the whole communication layer in `opamp`, with its configuration added from
outside. Two rejected alternatives stand against that. ADR-0033 rejected "let the client build TLS
and credentials from settings". [ADR-0034](0034-five-crates-a-publishable-wire-layer-and-toml-configuration.md)
rejected "mutual TLS and the credential in `opamp`". Both called this project's policy. That reason
holds for the policy and not for the mechanism. Which CA to trust, which certificate to present and
which credential to send is policy. Turning those bytes into a rustls configuration, refusing
redirects, marking the header sensitive and bounding a listener is mechanism, and it is the same for
every OpAMP implementation.

## Decision

We will make `crates/opamp` the whole OpAMP communication layer: the wire layer always, and behind
`client` and `server` each side's connection end to end, its TLS and its listener included, built
from values the application hands it and never from a file or a configuration format of its own.

1. **The wire layer, without features.** `proto` (generated from the vendored Baseline schema with
   protox, [ADR-0010](0010-protocol-baseline-and-conformance.md)), `frame`, `endpoint`, `uid`,
   `attributes` and `BASELINE`. They depend on `prost`, `uuid` and `flate2` only.

2. **One publishable crate, versioned by the Baseline.** `publish = true` for `opamp` alone, with
   its metadata and opamp-spec's licence beside the schema. The `MAJOR.MINOR` is the Baseline's,
   the patch number is the crate's own, and a breaking change waits for the next Baseline. The first
   `cargo publish` is the maintainer's decision.

3. **Two features, neither on by default, each linted alone.** `client` adds `tokio`, `tracing`,
   `ring`, `tokio-tungstenite`, `futures-util`, `reqwest`, `rustls` and `webpki-roots`. `server`
   adds `tokio`, `tracing`, `axum`, `axum-server`, `hyper-util` and `rustls`. The lint runs with no
   feature, with `client` and with `server`. docs.rs builds with all features.

4. **`opamp::tls` is the TLS both sides share,** compiled with either feature. It installs the ring
   provider, never a system library ([ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)).
   It reads certificates and a private key from PEM bytes, and an empty result is an error. It
   carries an `Identity`, a certificate chain and its key as PEM. It opens no file. Every rustls
   configuration either side builds speaks TLS 1.3 alone, with the three TLS 1.3 suites of the ring
   provider. The process-wide provider carries those suites and no other, so a TLS stack that
   builds its own configuration, such as `reqwest`'s, cannot negotiate TLS 1.2 either. The
   `wss://` connector is always built here, never left to the WebSocket library, so TLS 1.3 holds
   even in an application that never installed the provider. This is the specification's floor
   (Q-3), not a setting the application can lower.

5. **The server endpoint.** `opamp::server` serves both transports on one path, with the media type,
   gzip, the limit in both directions, framing, the 1009 close and the per-connection loop. The
   application implements `Handler`: `on_connecting`, `on_message`, `outbound`, `on_outbound`,
   `on_unreadable` and `on_closed`. `router` returns an axum `Router`. The application may add
   routes and admission layers to it.

6. **The server listener.** `opamp::server::listen` builds the rustls `ServerConfig` from a
   certificate, a key and a client-certificate rule: none, optional or required against a CA. It
   sets the ALPN protocols. `serve` runs a router on a bound listener with the bounds of ADR-0012:
   a 30-second header read with the timer it needs, and a 10-second TLS handshake. It puts the
   verified peer certificate and the peer address into every request, on every listener it serves.
   A handle shuts listeners down, and one handle may drain several. How long a drain may take is
   the application's to state. `serve` refuses to listen without TLS on any address but the
   loopback literals `127.0.0.1` and `::1` (Q-1). Every OpAMP listener of this project serves through it. Those are
   the Server's two planes, the Gateway's downstream endpoint and the Supervisor Endpoint.

7. **The client state machine and its session.** `opamp::client::protocol` decides which fields a
   report carries and what a reply means, and depends on nothing but `opamp` and the standard
   library. The application implements `Session` for one connection carrying any number of Agents:
   `connected`, `routine`, `owed`, `on_reply`, `after_reply`, `changed`, `exchange_failed`, `stop`
   and `goodbyes`. `AfterReply`, `Ended`, `ReportSink`, `StopSignal` and `Backoff` are public.

8. **The client connection.** The application describes a connection as `opamp::client::Connection`:
   the endpoint, the `Authorization` value, the trust anchors and the identity as PEM, the message
   limit, the heartbeat and the poll interval. `opamp::client` builds everything from that
   description. It picks the transport by scheme. It builds the rustls configuration for `wss://`
   and the HTTP client for `https://`, which follows no redirect, times out after 30 seconds and
   marks the credential sensitive. It refuses `ws://` and `http://` to any host but the loopback
   literals `127.0.0.1` and `::1`, before it connects (Q-1). A host name is never loopback, not
   even `localhost`, because a name can be made to resolve anywhere. The same description drives the
   connection, the one-shot probe that proves offered settings (ADR-0018), and a single WebSocket
   for a caller that drives its own socket, such as the Gateway's upstream pool.

9. **What stays with the application is the policy and its sources.** The application decides
   which files hold the material, which identity is in force, which credential is sent and when it
   rotates. It parses `server.toml` and `supervisor.toml`. It persists and merges connection
   settings (ADR-0018). It runs the CSR flow and the CA, and it decides admission (ADR-0017). Its
   `AgentState` keeps the Client's decisions over one protocol state machine. Its flows after a
   reply run in one order: certificate, connection settings, packages, the self-update restart, the
   Supervisor set ([ADR-0021](0021-the-client-updates-itself.md)).

10. **Behaviour stays, with these deliberate exceptions.** The suites of the three surfaces and of
    the Client run unchanged on the moved code. The exceptions follow from one listener and one
    connect serving every surface:
    - The Gateway's downstream listener and the Supervisor Endpoint gain the header-read and
      handshake bounds that only the Server had.
    - A plaintext Gateway drains for a bounded time on shutdown, as a TLS one already did.
    - The Operator plane and the plain listeners carry the peer address and the peer certificate
      into their requests too. Nothing there reads them.
    - The probe of offered settings applies the message limit and marks the credential sensitive,
      as the long-running connection does.
    - TLS 1.2 is no longer offered or accepted by either end, and plaintext off the loopback is
      refused rather than warned about (clauses 4, 6 and 8).

**Out of scope:** the CSR flow and the Server's CA, which stay in the application with ADR-0017;
finer features, such as one per transport; a release routine for the crate; and where the PEM
material comes from on disk.

## Alternatives considered

- **Keep the material with the application, as ADR-0033 decided.** Rejected: three surfaces write
  the same connect, and two listeners lack the bounds the third has. The policy argument is met by
  clause 9, which keeps every decision about the material with the application.
- **Let `opamp` read the files and the TOML sections itself.** Rejected: a published crate would
  impose this project's file layout and configuration format on every user, and a test would need
  files where it now passes bytes.
- **Hand `reqwest` a prebuilt rustls configuration**, so that one configuration serves both
  transports. Not chosen: `tls_backend_preconfigured` is documented without semver stability and
  breaks silently on a version mismatch. The PEM bytes feed `reqwest`'s own builder instead.
- **Move the CSR flow into `opamp` too.** Not chosen now: OpAMP carries the CSR, but generating a
  key and signing a request is certificate policy, and it would add `rcgen` to the published crate.
  It can follow once a second user wants it.
- **Separate crates per side, or `client` and `server` on by default.** Rejected, as in ADR-0031:
  the sides share a version and break together, and a types-only user would pull both dependency
  sets.
- **Depend on an existing crate.** None fits: `otel-opamp-rs` follows a 2023 draft and is dormant,
  and `newrelic-opamp-rs` is licensed for New Relic's service only.

## Sources / Prior art

- [`opamp-go`](https://github.com/open-telemetry/opamp-go): `server.Start` beside `server.Attach`,
  where `StartSettings` carry the listen address and a `tls.Config`; `client.StartSettings` carry
  the endpoint, the header and a `tls.Config`, and the client builds its HTTP and WebSocket
  connections from them. Read 2026-10-01.
- [`reqwest::ClientBuilder`](https://docs.rs/reqwest/0.13/reqwest/struct.ClientBuilder.html):
  `tls_certs_only`, `identity` and `tls_backend_preconfigured`, with the stability warning on the
  last. Read 2026-10-03 in the vendored 0.13.5 source.
- [`axum-server`](https://docs.rs/axum-server/0.7) `RustlsAcceptor::handshake_timeout` and
  [`hyper-util`](https://docs.rs/hyper-util/0.1) `TokioTimer`, as ADR-0012 cites them.
- [`rustls-pki-types`](https://docs.rs/rustls-pki-types) `PemObject`, the PEM reader rustls itself
  uses.
- The sources of ADR-0031, ADR-0032 and ADR-0033 stand: hyper and tonic features, the sans-IO
  pattern, `quinn` over `quinn-proto`, and the Cargo book on publishing and feature unification.

## Consequences

- Positive: a Rust OpAMP implementer gets a working client or server, TLS included, from one
  dependency and a few values. This project's three surfaces share the connect, the listener and
  the bounds, so a fix lands once.
- Positive: `fleet-core` no longer links `rustls`; it holds the version and the platform aliases.
- Negative / trade-offs: the published API grows by `Connection`, the listener and the TLS types.
  `axum-server`'s `Handle` and `tokio-tungstenite`'s socket become part of it, so a breaking bump
  of either waits for the next Baseline.
- Negative / trade-offs: the `server` feature pulls `axum-server` and `hyper-util`, and `client`
  pulls `webpki-roots`. A user who brings their own listener pays for one anyway.
- Negative / trade-offs: the listener attaches the per-connection extensions with `Router::layer`,
  which rebuilds the route table once per accepted connection. It costs more on a listener with
  many routes, such as the Operator plane, and is unmeasured.
- Follow-ups: the CSR flow in `opamp` if a second user wants it; finer features if a user asks for
  one transport alone.

## Enforcement

- `crates/opamp/src/lib.rs` `the_crate_version_is_the_baselines` fails when the crate's
  `MAJOR.MINOR` and `opamp::BASELINE` disagree (clause 2).
- `cargo publish --dry-run -p opamp` builds from the packaged sources outside any checkout, and
  [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) lints `opamp` with no feature,
  with `client` and with `server` (clauses 1–3).
- `crates/opamp/src/tls.rs` tests that an empty or foreign PEM is an error and a bundle keeps its
  order (clause 4).
- `crates/opamp/tests/server_endpoint.rs` drives the endpoint over both transports (clause 5).
- `crates/opamp/tests/server_listen.rs` proves the header-read bound, the required and the optional
  client certificate, and the peer certificate in the request (clause 6).
- `crates/opamp/src/client/protocol.rs`, `ws.rs`, `http.rs` and `backoff.rs` keep the client-side
  MUSTs under test (clause 7). `crates/opamp/src/client/connection.rs` tests the scheme choice and
  the cleartext rule, and `crates/opamp/tests/client_connection.rs` the probe on both transports,
  the redirect refusal and a run (clause 8).
- The structural test `crates/fleet-core/tests/dependency_direction.rs` lists every module. The
  application suites run unchanged on the moved code (clauses 9, 10).
