# ADR-0004: Rust toolchain and build tooling in the Dev Container

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The [specification](../SPECIFICATION.md) commits the project to Rust — "Own both ends in Rust" — for
both the Server and the Supervisor Host. [ADR-0002](0002-dev-container-runtime.md) deliberately shipped
**no language toolchain** ("each project adds its own and fills in the Build, Test & Run section"), and
[ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md) defines the container through Compose
but says nothing about how Rust gets into it. That choice is architecture-relevant — it fixes how the
container and CI resolve a compiler, and adds a build dependency — so it is recorded here.

Two build-time tools beyond the compiler are needed by the stack the specification describes:

- **`protoc`** — the OpAMP wire types are generated from the protocol's vendored `.proto` schema at
  build time (`prost-build` requires `protoc`). Without it the workspace cannot build.
- **A pinned `otelcol-contrib` binary** — so a Collector Supervisor developed inside `dev` can own and
  restart a real Collector locally, matching the version the `opamp-agent` sidecar runs
  ([ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md)).

## Decision

We will install the Rust toolchain via the **Dev Container Rust Feature** and pin it, and install the
extra build tooling from a reviewable script.

- **Toolchain:** the `ghcr.io/devcontainers/features/rust` Feature (installs `rustup`); the exact
  channel and the `rustfmt`/`clippy` components are pinned in **`rust-toolchain.toml`** at the
  repository root, so the Dev Container and CI resolve the **same** compiler.
- **Build tooling:** an `onCreateCommand` script `.devcontainer/install-tools.sh` installs
  `protobuf-compiler` (`protoc`) from the distribution and a **pinned** `otelcol-contrib`
  (`0.156.0`, kept in sync with the `opamp-agent` sidecar). Keeping the tools and the pinned version in
  a script makes them visible and reviewable.
- **Editor:** the `rust-lang.rust-analyzer` VS Code extension is installed in the container.

## Alternatives considered

- **Install `rustup` by hand in the script** — reinvents what the maintained Rust Feature already does
  (PATH, profiles, components). Rejected; the Feature is the standard, and `rust-toolchain.toml` still
  pins the channel.
- **Vendor `protoc` through `protoc-bin-vendored`** — removes the system dependency but ships prebuilt
  binaries inside a crate, a supply-chain surface. Rejected in favour of the distribution's `protoc`.
- **Do not pin the toolchain** — lets the container and CI drift onto different compilers, so "builds
  clean locally" stops implying "builds clean in CI". Rejected; `rust-toolchain.toml` pins it.
- **Bundle the Rust toolchain into a custom base image** — premature; the Feature + pin is lighter and
  keeps the base image the shared one from [ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md).

## Sources / Prior art

- The requirement: [`SPECIFICATION.md`](../SPECIFICATION.md) ("Own both ends in Rust"). The container it
  layers onto: [ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md).
- Dev Container Rust Feature — <https://github.com/devcontainers/features/tree/main/src/rust>.
- Toolchain file (`rust-toolchain.toml`, channel + components) —
  <https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file>.
- `prost-build` requires `protoc` — <https://crates.io/crates/prost-build>.
- `otelcol-contrib` releases (pinned to match the sidecar) —
  <https://github.com/open-telemetry/opentelemetry-collector-releases>.

## Consequences

- Positive: one compiler for the container and CI; `rustfmt`/`clippy` present for the quality bar; the
  build's `protoc` dependency and the local Collector binary are explicit and pinned.
- Negative / trade-offs: the container gains a `protoc` package and an `otelcol-contrib` download at
  create time. Until the Cargo workspace exists there is nothing for `cargo build` to compile — the
  toolchain is provisioned ahead of the code it will build.
- Follow-ups: the Cargo workspace and its crates (Server, Supervisor Host, shared protocol crate); the
  **Build, Test & Run** section of [`README.md`](../../README.md) and the CI Rust jobs, once crates
  exist.
