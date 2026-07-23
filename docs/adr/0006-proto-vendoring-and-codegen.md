# ADR-0006: Vendor the Baseline's protobuf schema and compile it with prost via protox (no system protoc)

- **Status:** 🟢 accepted
- **Date:** 2026-07-22
- **Deciders:** Markus Brigl

## Context

[ADR-0004](0004-protocol-baseline-and-conformance-tracking.md) pinned the Protocol Baseline
(`v0.18.0`) and left one follow-up explicitly open: *how the protobuf definitions are obtained and
compiled — vendored copy versus fetch-at-build, and which Rust protobuf toolchain generates them —
needs its own ADR before the first protocol code lands.* This is that ADR.

Forces:

- **The wire contract must be reproducible and reviewable.** The generated Rust types are the wire
  contract of both ends; if the schema can change without a diff in this repository, conformance
  claims in [`CONFORMANCE.md`](../CONFORMANCE.md) are unverifiable.
- **Builds must work offline.** [ADR-0004](0004-protocol-baseline-and-conformance-tracking.md)
  already accepts that network-dependent checks degrade quietly; the *build* must not depend on the
  network at all.
- **The proto path must live in exactly one place.** Upstream has already relocated the proto files
  on `main` (`proto/` → `proto/opamp/v1/`); [`CONFORMANCE.md`](../CONFORMANCE.md) requires the file
  path and include root to be derived from the Baseline version in a single location so the
  relocation lands as a one-line change.
- **The Dev Container ships no `protoc`** ([ADR-0002](0002-dev-container-runtime.md) keeps it lean),
  and `prost-build` by itself shells out to `protoc`. Requiring a system protobuf compiler would add
  a system package to the container *and* to every CI runner, including the Windows and macOS client
  builders.

## Decision

We will **vendor** the Baseline's schema files unchanged (`opamp.proto`, `anyvalue.proto` from
upstream tag `v0.18.0`) inside the `opamp` crate under a **version-named directory**
(`crates/opamp/proto/v0.18.0/`), and compile them at build time with **`prost`** using
**`protox`** — a pure-Rust protobuf compiler — so no system `protoc` exists anywhere in the build
chain. The Baseline version string appears in the build script as the single constant from which
both the file paths and the include root are derived.

Concretely:

- `crates/opamp/build.rs` holds `BASELINE = "v0.18.0"`, compiles
  `proto/{BASELINE}/opamp.proto` with include root `proto/{BASELINE}/`, via
  `protox::compile` feeding `prost_build`.
- The generated module (`opamp.proto.v1`) is re-exported as `opamp::proto`; all other code uses
  those types and never touches paths or codegen.
- The vendored files are byte-identical to upstream's tag. Upgrading the Baseline means adding a new
  version directory, flipping the constant, and following the procedure in
  [`CONFORMANCE.md`](../CONFORMANCE.md) — the old directory is deleted in the same change.
- The OpAMP WebSocket framing (varint header `0` + protobuf body) lives next to the generated types
  in the `opamp` crate, reusing `prost`'s varint codec rather than a second implementation.

## Alternatives considered

- **Fetch the schema at build time** — rejected. It makes the wire contract a function of network
  state, breaks offline builds, and hides schema changes from review.
- **System `protoc` + plain `prost-build`** — rejected. Adds a system dependency to the Dev
  Container and three CI operating systems for something `protox` does in pure Rust inside the
  existing cargo build. The repository's earlier lineage used this and it was the single system
  package the build chain needed.
- **`protobuf` / `rust-protobuf` instead of `prost`** — rejected. `prost` is the ecosystem default
  (tonic, OpenTelemetry Rust), generates idiomatic types, and its varint codec doubles for the
  WebSocket framing header.
- **Depend on an existing OpAMP Rust crate (e.g. `opamp-rs`)** — rejected. The specification's
  strategy is to own both ends in Rust against the pinned Baseline; a third-party wire crate would
  pin us to *its* schema revision and capability subset, exactly the drift ADR-0004 exists to
  prevent.
- **Commit the generated `.rs` instead of generating at build time** — tempting for build speed,
  but the generated file then drifts from the vendored schema unless a check regenerates it anyway;
  build-time generation keeps schema and types incapable of disagreeing.

## Sources / Prior art

- [`opamp.proto` at `v0.18.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.18.0/proto/opamp.proto)
  and [`anyvalue.proto` at `v0.18.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.18.0/proto/anyvalue.proto)
  — the vendored files; protobuf package `opamp.proto.v1`.
- [`protox`](https://crates.io/crates/protox) — pure-Rust protobuf compilation for `prost`;
  `protox 0.9` pairs with `prost 0.14` (dependency requirements verified against the crates.io
  index, 2026-07-22).
- [`prost`](https://docs.rs/prost) — the generated-type toolchain; its
  `encoding::{encode_varint, decode_varint}` implements the LEB128 codec the OpAMP WebSocket header
  uses.
- [OpAMP specification § WebSocket Transport](https://github.com/open-telemetry/opamp-spec/blob/v0.18.0/specification.md)
  — each WebSocket message is a varint header (value `0` in this protocol version) followed by the
  protobuf-encoded message body.
- Proto relocation analysis in [`CONFORMANCE.md`](../CONFORMANCE.md) — the single-place path rule
  this ADR operationalizes; prior work in this repository's history (`6fba83b` lineage) vendored the
  schema the same way but required system `protoc`.

## Consequences

- Positive: fully offline, reproducible builds on all three client platforms with zero system
  dependencies; every schema change is a reviewable diff; the Baseline version is visible in the
  tree (`proto/v0.18.0/`) and mechanically tied to the build.
- Positive: the upstream proto relocation, when the Baseline moves past it, is a new directory plus
  a constant flip — exactly the one-line adoption `CONFORMANCE.md` demands.
- Negative / trade-offs: `protox` joins the trust base for the wire contract (build-dependency
  only). Accepted: it is maintained alongside `prost-reflect` and used widely; a divergence would
  surface as a decode failure against `opamp-go`-based peers in interoperability testing.
- Negative / trade-offs: build-time codegen costs a few seconds of first-build time in every crate
  build. Accepted for the impossibility of schema/type drift.
- Follow-ups: interoperability testing against `opamp-go` as the behavioural oracle (already
  flagged in ADR-0004) will exercise the generated types against the reference implementation.
