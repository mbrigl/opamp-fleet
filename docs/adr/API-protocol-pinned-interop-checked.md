# API-protocol-pinned-interop-checked: The protocol is pinned to a released Baseline, compiled from a vendored schema, and checked against opamp-go on the endpoint as it ships

- **Status:** 🟢 accepted
- **Date:** 2026-10-07
- **Deciders:** Markus Brigl
- **Applies to:** docs/CONFORMANCE.md, crates/opamp/build.rs, crates/opamp/proto/, scripts/check-protocol-baseline.sh, interop/, crates/fleet-agent/tests/interop_opamp_go.rs, .github/workflows/interop.yml, and every change that adds or alters protocol behaviour

## Context

The [specification](../SPECIFICATION.md) commits to implementing OpAMP in full and in step with
upstream (G-12, G-13), and puts the security of the link first (Q-1). Three properties of the
protocol and of this project make that commitment impossible to keep by good intentions alone.

- **OpAMP is a moving target.** `opamp-spec` is itself Beta and releases regularly. An
  implementation that tracks `main` has no stable contract to test against. One that silently
  stays on an old release drifts unnoticed.
- **The protocol is not uniformly mature.** Capabilities carry their own `[Beta]` or
  `[Development]` markers. Only `ReportsStatus` (Agent) and `AcceptsStatus` (Server) are
  required, and neither side may assume a capability the other has not declared. So "we implement
  OpAMP" is not a statement an operator can act on. It needs a per-capability record of what is
  live and how mature it is upstream.
- **The two ends share their reading.** The Client and the Server are tested against each other
  end to end. But both depend on `crates/opamp` and were written from the same sentences. A
  misread MUST is symmetric: two peers that agree perfectly and are both wrong. Only another
  implementation finds that class of defect.

The build has constraints of its own:

- The wire contract must be reproducible and reviewable, and builds must work offline.
- The Dev Container ships no `protoc`
  ([NUC-container-hides-host-daemon](NUC-container-hides-host-daemon.md)), and plain
  `prost-build` shells out to one.
- Upstream has moved its proto files between directories (`proto/` to `proto/opamp/v1/`) without
  changing the package `opamp.proto.v1`. So the path to the schema must live in exactly one place.

The oracle meets three further facts:

- **The oracle and the Baseline can stand at different versions.** `opamp-go` `v0.25.0`, the
  release the project pins, implements `opamp-spec` `v0.20.0`, the Baseline itself. An oracle
  that reads an older specification than the Baseline returns whenever either side moves. So the
  duty to write the gap down stays.
- **Plaintext runs alone check the endpoint as a test builds it, not as it ships.** They run on
  the loopback with an open admission, as the rest of the suite does. The shipped Server refuses
  plaintext beyond the loopback. It admits by a client certificate in a TLS 1.3 handshake alone
  ([SEC-tls13-two-planes-bounded-input](SEC-tls13-two-planes-bounded-input.md),
  [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)), and the Client
  presents one on both transports. Every installation depends on whether `opamp-go` can speak to
  that endpoint, and ours to an `opamp-go` Server that requires the same.
- **Not every scenario of the list is observable on both ends.** `opamp-go`'s plain-HTTP Client
  sends no `agent_disconnect`. Over WebSocket its goodbye reaches our Server together with the
  closing socket, which marks the Agent disconnected on its own. A test that waits for
  "disconnected" decides nothing there.

## Decision

We will pin one released `opamp-spec` version as the Protocol Baseline, and test both ends against
`opamp-go`:

- a per-capability conformance matrix in [`docs/CONFORMANCE.md`](../CONFORMANCE.md) records the
  Baseline;
- `prost`, through the pure-Rust `protox`, compiles its vendored schema;
- a check warns when upstream has released past it;
- a pinned, separately scheduled CI job tests both ends against `opamp-go`, on the endpoint as it
  ships and in plaintext on the loopback.

1. **The Baseline is a released version, never a branch.**
   - All protocol code targets one released `opamp-spec` tag, today `v0.20.0`.
   - The pin and the list of known upstream changes since it follow upstream *releases* only.
     Nobody tracks, lists, or reconciles against unreleased commits on `main`.
   - Moving the Baseline is a deliberate change. It follows the procedure in
     [`CONFORMANCE.md`](../CONFORMANCE.md#upgrading-the-baseline).

2. **`CONFORMANCE.md` records every capability on both ends.**
   - Each row states the implementation status: implemented, planned, or not planned.
   - Each row states the upstream maturity (stable, Beta, Development), transcribed from the
     Baseline's `opamp.proto`.
   - Each row states whether the protocol requires the capability.
   - Requirements that are not capability bits have their own section: transports, size limits,
     `instance_uid` handling.
   - A deliberate departure from the Baseline stands under *Deviations* with its reason.

3. **The matrix changes with the behaviour.** A change that adds, removes or alters protocol
   behaviour must update `CONFORMANCE.md` in the same change.

4. **Divergence from upstream warns and never fails.**
   - [`scripts/check-protocol-baseline.sh`](../../scripts/check-protocol-baseline.sh) runs in
     `scripts/check-all.sh` and in the `docs` job.
   - It reads the `<!-- protocol-baseline: vX.Y.Z -->` marker and compares it with the newest
     entry of upstream's releases list. It warns on a difference.
   - A missing marker is an error.
   - Without network it prints a note and skips, so the other checks stay usable offline.

5. **The schema is vendored unchanged under a version-named directory.**
   - `opamp.proto` and `anyvalue.proto` sit byte-identical to the upstream tag in
     `crates/opamp/proto/<BASELINE>/opamp/v1/`.
   - A move of the Baseline adds the new directory and deletes the old one in the same change.

6. **One constant derives every path.**
   - `crates/opamp/build.rs` holds `BASELINE`.
   - The file paths and the include root `proto/<BASELINE>`, the directory above `opamp/v1`,
     derive from it.
   - No other place may write a proto path.

7. **`protox` compiles and `prost` generates, at build time.**
   - `protox::compile` feeds `prost_build`, so no system `protoc` exists anywhere in the build
     chain.
   - The crate exposes the generated package as `opamp::proto`. All other code uses those types
     and never touches paths or code generation.
   - Nobody commits generated code.

8. **The WebSocket framing lives beside the types.** `opamp::frame` writes and reads the varint
   header `0` before the protobuf body. It uses `prost`'s varint codec, not a second
   implementation.

9. **`opamp-go` is the behavioural oracle, and the only one.**
   - It is the reference implementation, and the Collector's `opampextension` and
     `opampsupervisor` are built on it.
   - The candidate for a second oracle is the Collector carrying `opampextension`, not another
     library.

10. **Both directions are tested.** `opamp-go`'s Client drives our Server. Our Client is pointed
    at `opamp-go`'s Server.

11. **The oracle is pinned to its newest release, recorded beside the Baseline.**
    - It has its own row in `CONFORMANCE.md`, naming the `opamp-spec` version that release
      implements.
    - Moving it is a deliberate change like a move of the Baseline. It includes reading again what
      the new version brought.

12. **What the oracle cannot reach is written down.**
    - When the oracle's `opamp-spec` version is older than the Baseline, `CONFORMANCE.md` marks
      which rows lie beyond it.
    - When the two are the same, `CONFORMANCE.md` says so.
    - Either way it lists which rows the scenarios reach and which they do not. So
      "interop-tested" never reads as "all of it".

13. **A named scenario list, not a certification suite.** The job proves these end to end, on
    both transports where the Baseline offers both:
    - connect and report;
    - `sequence_num` continuity, and the `ReportFullState` recovery that both an unknown Agent and
      a gap trigger;
    - the remote-config offer, its acknowledgement, and the hash gate that stops it repeating;
    - capability negotiation in both directions, including a declaration the other side stops
      making;
    - identity handling, including a Server-assigned `AgentIdentification`;
    - `agent_disconnect` on shutdown.

    The job asserts a scenario only where the oracle makes its outcome observable. Where it does
    not, `CONFORMANCE.md` and the job's documentation name the scenario as undecided. No test may
    pass either way.

14. **Both directions also run on the endpoint as it ships.**
    - Beside the plaintext runs on the loopback, connect and report and the remote-config round
      trip run over `wss://` and `https://` with TLS 1.3 only.
    - Our Server admits by a client certificate the handshake requires.
    - Our Client presents its certificate to an `opamp-go` Server that requires one.
    - The certificates come from a PKI the test generates. Nothing in the job reaches the network.
    - Both refuse a peer without a certificate the CA issued, and the job asserts that too.

15. **It runs like the service smoke test.**
    - An `#[ignore]`d Rust test drives the scenarios.
    - A dedicated workflow runs it on a schedule and on demand, never on every commit. So do
      `crates/fleet-agent/tests/service_smoke.rs` and
      [`service-smoke.yml`](../../.github/workflows/service-smoke.yml).
    - `cargo test --workspace` stays self-contained.

16. **The Go side is a pinned module under `interop/`, and a puppet.**
    - A small Go program has its own `go.mod` pinning `opamp-go` and a checked-in `go.sum`.
    - It reports what `opamp-go` sees and takes commands. Every assertion lives in the Rust test,
      where both ends' state can be read.
    - CI installs Go with `actions/setup-go`, and formats and vets the program before it runs.
    - The Dev Container gets no Go toolchain. The README's *Build, Test & Run* section says that a
      developer who runs the job locally installs Go, and that nothing else needs it.

17. **A failure is triaged before it is a defect.** The job's documentation names three outcomes:
    - our bug, which is fixed;
    - the oracle lagging the Baseline or lacking a behaviour. It is recorded like a known upstream
      gap, with the scenario pinned to the older behaviour or named as undecided;
    - a genuine ambiguity in the specification. It goes upstream as an issue linked from its row.

**Out of scope:**

- whether a passing interop run gates a release;
- a second, non-blocking job against `opamp-go`'s `main`;
- the oracle on connection settings, packages, own telemetry and Gateway Mode.

## Alternatives considered

- **Record the oracle's reach in `CONFORMANCE.md` alone.** The matrix would be current. But the
  binding record would say nothing about the endpoint as it ships, and nothing would oblige the job
  to reach it.
- **One ADR for the Baseline and another for the oracle.** Two records would govern one subject.
  One record is what a reader of a protocol change has to open.
- **Keep the oracle on plaintext only.** Cheaper, and the scenarios are protocol behaviour, not
  transport. But the hardened endpoint is the only one an installation runs. A TLS 1.3 or
  client-certificate incompatibility with the reference implementation would surface first in the
  field, on every host that runs an `opamp-go`-based agent.
- **Gate releases on a green interop run.** A defect upstream would then block a release of this
  project. The scheduled job and its triage keep the evidence without that coupling.
- **The matrix in `SPECIFICATION.md`.** The specification states the commitment and changes
  rarely. The matrix changes with nearly every protocol change.
- **Conformance only in code.** The code cannot lie about the bits it sets. But it is unreadable
  to an operator and cannot say *planned* or *not planned*.
- **Manual reconciliation, no check.** The discipline that erodes first.
- **Fail CI on divergence.** CI would go red when upstream tags a release with nothing wrong here,
  and the reflex fix is to disable the check.
- **Track upstream `main`.** No fixed contract to test against: any commit can move behaviour
  under the implementation. The same holds for the oracle. A red run against an unreleased
  `opamp-go` can be our bug, theirs, or their refactor.
- **Fetch the schema at build time.** It makes the wire contract a function of network state and
  hides schema changes from review.
- **System `protoc` with plain `prost-build`.** It adds a system package to the Dev Container and
  three CI operating systems for what `protox` does inside the cargo build.
- **`rust-protobuf` instead of `prost`.** `prost` is the ecosystem default (tonic, OpenTelemetry
  Rust), and its varint codec doubles for the framing header.
- **An existing OpAMP Rust crate.** It would pin the project to *its* schema revision and
  capability subset.
- **Committing the generated `.rs`.** It drifts from the vendored schema unless a check
  regenerates it anyway.
- **Vendoring `opamp-go`.** The schema is vendored because it is a small build input. A Go library
  is a large test fixture, and a module pin gives the same reproducibility.
- **A harness written entirely in Go.** It cannot assert on our side's state: the fleet view, a
  Supervisor's status, what the Client wrote to disk.
- **The Collector as the first oracle.** It drags in a distribution, its configuration and its
  release cadence, and gives a failure three plausible homes.
- **Relying on the existing end-to-end suite.** It is structurally incapable of catching a
  misreading both peers share.

## Sources / Prior art

- [`open-telemetry/opamp-spec`](https://github.com/open-telemetry/opamp-spec): release history and
  Beta status. Its `opamp.proto` is the source of capability bits and maturity markers. Its
  [`specification.md`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  is the source of the MUST/SHOULD/MAY rules and of the WebSocket framing (varint header `0`, then
  the protobuf body).
- [`protox`](https://crates.io/crates/protox): pure-Rust protobuf compilation for `prost`
  (`protox 0.10` pairs with `prost 0.14`). [`prost`](https://docs.rs/prost): generated types and
  `encoding::{encode_varint, decode_varint}`.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go): the reference implementation,
  `client`, `server`, `protobufs`.
  - Its release notes give each release's `opamp-spec` version:
    [`v0.24.0`](https://github.com/open-telemetry/opamp-go/releases/tag/v0.24.0) (2026-09-08)
    implements `v0.20.0`, and
    [`v0.25.0`](https://github.com/open-telemetry/opamp-go/releases/tag/v0.25.0) (2026-09-29) is
    the release pinned in `interop/go.mod`.
  - Its `StartSettings.TLSConfig` takes a Go `tls.Config` on both the Client and the Server. That
    carries a client certificate and a required-client-certificate policy.
  - Its plain-HTTP Client's `Stop` sends no `agent_disconnect` (`client/httpclient.go`).
- Neither `opamp-go` nor `opamp-spec` ships a conformance suite for third-party implementations.
  That is why clause 13 bounds the job to a named list.
-
  [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension):
  a widely used client implementing a deliberately small subset, discoverable only from its
  documentation.

## Consequences

- Positive: "which OpAMP does this speak" has a written answer. The maturity column makes the risk
  of building on a `[Development]` feature visible at the point of decision.
- Positive: builds are offline and reproducible on all three Client platforms with no system
  protobuf compiler. Every schema change is a reviewable diff, and an upstream relocation is a new
  directory plus a constant.
- Positive: the oracle is what the ecosystem runs. Passing it speaks for `opampextension`,
  `opampsupervisor` and agents built on `opamp-go`, and, with clause 14, on the endpoint those
  agents actually meet.
- Negative / trade-offs: `CONFORMANCE.md` is maintained by hand. Apart from the scenarios the
  oracle reaches, nothing verifies that a row claimed *implemented* is implemented.
- Negative / trade-offs: the currency check needs network and degrades quietly. So it does nothing
  in a sandboxed CI.
- Negative / trade-offs: `protox` joins the trust base for the wire contract as a build
  dependency, and code generation costs a few seconds of first build.
- Negative / trade-offs: a second toolchain enters the project's life, and the TLS runs need a
  test PKI in the harness. A scheduled job nobody reads is decoration. Clause 17 is what makes it
  worth having.
- Follow-ups: a non-blocking job against `opamp-go`'s `main` if its releases stay frozen; the
  Collector as a second oracle; generating the matrix from the code.

## Enforcement

- [`scripts/check-protocol-baseline.sh`](../../scripts/check-protocol-baseline.sh) fails on a
  missing Baseline marker. It warns on divergence from upstream's newest release (clauses 1, 4).
  [`scripts/test-check-protocol-baseline.sh`](../../scripts/test-check-protocol-baseline.sh)
  drives it against fixtures.
- [`checks.yml`](../../.github/workflows/checks.yml) and [`ci.yml`](../../.github/workflows/ci.yml)
  build the workspace on Linux, Windows and macOS. Neither they nor the Dev Container install
  `protoc`. Every build compiles the generated types from the vendored files through `BASELINE`
  (clauses 5 to 7).
- `crates/opamp/src/frame.rs` holds the framing: `round_trips_a_message`,
  `rejects_a_non_zero_header`, `rejects_a_truncated_header` (clause 8).
- `crates/fleet-agent/tests/interop_opamp_go.rs`, run by
  [`interop.yml`](../../.github/workflows/interop.yml), cites this ADR on every scenario (clauses
  9, 10, 13 to 16):
  - `opamp_go_client_against_our_server_over_websocket` and
    `opamp_go_client_against_our_server_over_plain_http`;
  - `our_client_against_opamp_go_server_over_websocket` and
    `our_client_against_opamp_go_server_over_plain_http`;
  - `opamp_go_client_against_our_tls_and_admission` and
    `our_client_against_opamp_go_server_requiring_a_client_certificate` (clause 14).

**Not mechanically decidable:** review must judge these when a protocol change, a move of the
Baseline or a move of the oracle lands:

- that the matrix tells the truth (clauses 2, 3, 12);
- that the Baseline and the oracle pin are releases, and the oracle's pin the newest (clauses 1,
  11);
- that a red interop run is triaged (clause 17).
