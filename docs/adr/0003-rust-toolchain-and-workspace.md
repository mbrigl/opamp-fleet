# ADR-0003: Rust toolchain and a three-crate Cargo workspace

- **Status:** 🟢 accepted
- **Date:** 2026-07-20
- **Deciders:** Maintainer

## Context

The specification ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)) commits the project to Rust on both
ends ("Own both ends in Rust") and to two deployables: a **Server** (control plane) and a **Supervisor
Host** (the client that runs many Supervisors). ADR-0002 deliberately ships **no language toolchain** in
the Dev Container and states that "each project records its own toolchain choice (a new ADR if it
constrains future choices)". Committing the whole codebase to Rust and fixing how the deployables are
laid out is exactly such a choice — it constrains every future dependency and build decision — so it
needs an ADR before any code is written.

Two forces shape the layout. First, the Server and the Supervisor Host must exchange the **same** OpAMP
wire types, so those types (and shared domain helpers like Instance UID and Config hash — see the
vocabulary in the specification) belong in one place both depend on. Second, the specification's
hexagonal-core goal ("One host, many supervisors, behind a hexagonal core") needs the client to be its
own crate that can grow ports and plugins without dragging the Server along.

## Decision

We will add the **Rust stable toolchain** to the Dev Container via the official
`ghcr.io/devcontainers/features/rust:1` Feature (layered on ADR-0002's base image, no host Docker
access), and organise all code as a **single Cargo workspace** with `tokio` as the async runtime and
three crates named with specification vocabulary:

- **`opamp`** (library) — the **shared crate**: the OpAMP wire types and shared domain helpers (Instance
  UID, Config hash, Capability constants). Both binaries depend on it and nothing else depends on them.
- **`server`** (binary) — the **Server**.
- **`supervisor`** (binary) — the **Supervisor Host**.

The exact build/test/run commands are recorded in the **Build, Test & Run** section of
[`README.md`](../../README.md); CI enforces them (AGENTS.md §5).

## Alternatives considered

- **A single crate with binaries under `src/bin/`** — smallest possible tree, but it forces the Server
  and the Supervisor Host to share one dependency set and gives the shared wire types no natural home
  separate from either deployable; it also blocks the hexagonal split the specification calls for.
- **Separate repositories per deployable** — maximal isolation, but the two ends must co-evolve with a
  single shared wire contract; splitting repos adds cross-repo versioning overhead with no present
  benefit (YAGNI).
- **A heavier toolchain install (custom Dockerfile with pinned system deps)** — more control, but the
  Dev Container Feature is the reproducible, ADR-0002-consistent path and the pure-Rust build chain
  (see ADR-0004) needs no extra system packages.
- **A different async runtime (`async-std`, `smol`)** — `tokio` is the de-facto standard that the chosen
  Server and HTTP libraries (ADR-0004, ADR-0005) are built on; picking another would fight the ecosystem.

## Sources / Prior art

- Dev Container Features — Rust: <https://github.com/devcontainers/features/tree/main/src/rust> and
  <https://containers.dev/features>.
- Cargo workspaces: <https://doc.rust-lang.org/cargo/reference/workspaces.html>.
- OpAMP reference implementation structure (shared protobuf types consumed by both ends),
  `opamp-go`: <https://github.com/open-telemetry/opamp-go>.
- Tokio runtime: <https://tokio.rs>.

## Consequences

- Positive: one shared wire contract with a single owner (`opamp`); Server and Supervisor Host evolve
  independently behind it; the client crate is ready to grow ports/plugins without touching the Server;
  reproducible toolchain consistent with ADR-0002.
- Negative / trade-offs: a workspace is slightly more ceremony than one crate; the Dev Container must be
  rebuilt to pick up the Rust Feature (an in-session `rustup` install is used to verify before rebuild).
- Follow-ups: the wire contract and transport are decided separately (ADR-0004); the Server's runtime
  and UI separately (ADR-0005). The hexagonal ports/plugins split inside `supervisor` is deferred until
  a second Supervisor type actually exists (specification: "close the loop before widening it").
