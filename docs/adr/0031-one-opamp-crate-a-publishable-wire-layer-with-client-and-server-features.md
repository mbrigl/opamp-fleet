# ADR-0031: One `opamp` crate — a publishable wire layer always, the client and the server behind features, and what is this project's own in an internal crate

- **Status:** 🟢 accepted
- **Date:** 2026-10-02
- **Deciders:** Markus Brigl
- **Applies to:** `crates/opamp/` (its manifest and `[features]`, `build.rs`, the feature gates in `src/lib.rs`, `LICENSE`, `NOTICE` and `README.md`), `crates/fleet-core/`, every item that moves between the two, and the per-feature lint in `.github/workflows/ci.yml` and `README.md`

## Context

`crates/opamp` is the internal seam between the Server and the Client. Most of it is OpAMP as the
Baseline defines it and nothing else: the generated protobuf types, the WebSocket framing, the
endpoint shell (path, media type, gzip with the post-decompression limit), `InstanceUid`, and the
attribute keys with their accessors. That is exactly what anyone writing an OpAMP server or agent
in Rust needs before writing a line of their own, and today nobody outside this repository can use
it.

The goal is to make that part reusable as a crate. Measured against the code, three things stand
in the way:

1. **The build script refuses to build outside this repository.** `build.rs` resolves this
   project's version from git ([ADR-0013](0013-versions.md)) and fails closed with *"not inside a
   git repository"* when there is none. A crate fetched from crates.io is never inside one, so it
   would not compile for anyone but us. `git2` is in its build dependencies for the same reason.
2. **Three modules are this project's policy, not the protocol.**
   - `opamp::version` — `current()` is this build's baked version, and `parse` / `identity` /
     `same_release` / `precedence` implement this project's version grammar (ADR-0013).
   - `opamp::pem` — a rustls PEM reader. Not OpAMP at all; it is here because both ends read
     certificates ([ADR-0011](0011-workspace-crates-and-configuration.md)).
   - `canonical_os` / `canonical_arch` in `opamp::attributes` — this project's platform alias table
     ([ADR-0020](0020-the-package-store.md)), the leniency that lets a release file name and an
     Agent's report meet.
3. **The package metadata is missing.** `publish = false` for the whole workspace, no
   `repository`, no `readme`, and the vendored `.proto` files ship without the upstream licence
   they are distributed under (`opamp-spec` is Apache-2.0).

The wire layer is not all an OpAMP program needs. The server endpoint
([ADR-0032](0032-one-opamp-server-endpoint-for-every-server-surface.md)) and the client
([ADR-0033](0033-an-agents-side-of-opamp-is-one-reusable-client.md)) are reusable too, and they
share the wire layer's version by decision — `0.20.x`, the Baseline's — and break together with it:
a new Baseline changes the types all of them expose, and a `prost` minor version is part of all
their APIs. Neither side is usable without the wire layer. Separate crates would offer independent
release cycles the versioning has already given up, and triple the publishing and the
documentation. The established Rust shape for a protocol library with two optional sides is one
crate with features — `hyper` (`client` and `server` opt-in since 1.0), `tonic` (`server`,
`channel`); `opamp-go` is one module with `client` and `server` packages.

The forces:

- **A present need.** ADR-0011 keeps seams as modules *until a concrete need appears*; a crate
  that others can depend on is one.
- **The specification neither forbids nor asks for a reusable OpAMP library.**
  [`docs/SPECIFICATION.md`](../SPECIFICATION.md) has no sentence either way; that is recorded under
  the follow-ups rather than resolved here.
- **Simplicity first.** A user who wants only the types must not pull a web framework, and a user
  of one side must not pull the other's dependencies.

What this changes in ADR-0011 (which crates exist, what the shared crate holds) and in ADR-0013
(where the version is resolved and read) is restated by
[ADR-0034](0034-five-crates-a-publishable-wire-layer-and-toml-configuration.md) and
[ADR-0035](0035-versions-resolved-in-the-internal-crate.md), which supersede those two.

## Decision

We will make **`crates/opamp` one publishable OpAMP crate**: without features the wire layer, which
holds only what the Baseline defines, and two features, `client` and `server`, neither on by
default. What is this project's own moves into a new internal crate, **`crates/fleet-core`**.

1. **`opamp` keeps the wire layer, and without features it is nothing else:** `proto` (generated
   from the vendored Baseline schema), `frame`, `endpoint`, `uid`, `BASELINE`, and in `attributes`
   the Baseline's keys with `string_value`, `string_attr` and `string_array_attr`, depending on
   `prost`, `uuid` and `flate2` only — free of any web framework. The "an empty string is not a
   value" rule of `string_value` stays and stays documented — it is a property of the accessor,
   not of this fleet. Its public API is otherwise unchanged, so callers change only the path of
   what moves.
2. **`fleet-core` takes:** `version` (with `current()`), `pem`, and the platform alias table
   (`canonical_os`, `canonical_arch`). It is `publish = false`, and it holds what both ends
   implement identically, by measurement.
3. **The build split follows the modules.** `opamp/build.rs` only generates the protobuf types,
   with protox exactly as [ADR-0010](0010-protocol-baseline-and-conformance.md) decided — no system
   `protoc`, no git, no network, output only to `OUT_DIR`. The version resolution moves unchanged
   into `fleet-core/build.rs`, and `git2` with it. `OPAMP_FLEET_VERSION` keeps its name and
   meaning.
4. **`opamp`'s version is the Baseline's.** It no longer inherits `[workspace.package] version`,
   which is the *product's* release number (ADR-0013). Its `MAJOR.MINOR` is the Baseline's, so the
   crate generated from `v0.20.0` is `0.20.x`, and a reader of `Cargo.toml` knows which OpAMP they
   depend on without opening the crate. The **patch number is the crate's own**: a fix that keeps
   the API compatible is `0.20.1`, `0.20.2`, …. What follows from that:
   - **A breaking change waits for the next Baseline.** For a `0.x` crate only a minor bump may
     break, and the minor belongs to the standard — so a `prost` minor bump or a change to the
     crate's own API ships together with the move to the next Baseline, never between two.
   - **An upstream patch release is not mirrored.** opamp-spec has never published one (every
     release from `v0.1.0` to `v0.20.0` is `v0.N.0`); should it, the crate takes it in as one of its
     own patches if the generated types stay compatible, and with the next minor otherwise.
   - **One source, checked.** The Baseline is exposed as `opamp::BASELINE`, and a test fails the
     build when `MAJOR.MINOR` of the crate's version and of `BASELINE` disagree, so the two cannot
     drift apart by an edit to one of them.
5. **It is publishable, and publishing is a separate act.** `publish = true` for `opamp` only, with
   `description`, `license = "Apache-2.0"`, `repository`, `readme`, `keywords` and `categories`, and
   opamp-spec's licence beside the vendored schema. The change is verified with
   `cargo package --list` and `cargo publish --dry-run`; the first real `cargo publish` is the
   maintainer's decision, because a published version can be yanked but never removed.
6. **`client`** adds `opamp::client` — state machine, `Session`, drivers for both transports,
   `Backoff` — with tokio, tokio-tungstenite, futures-util, reqwest and ring. One feature for both
   transports: no user needs one alone, and splitting a feature later is compatible where removing
   one is not.
7. **`server`** adds `opamp::server` — `Handler`, `router` — with axum and tokio.
8. **Each feature is linted on its own** (no features, `client`, `server`), in the README's lint step
   and in CI: the workspace build unifies features and would hide a missing `#[cfg]`.
9. **docs.rs builds with all features** and marks what each module needs.

**Out of scope:** what the server endpoint and the client do (ADR-0032, ADR-0033); finer features
(one per transport, the state machine alone); a release routine for the crate.

## Alternatives considered

- **A version of the crate's own, independent of the Baseline** — the convention of
  `opentelemetry-proto` (0.33 for OTLP 1.x) and `opamp-go` (v0.25 for specification v0.20), and
  freer: a `prost` bump could break at any time. Not chosen: the dependency a user declares is then
  silent about which OpAMP it speaks, and this crate exists to carry exactly one Baseline. Holding
  breaking changes for the next Baseline is the price, and a small one while upstream releases
  every few months.
- **Commit the generated Rust code instead of generating it in `build.rs`.** This is the dominant
  pattern for published protobuf crates (`opentelemetry-proto`, `tonic-types`, `prost-types`,
  `opamp-go`'s `protobufs`), and it spares every consumer from compiling protox and prost-build.
  Not chosen *now*: it reverses ADR-0010 and needs a regeneration path plus a CI check that the
  committed code is current. The present build already works for consumers — the schema is inside
  the package and protox needs no system tool — so it is a follow-up if build time turns out to
  matter to them.
- **Keep everything in `opamp` and gate the project parts behind a feature.** Rejected: the git
  build step would sit in a published crate's build script behind a flag, and the crate would
  still describe this fleet's version grammar and platform aliases as if they were OpAMP.
- **Publish a separate `opamp-proto` crate and keep `opamp` internal.** Rejected: it splits the
  wire layer where nothing asks for a split. Framing and the endpoint shell are as much the
  Baseline as the types are; `opamp-go` keeps them together too.
- **Separate crates per side** — rejected: they share a version and break together.
- **Finer features** (one per transport, the state machine alone) — not now; adding them later
  under `client` breaks no one.
- **`client` and `server` on by default** — rejected: a types-only user would pull both dependency
  sets, which is what the wire layer of clause 1 keeps away.
- **Depend on an existing crate instead.** None fits: `otel-opamp-rs` is a client pinned to a
  2023 draft of the specification and dormant; `newrelic-opamp-rs` is unpublished and licensed for
  use with New Relic's service only. No permissively licensed crate on crates.io carries the
  current Baseline.

## Sources / Prior art

- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — public `protobufs` and
  `protobufshelpers`, framing in `internal`, client and server as separate packages of one module;
  its module version (v0.25) is independent of the specification's (v0.20).
- [hyper features](https://docs.rs/crate/hyper/latest/features),
  [tonic features](https://docs.rs/crate/tonic/latest/features) — one crate, two optional sides.
- [`opentelemetry-proto`](https://github.com/open-telemetry/opentelemetry-rust/tree/main/opentelemetry-proto),
  [`tonic-types`](https://github.com/hyperium/tonic/tree/master/codegen),
  [`prost-types`](https://github.com/tokio-rs/prost) — committed generated code; the alternative
  recorded above.
- [`otel-opamp-rs`](https://crates.io/crates/otel-opamp-rs) (0.0.14, specification commit from
  2023-05) and [`newrelic-opamp-rs`](https://github.com/newrelic/newrelic-opamp-rs) (proprietary
  New Relic Software License) — the existing Rust crates, checked 2026-10-01. The name `opamp` is
  free on crates.io as of the same date.
- [protox](https://github.com/andrewhickman/protox) — pure-Rust protobuf compilation for
  prost-build; why a consumer needs no `protoc`.
- Cargo: [publishing](https://doc.rust-lang.org/cargo/reference/publishing.html),
  [build scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html) (write only to
  `OUT_DIR`), [version-incompatibility hazards](https://doc.rust-lang.org/cargo/reference/resolver.html#version-incompatibility-hazards),
  [features](https://doc.rust-lang.org/cargo/reference/features.html) and
  [feature unification](https://doc.rust-lang.org/cargo/reference/features.html#feature-unification);
  SemVer for [adding](https://doc.rust-lang.org/cargo/reference/semver.html#cargo-feature-add) and
  [removing](https://doc.rust-lang.org/cargo/reference/semver.html#cargo-feature-remove) a feature;
  [docs.rs builds](https://docs.rs/about/builds) (read-only source, no network) and
  [docs.rs metadata](https://docs.rs/about/metadata);
  [API Guidelines C-STABLE](https://rust-lang.github.io/api-guidelines/necessities.html#c-stable)
  (a crate exposing `prost` types cannot be 1.0 before `prost` is).

## Consequences

- Positive: a Rust OpAMP implementer gets the current Baseline's types, framing and endpoint rules
  from one dependency, with no system tools — and either side on top of them from the same one,
  `opamp = { version = "0.20", features = ["client"] }`. Both ends of this project keep using
  exactly that code, so what is published is what this fleet runs on.
- Positive: the line between "OpAMP" and "this fleet's policy" becomes a crate boundary instead of
  a comment. A version-grammar or platform-alias change can no longer look like a protocol change.
- Positive: the crate's version says which OpAMP it is — `opamp = "0.20"` is the `v0.20.0`
  Baseline, and moving to a new one is a deliberate edit of that requirement. One thing to
  publish, version and document.
- Negative / trade-offs: `prost` becomes part of a public API, and since the minor number belongs
  to the standard, a `prost` minor bump can only ship with the next Baseline. The crate stays `0.x`
  as long as the specification does.
- Negative / trade-offs: a fifth workspace member, and call sites on both ends change their import
  paths for the moved items.
- Negative / trade-offs: `#[cfg(feature = …)]` in the crate root, and a lint per feature, since only
  those builds prove a feature stands alone.
- Negative / trade-offs: consumers compile protox and prost-build. Accepted for now; see the second
  alternative.
- Follow-ups: whether the specification should name a reusable wire crate as a goal; publishing and
  a release routine for `opamp` (its changelog, semver checks in CI); committed generated code if
  consumers ask for shorter builds.

## Enforcement

- `crates/opamp/src/lib.rs` `the_crate_version_is_the_baselines` fails the build when the crate's
  `MAJOR.MINOR` and `opamp::BASELINE` disagree (clause 4).
- `crates/opamp/Cargo.toml` declares no git dependency and `crates/opamp/build.rs` reads no
  repository, so `cargo publish --dry-run -p opamp` — which builds from the packaged sources,
  outside any checkout — fails if either comes back (clauses 3, 5).
- The tests of `crates/fleet-core/src/version.rs`, `pem.rs` and `platform.rs` are the moved
  modules' tests, unchanged but for their paths (clause 2).
- [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) lints `opamp` with no feature, with
  `client` and with `server` on their own, and fails on a missing gate (clauses 1, 6–8).

**Not mechanically decidable:** what docs.rs renders (clause 9) is seen only on a published crate.
