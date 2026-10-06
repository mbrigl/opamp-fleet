# ADR-0004: Pin the protocol to a Baseline version, track conformance in a dedicated document, and prove it against `opamp-go`

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

The [specification](../SPECIFICATION.md) commits to implementing OpAMP *"in full and in step with
upstream"* (goals 12 and 13). Two properties of the protocol make that commitment impossible to keep
by good intentions alone.

**OpAMP is a moving target.** The upstream specification is itself at status **Beta** and is released
regularly — `v0.14.0` through `v0.18.0` all landed between August 2025 and May 2026. An
implementation that tracks "whatever is on `main`" has no stable contract to test against and no way
to state what it supports; one that silently stays on an old version drifts without anyone noticing.

**The protocol is not uniformly mature.** Individual features carry their own markers. Of the 16
`AgentCapabilities` bits, five are effectively stable, eight are marked `[Beta]`, and three —
`ReportsHeartbeat`, `ReportsAvailableComponents`, and `ReportsConnectionSettingsStatus` — are
marked `[Development]`, meaning they may still change shape; the custom-message facilities
(`CustomCapabilities`/`CustomMessage`) carry `[Development]` too but live outside the capability
bitmask, negotiated through their own message fields. Only two
capabilities across both ends are genuinely required: `ReportsStatus` on the Agent side and
`AcceptsStatus` on the Server side. Everything else is optional, and the protocol forbids either side
from assuming a capability the other has not declared.

The consequence is that "we implement OpAMP" is not a statement anyone can act on. An operator
pairing this Server with a third-party agent, or this Client with a third-party server, needs to know
*which* capabilities are live, *how mature* each one is upstream, and *where the gaps are*. That
information exists only if it is written down and kept honest.

**A written record is not yet evidence.** [`CONFORMANCE.md`](../CONFORMANCE.md) is a careful, honest
self-assessment — every row was written by reading the Baseline and then reading this code — and no
line of it has ever been checked against anything but itself.

**The two ends share their reading, so testing them against each other cannot find a misreading.**
The Client and the Server are tested together end to end, over both transports, through Gateway
Mode and Supervisor Mode. But both depend on `crates/opamp`, both were written from the same
sentences by the same author, and a wrong interpretation of a MUST is therefore *symmetric*: it
produces two peers that agree perfectly and are both wrong. Every one of the 40-odd `implemented`
rows rests on that symmetry. This is the one class of defect the existing suite cannot reach by
construction, and it is exactly the class that matters for a protocol whose value is that other
people's agents can speak it.

**The oracle is behind us, and that is the crux.** `opamp-go` is the reference implementation — the
OpenTelemetry Collector's `opampextension` and `opampsupervisor` are built on it, and so is
Bindplane — but its newest **release**, `v0.23.0` of 2026-02-18, implements opamp-spec **`v0.16.0`**.
This project's Baseline is **`v0.19.0`**. Three specification versions separate them. Its `main` has
since taken two of the changes that gap contains — transport message size limits (2026-06-18) and
the proto folder restructure (2026-07-16) — but no release has been cut in nearly six months. So the
oracle either lags the Baseline by three versions or is an unreleased moving target, and whichever
is chosen, some of what this project implements is beyond what any `opamp-go` can check.

**Two local constraints shape the harness.** [ADR-0002](0002-dev-container-runtime.md) gives the Dev
Container *"no access to the host daemon"*, so `opamp-go`'s `docker-compose` examples are not
available here and the harness must build and run plain binaries. And that same ADR keeps the base
image free of any language toolchain — there is no Go in the container, and putting one there
is a change to a decision made deliberately.

## Decision

### The Baseline and the conformance record

1. **We pin a Protocol Baseline** — one named upstream `opamp-spec` version that all code is
   written against — and record it, together with a per-capability conformance matrix, in a dedicated
   [`docs/CONFORMANCE.md`](../CONFORMANCE.md); a check in
   [`scripts/check-docs.sh`](../../scripts/check-docs.sh) compares the pinned version against the latest
   upstream release and **warns** on divergence.
2. **The Baseline is a version, not a branch.** Code targets a specific released tag. Moving to a newer
   one is a deliberate change that includes reading the upstream changelog and updating the matrix.
3. **Every capability has a recorded status**, on both ends: implemented, planned, or not planned —
   alongside its upstream maturity (stable / Beta / Development) and whether the protocol requires it.
4. **The matrix is part of the change that changes behaviour.** Adding or altering protocol behaviour
   updates `CONFORMANCE.md` in the same change, exactly as ADR status changes update the ADR index.
5. **Divergence warns, it does not fail.** An upstream release is not a defect in this repository. The
   check must also be tolerant of having no network, so the rest of the documentation checks stay
   usable offline.

### Interoperability against `opamp-go`

6. **`opamp-go` is the oracle, and only it.** It is the reference implementation, it is what the
   Collector's own `opampextension` and `opampsupervisor` speak, and an agent this project manages in
   the field is far more likely to be built on it than on anything else. A second oracle would be
   more evidence and more maintenance; if one is ever wanted, the Collector itself carrying
   `opampextension` is the candidate, not another library.

7. **Both directions, because both ends make claims.** Our Server is driven by `opamp-go`'s Client,
   and our Client is pointed at `opamp-go`'s Server. Running only one direction would leave half the
   conformance matrix resting on the symmetry this decision exists to break.

8. **The pin is the newest `opamp-go` release, recorded beside the Protocol Baseline.** Today that is
   `v0.23.0`. A released version is reproducible, is what real deployments consume, and moves on
   someone else's deliberate act rather than on whatever landed on `main` this morning. It gets its
   own row in `CONFORMANCE.md` next to the Baseline, and moving it is a deliberate change like a
   Baseline bump — including re-reading what the new version brought.

9. **The gap is written down rather than papered over.** Pinning a `v0.16.0` oracle against a
   `v0.19.0` Baseline means four Baseline features are **outside the oracle's reach**: transport
   message size limits, the `AgentConfigFile.role` field, `ComponentHealth.attributes`, and
   `agent_disconnect` over plain HTTP. `CONFORMANCE.md` marks which rows the oracle covers and which
   it cannot, so "interop-tested" never silently reads as "all of it". Those four keep the tests they
   already have; what they lose is a second opinion, and the document says so.

10. **A named scenario list, not a certification suite.** The job proves the core loop and the rules
    most likely to be misread, each end to end and on both transports where the Baseline offers both:
    connect and report; `sequence_num` continuity and the `ReportFullState` recovery a gap triggers;
    the remote-config offer, its acknowledgement, and the hash gate that stops it repeating;
    capability negotiation in both directions; identity handling including a Server-assigned
    `AgentIdentification`; and `agent_disconnect` on shutdown. It is explicitly **not** an attempt to
    exercise every row of the matrix — that would be a conformance suite, which is upstream's job to
    define and not something to invent here.

11. **It runs like the service smoke test, because it has the same shape.** A `#[ignore]`d Rust test
    drives the scenarios, and a dedicated workflow runs it on a schedule and on demand — not on every
    push. `crates/client/tests/service_smoke.rs` and
    [`service-smoke.yml`](../../.github/workflows/service-smoke.yml) already established this for the
    one thing no in-process stand-in can prove, and the reasoning transfers unchanged: it needs a
    toolchain the ordinary build does not, it takes real time, *"a merge must not hang on it, and a
    flake must not read as a broken change"*. `cargo test --workspace` stays exactly as fast and as
    self-contained as it is today.

12. **The Go side is a pinned module, not vendored code.** A small Go program under `interop/` with
    its own `go.mod` pinning `opamp-go` and a checked-in `go.sum`; CI installs Go with
    `actions/setup-go` and builds it. Vendoring a Go tree into a Rust repository would double the
    review surface for no reproducibility this does not already have from the module pin, and
    `opamp-go`'s own examples are a separate module for the same reason.

13. **The Dev Container does not grow a Go toolchain.** ADR-0002 chose a base image with no language
    toolchain on purpose, and this job's home is CI. A developer who wants to run it locally installs
    Go themselves; the README's **Build, Test & Run** section says so, and says that nothing else in
    the repository needs it.

14. **A failure is triaged before it is a defect.** Three outcomes, and the job's documentation names
    them: **our bug**, which is the point of the exercise and is fixed; **the oracle lagging the
    Baseline**, which is expected given clause 8 and is recorded the way `CONFORMANCE.md` already
    records known upstream gaps, with the scenario pinned to the older behaviour or skipped by name;
    and **a genuine ambiguity in the specification**, which goes upstream as a spec issue and is
    linked from the row it concerns. Without this, the first red run with no owner gets muted, and a
    muted job is worse than no job because it still reads as evidence.

## Alternatives considered

- **Put the conformance matrix in `SPECIFICATION.md`.** Rejected. The specification is the
  constitution — a statement of intent that should change rarely and deliberately. The matrix changes
  with nearly every protocol-touching commit. Mixing the two would either make the specification
  churn or let the matrix go stale in a document nobody expects to be current. The specification
  states the *commitment*; `CONFORMANCE.md` carries the *evidence*.
- **Track conformance only in code (capability constants and their doc comments).** Rejected. It is
  the most drift-resistant option, since the code cannot lie about which bits it sets — but it is
  unreadable to an operator deciding whether to deploy, and it cannot express *planned* or *not
  planned*, which is most of the matrix's value early on. Worth revisiting as a *generator* for the
  matrix once the code exists.
- **Manual reconciliation with upstream, no automated check.** Rejected. It is precisely the
  discipline that erodes first. The whole point of goal 13 is that drift is *detected* rather than
  noticed by chance.
- **Fail CI on divergence instead of warning.** Rejected. CI would go red the moment upstream tags a
  release, with nothing wrong in this repository, and the reflex fix would be to disable the check.
- **Track `main` instead of pinning a release.** Rejected. There would be no fixed contract to test
  against, and any upstream commit could change behaviour under the implementation without a visible
  event.
- **Track `opamp-go`'s `main` instead of its newest release.** Tempting, and it would close most of
  clause 9's gap today: `main` has already taken message size limits and the proto restructure.
  Rejected as the standing pin — an unreleased branch changes under the job without anyone deciding
  that it should, so a red run could mean our bug, their bug, or their refactor, and the first two
  become indistinguishable from the third. The gap it would close is documented instead, and closes on
  its own when upstream releases.
- **Both: a gating job on the release and an informational one on `main`.** More coverage, and it was
  seriously considered because clause 9's four uncovered features are precisely the newest ones.
  Rejected for now on simplicity: two pins, two triage paths, and a non-blocking job nobody reads.
  It becomes the obvious answer if `opamp-go` stays unreleased for another six months, and is named
  in the follow-ups rather than built speculatively.
- **Run the interop job on every push.** Rejected for the reasons the service smoke test already
  settled: it needs a second toolchain, it starts processes and waits on them, and a merge should not
  hang on an oracle that is upstream's artifact rather than ours.
- **Vendor `opamp-go` into this repository**, the way the protobuf schema is vendored (ADR-0006).
  Rejected: the schema is vendored because it is a *build input* whose absence would make the build
  depend on the network at compile time, and it is small and stable. A Go library is neither — it is a
  test fixture, it is large, and a module pin with a `go.sum` gives the same reproducibility without
  putting someone else's source under review here.
- **Write the harness in Go entirely**, driving both sides from Go tests. Simpler to build, and it is
  how upstream would do it. Rejected: the scenarios then live outside the repository's own test
  vocabulary, and asserting on *our* side's state — the fleet view, a Supervisor's status, what the
  Client wrote to disk — is exactly what the Rust side can do and a Go harness cannot.
- **Test against the OpenTelemetry Collector carrying `opampextension`** instead of the library.
  Closer to the real world, and this project already relays that extension's reports through the
  Supervisor Endpoint (ADR-0011). Rejected as the *first* oracle: it drags in a Collector distribution,
  its configuration, and its release cadence, and a failure would have three plausible homes instead of
  two. It is the natural second step once the library-level job is trusted.
- **Do nothing, and rely on the existing end-to-end suite.** Rejected on the Context's argument: those
  tests are excellent at catching regressions and structurally incapable of catching a misreading,
  because both peers share it. The claim in goals 12 and 13 is about other people's agents, and only
  another implementation can speak for them.

## Sources / Prior art

- [`open-telemetry/opamp-spec`](https://github.com/open-telemetry/opamp-spec) — release history;
  `v0.18.0` (2026-05-20) is the initial Baseline. Overall specification status: **Beta**.
- [`opamp.proto` at `v0.18.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.18.0/proto/opamp.proto)
  — the authoritative source for capability bits and their `Status: [Beta]` / `Status: [Development]`
  markers; the matrix in [`CONFORMANCE.md`](../CONFORMANCE.md) is transcribed from it, not from
  memory.
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) —
  the MUST/SHOULD/MAY requirements that are *not* expressed as capability bits (transports, size
  limits, `instance_uid` handling), tracked as their own section of the matrix.
- [`opampextension` capabilities](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — a working example of the problem this decision addresses: a widely deployed OpAMP client
  implementing a deliberately small subset, which an operator can only discover by reading its
  documentation.
- The existing [`scripts/check-docs.sh`](../../scripts/check-docs.sh) — already enforces the ADR index
  and link integrity in pure bash with no added toolchain; the Baseline check follows that pattern
  rather than introducing a new one ([ADR-0002](0002-dev-container-runtime.md) keeps the container
  free of unnecessary dependencies).
- **[`opamp-go`](https://github.com/open-telemetry/opamp-go)** — the reference implementation, and
  the oracle chosen here. Structure checked directly: `client`, `server`, `protobufs`, and an
  `internal/examples` tree (a separate Go module) holding an example agent, an example server with an
  admin UI, and a supervisor, with Dockerfiles and a `docker-compose.yml` this project cannot use
  (ADR-0002). Its README still describes the repository as *"work-in-progress"*, which is worth
  keeping in view when a divergence is triaged under clause 14.
- **`opamp-go` release history**, read from the repository's own release notes: `v0.23.0`
  (2026-02-18) *"Update opamp-spec to v0.16.0"*, `v0.22.0` (2025-08-28) to `v0.14.0`, `v0.21.0`
  (2025-08-12) to `v0.13.0`, `v0.20.0` (2025-07-03) to `v0.12.0`. This is the measurement behind
  clauses 8 and 9 — the oracle's newest release trails this project's Baseline by three specification
  versions.
- **`opamp-go` `main` since that release** — *"Add transport message size limits"* (#570,
  2026-06-18) and *"Support new proto folder structure"* (#573, 2026-07-16) are the two `v0.19.0`
  changes it has taken, alongside *"Fix connection settings spec compliance issues"* (#547). Its most
  recent commit at the time of writing is 2026-08-05, so the branch is alive and the release is not:
  the fact that decides the first `opamp-go` alternative.
- **No upstream conformance suite exists.** Checked before choosing to write scenarios by hand:
  neither `opamp-go` nor [`opamp-spec`](https://github.com/open-telemetry/opamp-spec) ships a testbed
  or certification harness for third-party implementations. This is why clause 10 bounds the exercise
  to a named list instead of claiming coverage — there is no upstream definition of "conformant" to run.
- This repository: [ADR-0006](0006-proto-vendoring-and-codegen.md), whose generated types this
  testing exercises against the reference implementation; [ADR-0002](0002-dev-container-runtime.md), whose no-Docker and
  no-toolchain choices shape clauses 12 and 13; and `crates/client/tests/service_smoke.rs` with
  [`service-smoke.yml`](../../.github/workflows/service-smoke.yml), the precedent clause 11 follows.

## Consequences

- Positive: "which OpAMP does this speak" becomes a question with a written answer, for operators
  and for interoperability testing alike. The matrix doubles as the implementation work list while
  rows still read *planned*.
- Positive: the maturity column makes the risk of building on a `[Development]` feature explicit at
  the point of decision, rather than surfacing as breakage after an upstream release.
- Positive: the conformance matrix stops being unfalsified. Goals 12 and 13 gain evidence produced by
  someone else's code, which is the only kind that can contradict this project's own reading.
- Positive: it catches the failure the existing suite cannot — a MUST read the same wrong way at both
  ends. That defect would otherwise surface as a customer's Collector failing to enrol.
- Positive: the oracle is what the ecosystem actually runs, so passing it is close to a practical
  guarantee that `opampextension`, `opampsupervisor`, and Bindplane-built agents interoperate.
- Negative / trade-offs: a second document to keep current, which can drift from the code exactly
  like any hand-maintained record. The automated check covers only the pinned version, not the matrix
  rows. Generating the matrix from the code would close that gap.
- Negative / trade-offs: the Baseline check needs network access and therefore must degrade quietly,
  which means it can silently do nothing in a sandboxed CI environment.
- Negative / trade-offs: **the oracle cannot reach the newest quarter of what we implement**
  (clause 9). Message size limits, `role`, `ComponentHealth.attributes`, and plain-HTTP
  `agent_disconnect` are precisely the least-exercised parts of this project and precisely what a
  `v0.16.0` peer knows nothing about. The evidence is real but partial, and the document has to keep
  saying so or it becomes a false comfort.
- Negative / trade-offs: a second toolchain enters the project's life. Go is not in the Dev Container
  and will not be (clause 13), so this is a job most contributors never run locally, and one that can
  break for reasons — a Go release, a module checksum, upstream CI — that have nothing to do with any
  change here.
- Negative / trade-offs: a scheduled job that nobody watches is decoration. Clause 14's triage rule is
  what makes it worth having, and it depends on somebody reading a red run rather than muting it. That
  is a process cost, not a code cost, and it is the most likely way this decision fails.
- Negative / trade-offs: the scenario list will be argued about. It is deliberately narrower than the
  matrix, so there will always be a row someone expected it to cover; clause 10 draws the line where
  the cost of a scenario stops buying independent evidence.
- Follow-ups: how the definitions are obtained and compiled is decided in
  [ADR-0006](0006-proto-vendoring-and-codegen.md). [`CONFORMANCE.md`](../CONFORMANCE.md) records the
  details and the upgrade procedure.
- Follow-ups: whether the currency check runs on every push or on a schedule is a CI configuration
  question, not an architectural one.
- Follow-ups: a second, non-blocking job against `opamp-go`'s `main` if its release stays frozen, which
  would recover the four features of clause 9 at the cost of a moving target; the Collector carrying
  `opampextension` as a second oracle once this one is trusted; and whether a passing interop run should
  become a release gate rather than a nightly signal — which is a question about the release pipeline
  (ADR-0019) rather than about this decision.
