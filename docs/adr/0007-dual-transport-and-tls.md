# ADR-0007: Both OpAMP transports on both ends — plain HTTP(S) polling and WebSocket on one endpoint, TLS via rustls

- **Status:** 🟢 accepted
- **Date:** 2026-07-22
- **Deciders:** Markus Brigl

## Context

The Baseline defines two transports and is explicit about who supports what: *"Server
implementations SHOULD accept both plain HTTP connections and WebSocket connections. OpAMP Client
implementations may choose to support either."* The specification's strategy — implement the
protocol *as completely as the protocol allows* — turns that SHOULD/MAY into a project obligation on
**both** ends: a Server accepting only one transport fails half the third-party clients, and a
Client speaking only one transport cannot be validated against both faces of its own Server.

What each transport entails at the Baseline:

- **Plain HTTP** — the Client POSTs a protobuf-encoded `AgentToServer` to the endpoint and receives
  a `ServerToAgent` in the response; it polls (default every 30 seconds) and MUST set
  `Content-Type: application/x-protobuf`. The Server MUST honour `Content-Encoding: gzip` request
  bodies. Server-initiated messages wait for the next poll.
- **WebSocket** — one persistent connection, either side sends at will; each WebSocket message is a
  varint header (`0`) plus the protobuf body (framing codec: ADR-0006). This is the transport that
  gives the control loop its "within seconds" property and the one the multiplexing provision of
  [ADR-0003](0003-client-modes-and-connection-multiplexing.md) is stated for.
- **Transport detection** — the endpoint is one path (`/v1/opamp`, default port 4320): a request
  carrying the protobuf `Content-Type` is the plain-HTTP transport; a WebSocket upgrade request is
  the other. The Baseline itself describes the `Content-Type` header as how the Server tells them
  apart.

TLS is part of the transport decision, not a later add-on: the specification (goal 17) requires
Client–Server traffic to be TLS-protected, and both `wss://` and `https://` are the protocol's
normal deployment shape. The Dev Container constraint ([ADR-0002](0002-dev-container-runtime.md))
weighs here: OpenSSL-linked stacks need system headers; `aws-lc-rs`-backed rustls needs cmake.
Neither is present.

## Decision

We will implement **both transports on both ends** from the start: the Server accepts plain-HTTP
POST and WebSocket upgrades **on the same endpoint** (`/v1/opamp`, port 4320 by default),
distinguished per request exactly as the Baseline describes; the Client implements both and selects
by the scheme of its configured endpoint URL (`ws(s)://` → WebSocket, `http(s)://` → polling, poll
interval configurable, default 30 s). TLS is **rustls with the `ring` provider** on both ends —
`axum-server` for the Server's optional TLS listener (enabled when a certificate and key are
configured), `tokio-tungstenite` (rustls, webpki roots) for `wss://`, and `reqwest` (rustls, gzip)
for `https://` polling — with an optional configured CA file so self-signed deployments work; no
OpenSSL anywhere.

Bound by this decision:

- The Server honours `Content-Encoding: gzip` on plain-HTTP request bodies (a Baseline MUST) and
  enforces a receive size limit on both transports.
- The WebSocket default: the Client uses WebSocket unless configured otherwise, because it is the
  only transport that delivers Server-initiated changes without polling latency — the control
  loop's "within seconds" vision.
- Reconnection with backoff and a fresh full status report after reconnect are part of the
  transport layer on the Client; the Server, per [ADR-0003](0003-client-modes-and-connection-multiplexing.md),
  keys no state on the connection either way.
- Authentication (tokens, mutual TLS, `ConnectionSettings` rotation) stays out of scope here — it
  is the separate decision ADR-0003 already flagged. This ADR only ensures the pipe it will ride on
  is encrypted.

## Alternatives considered

- **WebSocket only first, HTTP later** — rejected. The Baseline says Servers SHOULD accept both;
  deferring plain HTTP would put a deviation in `CONFORMANCE.md` on day one for a transport whose
  request/response shape is the *simpler* of the two, and third-party HTTP-polling clients would be
  locked out.
- **Plain HTTP only first** — rejected. It caps the control loop at poll latency, and the gateway
  multiplexing this project is architected around (ADR-0003) is specified in WebSocket terms.
- **Separate endpoints or ports per transport** — rejected. The Baseline's default is one path and
  one port with per-request detection; splitting them would be a deviation with no gain.
- **`native-tls`/OpenSSL** — rejected. System OpenSSL headers on Linux plus SChannel/SecureTransport
  variance on the Windows/macOS client builds, versus one pure-Rust TLS stack identical on all
  platforms.
- **rustls with the default `aws-lc-rs` provider** — rejected for now. It requires cmake at build
  time, which the Dev Container deliberately lacks; the `ring` provider builds with the existing
  toolchain. Revisit if FIPS or aws-lc becomes a requirement.
- **A hand-rolled HTTP client (raw hyper) instead of `reqwest`** — rejected. Polling with gzip,
  TLS, redirects, and timeouts is exactly reqwest's job; hand-rolling it saves a dependency but
  re-implements well-tested behaviour.

## Sources / Prior art

- [OpAMP specification § Transport, § Plain HTTP Transport, § WebSocket Transport (`v0.18.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.18.0/specification.md)
  — dual-transport SHOULD, `Content-Type: application/x-protobuf` and its use for transport
  detection, gzip MUST, 30 s default poll, port 4320 and `/v1/opamp`, varint framing.
- [`CONFORMANCE.md`](../CONFORMANCE.md) — the behaviour matrix rows this ADR implements, and the
  post-Baseline upstream change on transport size limits (64 MiB recommendation) that informed
  enforcing a receive limit now.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — the reference implementation ships
  both transports behind one client interface (`NewWebSocket` / `NewHTTP`); behavioural oracle per
  the specification.
- [`tokio-tungstenite`](https://crates.io/crates/tokio-tungstenite),
  [`reqwest`](https://crates.io/crates/reqwest), [`axum-server`](https://crates.io/crates/axum-server),
  [`rustls`](https://crates.io/crates/rustls) — the concrete stack; rustls `ring` provider avoids
  cmake/aws-lc builds (verified against the Dev Container's package set, 2026-07-22).
- Prior work in this repository's history (`6fba83b` lineage) ran this exact TLS composition
  (rustls-`ring`, `axum-server` `tls-rustls-no-provider`, tungstenite with webpki roots) inside the
  same Dev Container.

## Consequences

- Positive: full transport conformance from the first release — several `planned` rows in
  `CONFORMANCE.md` flip to `implemented` honestly, and any third-party client or server can pair
  with this project regardless of its transport choice.
- Positive: one TLS stack, identical on Linux, macOS, and Windows; the client cross-builds need no
  system TLS libraries.
- Negative / trade-offs: two transports mean two code paths to test on each end (plus
  reconnect/backoff behaviour). Accepted — the alternative is a recorded deviation and a locked-out
  client class; shared message-handling code keeps the fork small.
- Negative / trade-offs: `reqwest` is a sizeable dependency for a poll loop. Accepted for its
  battle-tested TLS/gzip/timeout handling; it is client-side only.
- Follow-ups: authentication and credential rotation (`ConnectionSettings`) need their own ADR
  (flagged in ADR-0003); response compression on plain HTTP (a SHOULD, not a MUST) and the exact
  size-limit/`413`/`1009` behaviour arriving with the next Baseline bump are revisited when the
  Baseline moves.
