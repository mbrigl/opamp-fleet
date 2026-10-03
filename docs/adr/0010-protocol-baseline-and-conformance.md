# ADR-0010: The protocol is pinned to a released Baseline, compiled from a vendored schema, and checked against opamp-go

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl
- **Applies to:** docs/CONFORMANCE.md, crates/opamp/build.rs, crates/opamp/proto/, the Protocol Baseline check in scripts/check-docs.sh, and every change that adds or alters protocol behaviour

## Context

The [specification](../SPECIFICATION.md) commits to implementing OpAMP in full and in step with
upstream (goals 12 and 13). Three properties of the protocol and of this project make that
commitment impossible to keep by good intentions alone.

**OpAMP is a moving target.** `opamp-spec` is itself Beta and releases regularly. An implementation
that tracks `main` has no stable contract to test against; one that silently stays on an old release
drifts unnoticed.

**The protocol is not uniformly mature.** Capabilities carry their own `[Beta]` or `[Development]`
markers. Only `ReportsStatus` (Agent) and `AcceptsStatus` (Server) are required, and neither side may
assume a capability the other has not declared. "We implement OpAMP" is therefore not a statement an
operator can act on without a per-capability record of what is live and how mature it is upstream.

**The two ends share their reading.** The Client and the Server are tested against each other end to
end, but both depend on `crates/opamp` and were written from the same sentences. A misread MUST is
symmetric: two peers that agree perfectly and are both wrong. Only another implementation can find
that class of defect.

The build has constraints of its own. The wire contract must be reproducible and reviewable, builds
must work offline, the Dev Container ships no `protoc` ([ADR-0002](0002-dev-container-runtime.md)),
and plain `prost-build` shells out to one. Upstream has relocated its proto files once
(`proto/` to `proto/opamp/v1/`) without changing the package `opamp.proto.v1`, so the path to the
schema must live in exactly one place.

## Decision

We will pin one released `opamp-spec` version as the Protocol Baseline, record it with a
per-capability conformance matrix in [`docs/CONFORMANCE.md`](../CONFORMANCE.md), compile its vendored
schema with `prost` through the pure-Rust `protox`, warn when upstream has released past it, and test
both ends against `opamp-go` in a pinned, separately scheduled CI job.

1. **The Baseline is a released version, never a branch.** All protocol code targets one released
   `opamp-spec` tag, today `v0.20.0`. The pin and the list of known upstream changes since it follow
   upstream *releases* only; unreleased commits on `main` are not tracked, listed, or reconciled
   against. Moving the Baseline is a deliberate change that follows the procedure in
   [`CONFORMANCE.md`](../CONFORMANCE.md#upgrading-the-baseline).

2. **`CONFORMANCE.md` records every capability on both ends.** Each row states implementation status
   (implemented, planned, not planned), upstream maturity (stable, Beta, Development) transcribed from
   the Baseline's `opamp.proto`, and whether the protocol requires it. Requirements that are not
   capability bits (transports, size limits, `instance_uid` handling) have their own section, and a
   deliberate departure from the Baseline is recorded under *Deviations* with its reason.

3. **The matrix changes with the behaviour.** A change that adds, removes or alters protocol
   behaviour updates `CONFORMANCE.md` in the same change.

4. **Divergence from upstream warns, never fails.** `check_protocol_baseline` in
   [`scripts/check-docs.sh`](../../scripts/check-docs.sh) reads the
   `<!-- protocol-baseline: vX.Y.Z -->` marker, compares it with the newest entry of upstream's
   releases list, and warns on a difference. A missing marker is an error. Without network it prints
   a note and skips, so the other documentation checks stay usable offline.

5. **The schema is vendored unchanged under a version-named directory.** `opamp.proto` and
   `anyvalue.proto` sit byte-identical to the upstream tag in
   `crates/opamp/proto/<BASELINE>/opamp/v1/`. Moving the Baseline adds the new directory and deletes
   the old one in the same change.

6. **One constant derives every path.** `crates/opamp/build.rs` holds `BASELINE`; the file paths and
   the include root `proto/<BASELINE>` (the directory above `opamp/v1`) derive from it. No proto path
   is written anywhere else.

7. **`protox` compiles, `prost` generates, at build time.** `protox::compile` feeds
   `prost_build`, so no system `protoc` exists anywhere in the build chain. The generated package is
   exposed as `opamp::proto`; all other code uses those types and never touches paths or codegen.
   Generated code is not committed.

8. **The WebSocket framing lives beside the types.** `opamp::frame` writes and reads the varint
   header `0` before the protobuf body with `prost`'s varint codec, rather than a second
   implementation.

9. **`opamp-go` is the behavioural oracle, and the only one.** It is the reference implementation and
   what the Collector's `opampextension` and `opampsupervisor` are built on. The Collector carrying
   `opampextension` is the candidate for a second oracle, not another library.

10. **Both directions are tested.** Our Server is driven by `opamp-go`'s Client, and our Client is
    pointed at `opamp-go`'s Server.

11. **The oracle is pinned to its newest release, recorded beside the Baseline.** It gets its own row
    in `CONFORMANCE.md`. Moving it is a deliberate change like a Baseline move, including re-reading
    what the new version brought.

12. **What the oracle cannot reach is written down.** `opamp-go`'s newest release implements an older
    `opamp-spec` than the Baseline. `CONFORMANCE.md` marks which rows the oracle covers and which lie
    beyond its specification version, so "interop-tested" never reads as "all of it".

13. **A named scenario list, not a certification suite.** The job proves, end to end and on both
    transports where the Baseline offers both: connect and report; `sequence_num` continuity and the
    `ReportFullState` recovery a gap triggers; the remote-config offer, its acknowledgement, and the
    hash gate that stops it repeating; capability negotiation in both directions; identity handling
    including a Server-assigned `AgentIdentification`; and `agent_disconnect` on shutdown.

14. **It runs like the service smoke test.** An `#[ignore]`d Rust test drives the scenarios, and a
    dedicated workflow runs it on a schedule and on demand, never on every push, as
    `crates/fleet-agent/tests/service_smoke.rs` and
    [`service-smoke.yml`](../../.github/workflows/service-smoke.yml) do. `cargo test --workspace`
    stays self-contained.

15. **The Go side is a pinned module under `interop/`.** A small Go program with its own `go.mod`
    pinning `opamp-go` and a checked-in `go.sum`; CI installs Go with `actions/setup-go`. The Dev
    Container gets no Go toolchain; the README's *Build, Test & Run* section says that a developer
    who runs the job locally installs Go, and that nothing else needs it.

16. **A failure is triaged before it is a defect.** Three outcomes, named in the job's
    documentation: our bug, which is fixed; the oracle lagging the Baseline, recorded like a known
    upstream gap with the scenario pinned to the older behaviour or skipped by name; a genuine
    ambiguity in the specification, which goes upstream as an issue linked from its row.

**Out of scope:** whether a passing interop run gates a release; a second, non-blocking job against
`opamp-go`'s `main`.

## Alternatives considered

- **The matrix in `SPECIFICATION.md`.** The specification states the commitment and changes rarely;
  the matrix changes with nearly every protocol change.
- **Conformance only in code.** The code cannot lie about the bits it sets, but it is unreadable to
  an operator and cannot say *planned* or *not planned*.
- **Manual reconciliation, no check.** The discipline that erodes first.
- **Fail CI on divergence.** CI would go red when upstream tags a release with nothing wrong here, and
  the reflex fix is to disable the check.
- **Track upstream `main`.** No fixed contract to test against; any commit could move behaviour under
  the implementation. The same holds for the oracle: a red run against an unreleased `opamp-go` could
  be our bug, theirs, or their refactor.
- **Fetch the schema at build time.** Makes the wire contract a function of network state and hides
  schema changes from review.
- **System `protoc` with plain `prost-build`.** Adds a system package to the Dev Container and three
  CI operating systems for what `protox` does inside the cargo build.
- **`rust-protobuf` instead of `prost`.** `prost` is the ecosystem default (tonic, OpenTelemetry
  Rust), and its varint codec doubles for the framing header.
- **An existing OpAMP Rust crate.** It would pin the project to *its* schema revision and capability
  subset.
- **Committing the generated `.rs`.** It drifts from the vendored schema unless a check regenerates
  it anyway.
- **Vendoring `opamp-go`.** The schema is vendored because it is a small build input; a Go library
  is a large test fixture, and a module pin gives the same reproducibility.
- **A harness written entirely in Go.** It could not assert on our side's state: the fleet view, a
  Supervisor's status, what the Client wrote to disk.
- **The Collector as the first oracle.** It drags in a distribution, its configuration and its
  release cadence, giving a failure three plausible homes.
- **Relying on the existing end-to-end suite.** Structurally incapable of catching a misreading both
  peers share.

## Sources / Prior art

- [`open-telemetry/opamp-spec`](https://github.com/open-telemetry/opamp-spec) — release history and
  Beta status; its `opamp.proto` is the source of capability bits and maturity markers, and its
  [`specification.md`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) the
  source of the MUST/SHOULD/MAY rules and of the WebSocket framing (varint header `0`, then the
  protobuf body).
- [`protox`](https://crates.io/crates/protox) — pure-Rust protobuf compilation for `prost`
  (`protox 0.9` pairs with `prost 0.14`); [`prost`](https://docs.rs/prost) — generated types and
  `encoding::{encode_varint, decode_varint}`.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — the reference implementation:
  `client`, `server`, `protobufs`, and an `internal/examples` module with Dockerfiles this project
  cannot use ([ADR-0002](0002-dev-container-runtime.md)). Its release notes give each release's
  `opamp-spec` version; `v0.23.0` (2026-02-18) implements `v0.16.0`.
- Neither `opamp-go` nor `opamp-spec` ships a conformance suite for third-party implementations,
  which is why clause 13 bounds the job to a named list.
- [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — a widely deployed client implementing a deliberately small subset, discoverable only from its
  documentation.

## Consequences

- Positive: "which OpAMP does this speak" has a written answer, and the maturity column makes the
  risk of building on a `[Development]` feature visible at the point of decision.
- Positive: builds are offline and reproducible on all three Client platforms with no system protobuf
  compiler; every schema change is a reviewable diff, and a future upstream relocation is a new
  directory plus a constant.
- Positive: the oracle is what the ecosystem runs, so passing it speaks for `opampextension`,
  `opampsupervisor` and agents built on `opamp-go`.
- Negative / trade-offs: `CONFORMANCE.md` is hand-maintained; nothing verifies that a row claimed
  *implemented* is implemented, apart from the scenarios the oracle reaches.
- Negative / trade-offs: the currency check needs network and degrades quietly, so it can do nothing
  in a sandboxed CI.
- Negative / trade-offs: `protox` joins the trust base for the wire contract as a build dependency,
  and codegen costs a few seconds of first build.
- Negative / trade-offs: the oracle cannot reach the newest Baseline features, a second toolchain
  enters the project's life, and a scheduled job nobody reads is decoration; clause 16 is what makes
  it worth having.
- Follow-ups: the harness under `interop/`, its workflow and the oracle row and coverage marks in
  `CONFORMANCE.md` (clauses 11 to 16) are not yet in the repository; until they are, no run backs
  the oracle clauses. Also: a non-blocking job against `opamp-go`'s `main` if its releases stay
  frozen; the Collector as a second oracle; generating the matrix from the code.

## Enforcement

- `check_protocol_baseline` in [`scripts/check-docs.sh`](../../scripts/check-docs.sh) fails on a
  missing Baseline marker and warns on divergence from upstream's newest release (clauses 1, 4).
- [`ci.yml`](../../.github/workflows/ci.yml) builds the workspace on Linux, Windows and macOS, and
  neither it nor the Dev Container installs `protoc`; every build compiles the generated types from
  the vendored files through `BASELINE` (clauses 5 to 7).
- `crates/opamp/src/frame.rs`: `round_trips_a_message`, `rejects_a_non_zero_header`,
  `rejects_a_truncated_header` (clause 8).

**Not mechanically decidable:** that the matrix tells the truth (clauses 2, 3) and that the Baseline
is a release (clause 1) are review duties when a protocol change or a Baseline move lands. The
oracle clauses 9 to 16 are enforced by the interop job once it exists.
