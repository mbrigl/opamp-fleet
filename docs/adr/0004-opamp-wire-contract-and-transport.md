# ADR-0004: OpAMP wire contract from vendored proto, plain-HTTP transport first

- **Status:** 🟢 accepted
- **Date:** 2026-07-20
- **Deciders:** Maintainer

## Context

The specification's first strategy is "Speak the protocol, do not reinvent it": the wire contract **is**
the OpAMP specification, implemented faithfully on both ends. OpAMP defines its messages as Protobuf —
`AgentToServer` sent by the Client, `ServerToAgent` returned by the Server — and allows a Client to use
**either** plain HTTP (synchronous, half-duplex: the Client POSTs `AgentToServer`, the Server replies
`ServerToAgent`, `Content-Type: application/x-protobuf`) **or** WebSocket (full-duplex, enabling instant
Server-to-Agent push). The specification also notes a concrete gap: there is **no OpAMP Server in Rust**,
so this project must build the Server rather than adopt one.

Two decisions must be fixed before code: (1) **where the Rust wire types come from**, and (2) **which
transport** the first version uses. Both are protocol/data-format choices and both constrain future work,
so per AGENTS.md §3 they need an ADR. Constraints: the container ships no `protoc` (ADR-0002), and Goal #1
("the loop closes") only requires that a config change reaches a connected Agent "within seconds" — which
short-interval HTTP polling already satisfies. The specification explicitly sequences work: "close the
loop before widening it."

## Decision

We will treat the official `open-telemetry/opamp-spec` Protobuf as the source of truth: **vendor**
`opamp.proto` and its `anyvalue.proto` import into `crates/opamp/proto/` and **generate** Rust types at
build time with `prost`, driven by **`protox`** (a pure-Rust Protobuf compiler) so **no system `protoc`
is required**. For the first version the transport is **plain HTTP** — the Server exposes
`POST /v1/opamp` accepting/returning `application/x-protobuf`, and the Supervisor Host is an HTTP client
that POSTs on a short poll interval. **WebSocket transport is deferred** to a future ADR. `opamp-go`
remains the behavioural oracle the Rust code is checked against.

## Alternatives considered

- **Depend on a third-party Rust OpAMP crate** (`newrelic-opamp-rs`, `otel-opamp-rs`) — they cover only
  the Client, are early-stage/pre-1.0, and there is no Rust Server to pair with them; adopting one would
  contradict "own both ends in Rust" and leave the Server unbuilt. Vendoring the proto keeps one owned,
  auditable contract shared by both ends (the `opamp` crate from ADR-0003).
- **System `protoc` via a Dev Container Feature** — works, but adds a system dependency to the container;
  `protox` compiles the same `.proto` in pure Rust, keeping the toolchain minimal and the build
  hermetic.
- **Fetch the proto at build time instead of vendoring** — makes the build depend on network and on an
  upstream tag moving; vendoring pins the exact contract in-tree and keeps builds reproducible and
  reviewable.
- **WebSocket transport first** — gives instant Server push, but is more moving parts (framing, a header
  byte, connection lifecycle) than Goal #1 needs; HTTP polling reaches an Agent within seconds and is the
  simpler thing that closes the loop. Server push is a real need, taken up when the loop holds.

## Sources / Prior art

- OpAMP specification (message pair, transports, `application/x-protobuf`, gzip, `OpAMP-Instance-UID`
  header): <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md> and
  <https://opentelemetry.io/docs/specs/opamp/>.
- OpAMP proto definitions: <https://github.com/open-telemetry/opamp-spec/tree/main/proto>.
- `opamp-go` reference implementation: <https://github.com/open-telemetry/opamp-go>.
- `prost` (Protobuf → Rust): <https://github.com/tokio-rs/prost>; `protox` (pure-Rust compiler):
  <https://github.com/andrewhickman/protox>.

## Consequences

- Positive: one owned, in-tree wire contract shared by Server and Supervisor Host; hermetic build with no
  `protoc`; the simplest transport that closes the loop; conformance checkable against `opamp-go`.
- Negative / trade-offs: HTTP polling adds latency up to one poll interval and cannot push instantly;
  vendored proto must be refreshed deliberately when upstream changes (a visible, reviewable diff).
- Follow-ups: a future ADR adds **WebSocket transport** for Server-initiated push when needed; gzip
  request compression and the `OpAMP-Instance-UID` header are HTTP niceties that can be added within this
  decision without a new ADR.
