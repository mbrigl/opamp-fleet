# ADR-0006: Implement the OpAMP server side in Rust, from the specification

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The [specification](../SPECIFICATION.md) makes the Server the control plane and requires it to close
the loop against real agents (Goal 1, Goal 12). To do that it must speak the OpAMP **server** side of
the wire protocol. Two facts shape how:

- **There is no OpAMP *server* SDK in Rust.** The one Rust OpAMP crate (`otel-opamp-rs`) is a pre-0.1
  *client* library. So the server side must be implemented here, from the specification's own schema.
- **OpAMP's WebSocket transport is not "just Protobuf."** The spec frames every message as a *header*
  (a Varint-encoded unsigned 64-bit integer, `0` in this version) followed by the Protobuf message; it
  requires `sequence_num` gap detection with a `ReportFullState` recovery flag, and a message size
  limit. A decoder that assumed a bare payload would fail silently against a real agent.

Owning the protocol layer is therefore a real obligation, not a translation. This ADR records adopting
it, and the dependencies it pulls in.

## Decision

We will implement the **OpAMP server side in Rust, from the vendored specification schema**, in the
`opamp-server` crate over the shared `opamp-proto` crate
([ADR-0005](0005-cargo-workspace-layout.md)).

- **Runtime & transport:** `tokio` with `axum` (WebSocket). The OpAMP agent endpoint
  (`/v1/opamp`, default `:4320`) and the human/UI surface (`:4321`,
  [ADR-0007](0007-rest-api-and-fleet-ui.md)) are two listeners, so agent-facing and human-facing ports
  are exposed and firewalled independently.
- **Protobuf:** `prost`, generated at build time by `build.rs` from a **vendored, pinned** copy of the
  specification's `proto/opamp/v1/*.proto`, so the generated types cannot drift from the schema
  silently. Generation needs `protoc` ([ADR-0004](0004-rust-toolchain-dev-container.md)).
- **Protocol obligations we take on explicitly:** the varint frame header; per-agent state with the
  **delta rule** (a field's absence means "unchanged", never "gone"); `sequence_num` gap detection with
  `ReportFullState` recovery; and a message size limit.
- **The control loop is one comparison:** each `AgentToServer` reports the hash of the configuration
  the agent last received; the Server includes the remote configuration in its reply only when that
  hash differs from the one it distributes. The distributed configuration is a single YAML file on
  disk, and writing that file is the only way to reconfigure the fleet (the poll loop distributes it).
- **Scope now (initial server):** a **plain-`ws`, unauthenticated, single-transport** server —
  enough to see the dev sidecars and their status. Deliberately **out of this ADR's scope**, each to
  be added under its own ADR as the corresponding specification goal is implemented: TLS + shared-token
  authentication (spec's authenticated-transport goal), OpAMP package delivery (software-updates goal),
  subset targeting (targeting goal), and the OpAMP plain-HTTP transport.

## Alternatives considered

- **Depend on `otel-opamp-rs` for the protocol layer.** It is a pre-0.1 *client* library; it does not
  implement the server side, and coupling to a pre-0.1 API for types we can generate from the schema is
  a worse trade. Rejected.
- **Commit the generated Rust protobuf code.** Removes `protoc` from the build, but generated code in
  the tree drifts from the `.proto` silently. Rejected: generate at build time from the vendored schema.
- **Vendor `protoc` via `protoc-bin-vendored`.** Ships prebuilt binaries inside a crate, a supply-chain
  surface. Rejected in favour of the distribution's `protoc`
  ([ADR-0004](0004-rust-toolchain-dev-container.md)).
- **A raw `hyper`/`tungstenite` stack instead of `axum`.** More control, more boilerplate for routing
  and the second (UI/API) listener. `axum` (on `tokio`/`hyper`) is the smaller path to two listeners.
- **Build the whole protocol surface at once (TLS, packages, HTTP transport).** Rejected for now as
  against "close the loop before widening it": the initial server proves fleet visibility; each further
  capability lands with the ADR for its specification goal.

## Sources / Prior art

- OpAMP specification — WebSocket framing (varint header), `sequence_num`/`ReportFullState`, size
  limits: <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- The vendored schema: <https://github.com/open-telemetry/opamp-spec/tree/main/proto/opamp/v1>.
- `otel-opamp-rs` — the only Rust OpAMP crate, client-side, pre-0.1: <https://lib.rs/crates/otel-opamp-rs>.
- `prost-build` requires `protoc`: <https://crates.io/crates/prost-build>. `axum` WebSockets:
  <https://docs.rs/axum/latest/axum/extract/ws/index.html>.
- `opamp-go`, the reference to compare behaviour against (not depended on):
  <https://github.com/open-telemetry/opamp-go>.

## Consequences

- Positive: one language across the loop; we control the protocol layer. The server closes the control
  loop against the real dev sidecars ([ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md)).
- Negative / trade-offs: **we own the protocol layer, and its bugs are ours** — there is no reference
  server to fall back on, and OpAMP is still evolving. Correctness is judged by whether a *real* agent
  works against us, not by our own tests agreeing with themselves.
- Follow-ups: the REST API + UI over the fleet state ([ADR-0007](0007-rest-api-and-fleet-ui.md)); and,
  each under its own ADR, authenticated transport, package delivery, subset targeting, and the HTTP
  transport.
