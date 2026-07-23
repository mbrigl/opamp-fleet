# ADR-0005: Three-crate Cargo workspace; tokio runtime; axum serves OpAMP, REST API, and the bundled UI on one port

- **Status:** 🟢 accepted
- **Date:** 2026-07-22
- **Deciders:** Markus Brigl

## Context

The [specification](../SPECIFICATION.md) fixes the language (both ends in Rust) and the deployables:
one **Server** (Linux only, API-first, with a rudimentary bundled UI) and one **Client** binary
covering all Client Modes ([ADR-0003](0003-client-modes-and-connection-multiplexing.md)). The README
already names the code layout — one Cargo workspace with three crates: `opamp` (shared library),
`server`, and `client` — and pins the toolchain to Rust stable via `rust-toolchain.toml`. What is
not yet decided is the concrete runtime and HTTP stack, and how the Server's three surfaces — the
OpAMP endpoint, the REST API, and the bundled UI — are exposed.

Forces:

- **Both transports on one endpoint.** The protocol serves plain HTTP and WebSocket on the same
  path (`/v1/opamp`, port 4320 by default), distinguished per request (see
  [`CONFORMANCE.md`](../CONFORMANCE.md)). The HTTP framework must therefore do protobuf request
  bodies and WebSocket upgrades on one route set.
- **Everything is async I/O.** The Server multiplexes many long-lived connections; the Client holds
  a Connection Pool and supervises processes. Rust async requires choosing an executor; the de-facto
  standard is `tokio`, and every serious Rust HTTP stack builds on it.
- **The UI must not grow a toolchain.** The specification bounds the UI to "rudimentary" and expects
  real UIs to live outside the project. A frontend build chain (npm, bundlers) would be a second
  toolchain to install, cache, and secure in CI for a UI that is explicitly not the product.
- **The Dev Container is deliberately lean** ([ADR-0002](0002-dev-container-runtime.md)): base
  Debian plus the Rust feature. Choices that demand extra system packages (OpenSSL headers, cmake,
  node) carry a real cost here.

## Decision

We will build one Cargo workspace with exactly three crates — `opamp` (shared protocol library),
`server`, and `client` — on the `tokio` async runtime, use **axum** as the Server's HTTP framework
serving the OpAMP endpoint, the REST API, and the bundled UI **on a single listening port**, and ship
the UI as **static assets embedded into the server binary** (`include_str!`), written in plain
HTML/CSS/JS with no frontend toolchain.

Supporting choices bound by this ADR:

- **Runtime:** `tokio` (multi-threaded), `tracing` + `tracing-subscriber` for structured logging.
- **Server HTTP:** `axum` with its `ws` feature — WebSocket upgrades and plain routes coexist on one
  router; `tower-http` middleware is available when needed.
- **Serialization:** `serde` for the REST API's JSON and for configuration files.
- **One port:** `/v1/opamp` (OpAMP), `/api/*` (REST), `/` (UI) share one listener, so a deployment
  exposes one address and TLS terminates once. The REST API is the contract; the UI is a client of
  that API and nothing more.
- **CI enforces the Definition of Done on this stack:** `cargo fmt --check`, `cargo clippy`
  (warnings denied), `cargo build`, `cargo test` on Linux; the Client is additionally built on
  Windows and macOS (the specification ships it on all three platforms, the Server on Linux only).

## Alternatives considered

- **`actix-web`** — mature and fast, but it brings its own runtime flavour and actor heritage;
  axum is plain tokio + tower, matches `tokio-tungstenite` on the client side, and is the stack the
  OpenTelemetry Rust ecosystem gravitates to. No capability we need favours actix.
- **Raw `hyper`** — maximal control, but we would hand-write routing, upgrades, and extractors that
  axum provides as a thin layer over hyper anyway. More code for no protocol gain.
- **Separate ports for OpAMP / API / UI** — cleaner firewalling in some deployments, but it triples
  the TLS and configuration surface and buys nothing now; a reverse proxy can still split paths. Can
  be revisited if a deployment need appears.
- **A frontend framework + bundler for the UI** — rejected. The specification caps the UI at
  rudimentary; a node toolchain in the Dev Container and CI is a large standing cost for a page that
  renders one fleet table and one config editor. Static embedded assets keep the server a single
  self-contained binary.
- **More crates (e.g. separate `fleet`, `ui`, per-plugin crates)** — premature. The hexagonal
  seams from the specification live as modules first; a module becomes a crate when a concrete need
  (compile time, reuse) appears, which is a reversible refactor.

## Sources / Prior art

- [`axum`](https://docs.rs/axum/0.8) — router, extractors, and built-in WebSocket upgrade support
  (`ws` feature); maintained by the tokio project. Version 0.8 current on crates.io (checked
  2026-07-22).
- [`tokio`](https://tokio.rs/) — the de-facto standard async runtime for network services in Rust.
- Prior work in this repository's history (branch lineage at `719d49b` and `6fba83b`): an axum
  Server serving `/v1/opamp`, `/api/*`, and an embedded theme-aware `index.html` on one port proved
  this exact composition end to end, including the Bindplane-style fleet table UI this project's
  bundled UI follows.
- [Bindplane](https://bindplane.com/) — the look-and-feel reference for the rudimentary UI (fleet
  table, status chips, config drawer); a design reference, not a dependency.
- [ADR-0002](0002-dev-container-runtime.md) — the lean-container constraint that penalizes stacks
  needing system packages or a second toolchain.

## Consequences

- Positive: one workspace, one lockfile, one pinned toolchain; the Server is a single static-ish
  binary embedding its UI; client and server share the `opamp` crate so the two ends cannot drift
  apart on the wire types.
- Positive: axum's `ws` feature makes the dual-transport endpoint (a follow-on ADR decides the
  transport details) a routing concern rather than an architectural one.
- Negative / trade-offs: committing to tokio/axum is a deep dependency commitment — reversing it
  would touch every I/O boundary. Accepted: this is the mainstream Rust stack with the largest
  maintenance surface behind it.
- Negative / trade-offs: an embedded UI means a UI change requires a server rebuild. Accepted — the
  UI is rudimentary by charter, and this keeps deployment to one artifact.
- Follow-ups: the OpAMP transport details (plain HTTP + WebSocket semantics, TLS) and the
  configuration file format are decided in their own ADRs; an OpenAPI description for the REST API
  (specification goal 5) needs a decision on how it is authored or generated once the API grows past
  its seed.
