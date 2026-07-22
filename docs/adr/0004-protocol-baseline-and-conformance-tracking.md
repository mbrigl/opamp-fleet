# ADR-0004: Pin the protocol to a Baseline version and track conformance in a dedicated document

- **Status:** 🟢 accepted
- **Date:** 2026-07-22
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

The question this ADR settles is not *whether* to record it, but *where* it lives and *how* it stays
current.

## Decision

We will pin a **Protocol Baseline** — one named upstream `opamp-spec` version that all code is
written against — and record it, together with a per-capability conformance matrix, in a dedicated
[`docs/CONFORMANCE.md`](../CONFORMANCE.md); a check in
[`scripts/check-docs.sh`](../../scripts/check-docs.sh) compares the pinned version against the latest
upstream release and **warns** on divergence.

This binds four things:

- **The Baseline is a version, not a branch.** Code targets a specific released tag. Moving to a newer
  one is a deliberate change that includes reading the upstream changelog and updating the matrix.
- **Every capability has a recorded status**, on both ends: implemented, planned, or not planned —
  alongside its upstream maturity (stable / Beta / Development) and whether the protocol requires it.
- **The matrix is part of the change that changes behaviour.** Adding or altering protocol behaviour
  updates `CONFORMANCE.md` in the same change, exactly as ADR status changes update the ADR index.
- **Divergence warns, it does not fail.** An upstream release is not a defect in this repository. The
  check must also be tolerant of having no network, so the rest of the documentation checks stay
  usable offline.

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
  — a working example of the problem this ADR addresses: a widely deployed OpAMP client implementing
  a deliberately small subset, which an operator can only discover by reading its documentation.
- The existing [`scripts/check-docs.sh`](../../scripts/check-docs.sh) — already enforces the ADR index
  and link integrity in pure bash with no added toolchain; the Baseline check follows that pattern
  rather than introducing a new one ([ADR-0002](0002-dev-container-runtime.md) keeps the container
  free of unnecessary dependencies).

## Consequences

- Positive: "which OpAMP does this speak" becomes a question with a written answer, for operators
  and for interoperability testing alike. The matrix doubles as the implementation work list while
  every row still reads *planned*.
- Positive: the maturity column makes the risk of building on a `[Development]` feature explicit at
  the point of decision, rather than surfacing as breakage after an upstream release.
- Negative / trade-offs: a second document to keep current, which can drift from the code exactly
  like any hand-maintained record. Nothing yet verifies that a *claimed* implemented capability is
  actually implemented — the automated check covers only the pinned version, not the matrix rows.
  Generating the matrix from the code would close that gap and is worth doing once there is code.
- Negative / trade-offs: the Baseline check needs network access and therefore must degrade quietly,
  which means it can silently do nothing in a sandboxed CI environment.
- Follow-ups: how the definitions are obtained and compiled — vendored copy versus fetch-at-build,
  and which Rust protobuf toolchain generates them — is a real decision that needs its own ADR before
  the first protocol code lands. It is constrained by one thing already visible upstream: the proto
  files have been relocated on `main` from `proto/` to `proto/opamp/v1/`, with the import between
  them changing accordingly while the protobuf package name `opamp.proto.v1` stays put. So the wire
  format is unaffected and only the build inputs move — which means whatever that ADR decides must
  keep the proto path in exactly one place, derived from the Baseline version, rather than spread
  through build scripts and vendored copies. [`CONFORMANCE.md`](../CONFORMANCE.md) records the
  details and the upgrade procedure.
- Follow-ups: whether the currency check runs on every push or on a schedule is a CI configuration
  question, not an architectural one. Interoperability testing against `opamp-go` as the behavioural
  oracle is a separate decision.
