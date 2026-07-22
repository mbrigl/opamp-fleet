# OpAMP Conformance

> What this project implements of the OpAMP protocol, and how far it has got. The
> [specification](SPECIFICATION.md) commits to implementing the protocol **in full and in step with
> upstream** (goals 12 and 13); this document is the evidence for that claim. It is a **living
> document**: a change that adds, removes, or alters protocol behaviour updates the matrix in the
> same change.

## Protocol Baseline

The **Protocol Baseline** is the pinned upstream specification version this project implements
against. It is the single authoritative statement of "which OpAMP" this code speaks.

<!-- protocol-baseline: v0.18.0 -->

| | |
|---|---|
| **Baseline version** | `v0.18.0` |
| **Released upstream** | 2026-05-20 |
| **Upstream specification** | <https://github.com/open-telemetry/opamp-spec> |
| **Upstream status** | Beta — the protocol itself is not yet stable |
| **Last reconciled with upstream** | 2026-07-22 |

Moving the Baseline to a newer upstream version is a deliberate change — see
[Upgrading the Baseline](#upgrading-the-baseline) for what it obliges.
[`scripts/check-docs.sh`](../scripts/check-docs.sh) compares the pinned version above against the
latest upstream release and warns when they diverge, so falling behind is noticed rather than
discovered later.

Because upstream is itself **Beta**, individual features carry a maturity marker. This document
reproduces those markers rather than inventing its own.

### Known upstream changes since the Baseline

Recorded when the Baseline was last reconciled, so that a future bump is a review of a known list
rather than a rediscovery. These are **not** part of the Baseline and are deliberately not
implemented yet; they are what a move past `v0.18.0` would have to take in.

| Upstream change | Effect on this project |
|---|---|
| **Transport message size limits** ([#346](https://github.com/open-telemetry/opamp-spec/pull/346)) | New MUST on both ends: enforce a receive limit (64 MiB recommended), answer `HTTP 413` or close the WebSocket with `1009`. Absent from `v0.18.0` entirely. Adds a genuine conformance obligation. |
| **Proto folders restructured** ([#352](https://github.com/open-telemetry/opamp-spec/pull/352)) | Build-level only, but it breaks any hard-coded path. See [Preparing for the proto relocation](#preparing-for-the-proto-relocation). |
| **`ComponentHealth.attributes`** ([#334](https://github.com/open-telemetry/opamp-spec/pull/334)) | A new field on health reporting. |
| **`agent_disconnect` recommended for plain HTTP** ([#353](https://github.com/open-telemetry/opamp-spec/pull/353)) | Extends disconnect semantics to the HTTP transport. |

The remaining commits since `v0.18.0` are CI, dependency, and documentation changes with no bearing
on the protocol.

### Preparing for the proto relocation

The relocation is the one upstream change that touches the build rather than the wire, so it is worth
being ready for before it is adopted. What actually changes:

| | Baseline `v0.18.0` | Upstream `main` |
|---|---|---|
| Definitions | `proto/opamp.proto`, `proto/anyvalue.proto` | `proto/opamp/v1/opamp.proto`, `proto/opamp/v1/anyvalue.proto` |
| Import inside `opamp.proto` | `import "anyvalue.proto";` | `import "opamp/v1/anyvalue.proto";` |
| Protobuf package | `opamp.proto.v1` | `opamp.proto.v1` — **unchanged** |
| `go_package`, `csharp_namespace` | unchanged | unchanged |

The consequence is the reassuring one: because the **protobuf package name does not change**, neither
does the wire format, and generated Rust type paths are unaffected. Only *where the files live* and
*how they import each other* changes. Nothing about an implementation's behaviour has to change; only
its build inputs do.

Being prepared therefore means one rule, applied from the first line of protocol code:

> **Keep the proto path in exactly one place** — the build script or vendoring step that fetches and
> compiles the definitions — and derive both the file path and the include path from the Baseline
> version. Never hard-code `proto/opamp.proto` anywhere else.

Follow that and adopting the relocation is a single-line change; ignore it and the path spreads
through build scripts, vendored copies, and documentation.

Two details are easy to get wrong. **Both** files moved, not just `opamp.proto` — a step that
relocates one and leaves the other behind fails at import resolution. And the include root stays
`proto/`: the import reads `opamp/v1/anyvalue.proto` and the file sits at
`proto/opamp/v1/anyvalue.proto`, so the two only compose when the generator's include root is
`proto/`. Pointing it at `proto/opamp/v1/` instead puts the file in reach but leaves the import
path unresolvable.

## Upgrading the Baseline

Moving to a newer upstream version is a deliberate change, not a version-string edit. The procedure:

1. **Read the upstream changelog** between the current Baseline and the target, and update *Known
   upstream changes since the Baseline* to reflect the new gap.
2. **Re-derive the capability matrix** from the target's `opamp.proto` — bit values, and especially
   maturity markers, since a `[Development]` feature may have become `[Beta]` or changed shape.
3. **Re-check the behaviour table** against the target's `specification.md`. New MUSTs appear between
   releases: transport size limits arrived exactly this way.
4. **Adjust the code** for anything that moved, and record any gap under *Deviations* rather than
   leaving it silent.
5. **Update the marker and the reconciliation date** in [Protocol Baseline](#protocol-baseline) last,
   once the steps above actually hold.

The automated check in [`scripts/check-docs.sh`](../scripts/check-docs.sh) only tells you the Baseline
has fallen behind. It cannot tell you what that costs — that is what step 1 through 4 are for.

## How to read the matrix

- **Maturity** — the upstream marker for the feature, as written in the Baseline: **stable** (no
  marker upstream, but note that the protocol as a whole is still Beta), **Beta**, or
  **Development**. A Development feature may change shape in a future upstream release; implementing
  one is a deliberate acceptance of that risk.
- **Requirement** — whether the protocol mandates the capability. Only two are genuinely
  **required**: `ReportsStatus` on the Agent side (*"This bit MUST be set, since all Agents MUST
  report status"*) and `AcceptsStatus` on the Server side (*"This bit MUST be set, since all Server
  MUST be able to accept status reports"*), both stated in `opamp.proto`. Everything else is
  **optional**: a conforming implementation may omit it, and *"Interoperability of Partial
  Implementations"* obliges each side to **stop using** a capability once it learns the peer lacks
  it — so an undeclared capability must never be assumed, in either direction.
- **Status** — where this project stands: **implemented**, **planned**, or **not planned** (with a
  reason, listed under [Deviations](#deviations)).

Status values are deliberately coarse. A capability counts as *implemented* only when the code
declares the bit **and** honours the behaviour behind it end to end.

## Agent capabilities

The Client declares these on behalf of each Agent it represents. Bit values are from
`AgentCapabilities` in the Baseline's `opamp.proto`.

| Capability | Bit | Maturity | Requirement | Status | Note |
|---|---|---|---|---|---|
| `ReportsStatus` | `0x0001` | stable | **required** | planned | MUST be set by every Agent. |
| `AcceptsRemoteConfig` | `0x0002` | stable | optional | planned | Core of the control loop (goal 1). |
| `ReportsEffectiveConfig` | `0x0004` | stable | optional | planned | Core of the control loop (goal 2). |
| `AcceptsPackages` | `0x0008` | Beta | optional | planned | Software distribution (goal 10). |
| `ReportsPackageStatuses` | `0x0010` | Beta | optional | planned | Software distribution (goal 10). |
| `ReportsOwnTraces` | `0x0020` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `ReportsOwnMetrics` | `0x0040` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `ReportsOwnLogs` | `0x0080` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `AcceptsOpAMPConnectionSettings` | `0x0100` | Beta | optional | planned | Needed for Server-driven credential rotation (goal 17). |
| `AcceptsOtherConnectionSettings` | `0x0200` | Beta | optional | planned | Settings for non-OpAMP destinations. |
| `AcceptsRestartCommand` | `0x0400` | Beta | optional | planned | Server-initiated restart of a Managed Process. |
| `ReportsHealth` | `0x0800` | stable | optional | planned | Core of the control loop (goal 2). |
| `ReportsRemoteConfig` | `0x1000` | stable | optional | planned | Reports acceptance or rejection (goals 3 and 4). |
| `ReportsHeartbeat` | `0x2000` | Development | optional | planned | Liveness independent of message traffic. |
| `ReportsAvailableComponents` | `0x4000` | Development | optional | planned | Also reported by the Collector's `opampextension`. |
| `ReportsConnectionSettingsStatus` | `0x8000` | Development | optional | planned | Reports the outcome of a connection-settings offer. |

## Server capabilities

Bit values are from `ServerCapabilities` in the Baseline's `opamp.proto`.

| Capability | Bit | Maturity | Requirement | Status | Note |
|---|---|---|---|---|---|
| `AcceptsStatus` | `0x0001` | stable | **required** | planned | MUST be set by every Server. |
| `OffersRemoteConfig` | `0x0002` | stable | optional | planned | Core of the control loop (goal 1). |
| `AcceptsEffectiveConfig` | `0x0004` | stable | optional | planned | Core of the control loop (goal 2). |
| `OffersPackages` | `0x0008` | Beta | optional | planned | Software distribution (goal 10). |
| `AcceptsPackagesStatus` | `0x0010` | Beta | optional | planned | Software distribution (goal 10). |
| `OffersConnectionSettings` | `0x0020` | Beta | optional | planned | Server-driven credential rotation (goal 17). |
| `AcceptsConnectionSettingsRequest` | `0x0040` | Development | optional | planned | Agent-initiated certificate signing request flow. |

## Protocol behaviour beyond capabilities

Not everything the protocol requires is expressed as a capability bit. These items are tracked
separately because conformance depends on them just as much.

| Area | Requirement | Status | Note |
|---|---|---|---|
| WebSocket transport | Servers SHOULD accept it; Clients MAY choose either | planned | Varint header followed by the Protobuf message. |
| Plain HTTP transport | Servers SHOULD accept it; Clients MAY choose either | planned | *"Server implementations SHOULD accept both plain HTTP connections and WebSocket connections. OpAMP Client implementations may choose to support either."* The Client polls, by default every 30 s. |
| Default endpoint | Port 4320, path `/v1/opamp` | planned | Both MAY be configurable. |
| gzip on HTTP | The Server MUST honour `Content-Encoding` | planned | Must accept both compressed and uncompressed bodies. |
| `Content-Type` header | The Client MUST set `application/x-protobuf` on plain HTTP | planned | Doubles as the Server's transport detection: with the header it SHOULD treat the request as plain HTTP transport, without it as a WebSocket initiation. |
| `instance_uid` | MUST be 16 bytes, SHOULD be UUID v7 | planned | SHOULD be self-generated and stay unchanged for the process lifetime. The routing key across connection boundaries. |
| `sequence_num` | Incremented per `AgentToServer` | planned | |
| Unchanged fields omitted | SHOULD be unset when unchanged | planned | Applies to description, health, effective config, remote config status, package statuses. |
| `ReportFullState` | The Agent MUST report full state when requested | planned | `ServerToAgent.flags`; the recovery path for the row above — a Server that lost state MUST set this flag. |
| `agent_disconnect` | MUST be set in the final message | planned | |
| `AgentIdentification` | The Agent MUST adopt a new `instance_uid` | planned | The Server may reassign identity. |
| `RequestInstanceUid` | Server-generated identity on request | planned | `AgentToServer.flags`; an Agent MAY ask the Server for its `instance_uid` at first start, setting a temporary value and this flag. |
| Connection multiplexing | Distinguish Agents by `instance_uid` | planned | Required by goals 14 and 15; the Server must never key state on the connection. |
| Duplicate `instance_uid` | Detection and handling | planned | |
| Duplicate WebSocket connections | Handling defined by the spec | planned | |
| Undefined capability bits | MUST be zero | planned | |
| Authentication | HTTP auth methods MAY be used; `401` MUST be returned on failure | planned | `[Beta]`. Basic or Bearer, applied before the WebSocket upgrade. Underpins goal 17. |
| Capability negotiation | Each side MUST stop using capabilities the peer lacks | planned | *"Interoperability of Partial Implementations"* — binding in both directions. |
| Retrying, throttling, bad request | Defined error and backoff behaviour | planned | |
| Custom messages | `CustomCapabilities` / `CustomMessage` exchange | planned | `[Development]`. Outside the capability bitmask: each side lists supported custom capabilities as reverse-FQDN strings; a `CustomMessage` for an unsupported capability can be ignored. |

## Deviations

Deliberate departures from the Baseline, each with a reason. A deviation is a recorded decision, not
a gap left unexplained — the specification's non-goal *"Forking or extending the protocol"* forbids
resolving one by inventing semantics of this project's own.

| Deviation | Reason |
|---|---|
| *(none yet)* | No code exists yet, so nothing has diverged. |

## Status summary

No protocol code has been written yet; every row above reads *planned*. This document therefore
doubles as the implementation work list, and its first real revision comes with the first capability
that actually ships.
