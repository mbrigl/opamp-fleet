# ADR-0005: A Cargo workspace — `opamp-server`, `opamp-supervisor`, and a shared `opamp-proto`

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The [specification](../SPECIFICATION.md) commits the project to Rust on both ends — a **Server** and a
**Supervisor Host** — and [ADR-0004](0004-rust-toolchain-dev-container.md) provisions the toolchain.
Both ends speak OpAMP, so both need the **same** generated message types and the **same** WebSocket
framing. Giving each binary those types independently means vendoring the schema twice and running
`protoc` twice, and letting the two copies drift — for a wire protocol a correctness hazard, not just
untidiness. How the repository is carved into crates fixes the build's structure and dependency graph,
so it is recorded here.

## Decision

We will lay the repository out as a **Cargo workspace** with three members and one shared protocol
crate.

- **`crates/opamp-proto`** — the shared wire layer: the vendored `proto/opamp/v1/*.proto`, the
  `build.rs` + `prost-build` code generation, and the WebSocket varint framing. It is the *single*
  place the schema is vendored and generated, and both binaries depend on it.
- **`crates/opamp-server`** — the OpAMP Fleet Server binary (`opamp-server`), plus a library so its
  protocol handling can be exercised from integration tests.
- **`crates/opamp-supervisor`** — the Supervisor Host binary (`opamp-supervisor`). Initially a
  skeleton; its full plugin/hexagonal implementation follows in later ADRs.
- The **root `Cargo.toml`** is `[workspace]` (members `crates/*`, resolver `"2"`), with one shared
  `Cargo.lock` and dependency versions pinned once under `[workspace.dependencies]` so the two binaries
  never diverge on `tokio`, `prost`, or the OpAMP wire crate. `rust-toolchain.toml`
  ([ADR-0004](0004-rust-toolchain-dev-container.md)) stays at the root so the whole workspace resolves
  one compiler.

## Alternatives considered

- **Generate the Protobuf types independently in each crate.** No shared crate, but the vendored schema
  and framing exist twice and drift silently — rejected as a wire-format correctness hazard.
- **Two entirely separate Cargo projects (no workspace).** Maximum isolation, but duplicate dependency
  resolution, no shared lockfile, and no natural home for shared protocol code. Rejected as
  over-separation for one repository.
- **One crate with two `[[bin]]`s.** Avoids the workspace, but couples the Server and the Supervisor
  Host into one dependency set and crate boundary, so they cannot build and test independently.
  Rejected.
- **A fourth crate for framing, separate from the generated types.** More crates than the problem needs
  (YAGNI); framing and the generated types are both "the OpAMP wire format" and are used together. One
  `opamp-proto` holds both until there is a reason to split.

## Sources / Prior art

- The requirement: [`SPECIFICATION.md`](../SPECIFICATION.md) ("Own both ends in Rust"). Toolchain:
  [ADR-0004](0004-rust-toolchain-dev-container.md).
- Cargo workspaces (shared `Cargo.lock`, `members`, resolver) —
  <https://doc.rust-lang.org/cargo/reference/workspaces.html>.
- `prost-build` code generation and its `protoc` requirement — <https://crates.io/crates/prost-build>.

## Consequences

- Positive: the OpAMP schema is vendored and generated **once**; the framing is written **once** and
  reused by both ends; each binary builds and tests as its own crate. `cargo build/test/clippy/fmt
  --workspace` covers everything.
- Negative / trade-offs: a three-crate workspace is more structure than a single binary needs on day
  one; it is justified by the second binary and the shared wire layer, both of which the specification
  requires.
- Follow-ups: the OpAMP server protocol implementation ([ADR-0006](0006-rust-opamp-server-from-spec.md))
  and the REST API + UI ([ADR-0007](0007-rest-api-and-fleet-ui.md)) build on this layout; the Supervisor
  Host's plugin/hexagonal structure is a later ADR.
