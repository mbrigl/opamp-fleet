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
| `ReportsStatus` | `0x0001` | stable | **required** | implemented | MUST be set by every Agent. |
| `AcceptsRemoteConfig` | `0x0002` | stable | optional | implemented | Core of the control loop (goal 1). |
| `ReportsEffectiveConfig` | `0x0004` | stable | optional | implemented | Core of the control loop (goal 2). |
| `AcceptsPackages` | `0x0008` | Beta | optional | planned | Software distribution (goal 10). |
| `ReportsPackageStatuses` | `0x0010` | Beta | optional | planned | Software distribution (goal 10). |
| `ReportsOwnTraces` | `0x0020` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `ReportsOwnMetrics` | `0x0040` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `ReportsOwnLogs` | `0x0080` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `AcceptsOpAMPConnectionSettings` | `0x0100` | Beta | optional | planned | Needed for Server-driven credential rotation (goal 17). |
| `AcceptsOtherConnectionSettings` | `0x0200` | Beta | optional | planned | Settings for non-OpAMP destinations. |
| `AcceptsRestartCommand` | `0x0400` | Beta | optional | implemented | Declared by Supervisor-backed Agents only — the self-Agent has no process to restart. Queued via `POST /api/v1/agents/{uid}/restart`, delivered as the Baseline's command-only message on both transports (pushed over WebSocket, on the next poll over plain HTTP). |
| `ReportsHealth` | `0x0800` | stable | optional | implemented | Core of the control loop (goal 2). |
| `ReportsRemoteConfig` | `0x1000` | stable | optional | implemented | Reports acceptance or rejection (goals 3 and 4). |
| `ReportsHeartbeat` | `0x2000` | Development | optional | implemented | Routine report every `heartbeat_interval_secs` (default 30 s, the Baseline's SHOULD; `0` disables and undeclares the bit) on the WebSocket transport; on plain HTTP every poll is the periodic report. A Server-offered interval arrives with `AcceptsOpAMPConnectionSettings`. |
| `ReportsAvailableComponents` | `0x4000` | Development | optional | implemented | Relayed from the Managed Process's `opampextension` through the Supervisor Endpoint; declared only once components are known. The hash rides full reports, the full map goes out on the Server's `ReportAvailableComponents` flag — which the Server sets while it only holds a hash. |
| `ReportsConnectionSettingsStatus` | `0x8000` | Development | optional | planned | Reports the outcome of a connection-settings offer. |

## Server capabilities

Bit values are from `ServerCapabilities` in the Baseline's `opamp.proto`.

| Capability | Bit | Maturity | Requirement | Status | Note |
|---|---|---|---|---|---|
| `AcceptsStatus` | `0x0001` | stable | **required** | implemented | MUST be set by every Server. |
| `OffersRemoteConfig` | `0x0002` | stable | optional | implemented | Core of the control loop (goal 1). |
| `AcceptsEffectiveConfig` | `0x0004` | stable | optional | implemented | Core of the control loop (goal 2). |
| `OffersPackages` | `0x0008` | Beta | optional | planned | Software distribution (goal 10). |
| `AcceptsPackagesStatus` | `0x0010` | Beta | optional | planned | Software distribution (goal 10). |
| `OffersConnectionSettings` | `0x0020` | Beta | optional | planned | Server-driven credential rotation (goal 17). |
| `AcceptsConnectionSettingsRequest` | `0x0040` | Development | optional | planned | Agent-initiated certificate signing request flow. |

## Protocol behaviour beyond capabilities

Not everything the protocol requires is expressed as a capability bit. These items are tracked
separately because conformance depends on them just as much.

| Area | Requirement | Status | Note |
|---|---|---|---|
| WebSocket transport | Servers SHOULD accept it; Clients MAY choose either | implemented | Varint header followed by the Protobuf message (`opamp::frame`); both ends (ADR-0007). The Client uses it by default; the Server pushes config changes over it. |
| Plain HTTP transport | Servers SHOULD accept it; Clients MAY choose either | implemented | *"Server implementations SHOULD accept both plain HTTP connections and WebSocket connections. OpAMP Client implementations may choose to support either."* Both ends (ADR-0007). The Client polls, by default every 30 s, with an immediate follow-up after a config outcome. |
| Default endpoint | Port 4320, path `/v1/opamp` | implemented | Both defaults in place; address/endpoint configurable on both ends (ADR-0008). |
| gzip on HTTP | The Server MUST honour `Content-Encoding` | implemented | The Server accepts gzip and identity request bodies (decompression capped at the message size limit). Response compression (a SHOULD) is not done yet. |
| `Content-Type` header | The Client MUST set `application/x-protobuf` on plain HTTP | implemented | The Client sets it; the Server requires it on POST (`415` otherwise) and takes a WebSocket upgrade as the other transport. |
| `instance_uid` | MUST be 16 bytes, SHOULD be UUID v7 | implemented | Generated as UUID v7, persisted across restarts (`opamp::uid`); the Server rejects other lengths with `bad_request`. |
| `sequence_num` | Incremented per `AgentToServer` | implemented | The Server detects gaps and requests full state. |
| Unchanged fields omitted | SHOULD be unset when unchanged | implemented | Routine Client polls carry identity and sequence number only; status fields are sent when they change, everything after (re)connect or on demand. |
| `ReportFullState` | The Agent MUST report full state when requested | implemented | The Client complies immediately; the Server sets the flag on sequence gaps and unknown Agents. |
| `agent_disconnect` | MUST be set in the final message | implemented | The Client sends it on shutdown on both transports; the Server marks the Agent disconnected (also on abrupt WebSocket loss). |
| `AgentIdentification` | The Agent MUST adopt a new `instance_uid` | implemented | The Client adopts and persists the new identity. |
| `RequestInstanceUid` | Server-generated identity on request | implemented | The Server mints a UUID v7 and re-keys the Agent. The Client does not use the flag (it self-generates), which the protocol permits. |
| Connection multiplexing | Distinguish Agents by `instance_uid` | implemented | Both ends. The Server keys all state on `instance_uid` and serves n Agents over one WebSocket connection (tested). The Client carries one Agent per Supervisor over one shared connection, routed by `instance_uid` alone (ADR-0003, ADR-0011); connection pools larger than one arrive with Gateway Mode. |
| Duplicate `instance_uid` | Detection and handling | planned | |
| Duplicate WebSocket connections | Handling defined by the spec | planned | |
| Undefined capability bits | MUST be zero | implemented | Both ends declare only defined bits (`opamp` generated enums). |
| Authentication | HTTP auth methods MAY be used; `401` MUST be returned on failure | planned | `[Beta]`. Basic or Bearer, applied before the WebSocket upgrade. Underpins goal 17. |
| Capability negotiation | Each side MUST stop using capabilities the peer lacks | implemented | The Server offers configuration only to Agents declaring `AcceptsRemoteConfig`; the Client stops reporting effective config to a Server without `AcceptsEffectiveConfig`. |
| Retrying, throttling, bad request | Defined error and backoff behaviour | implemented | The Server answers malformed input with `BAD_REQUEST` error responses; the Client honours `UNAVAILABLE` retry hints and reconnects with capped exponential backoff. The Server does not yet emit throttling itself. |
| Custom messages | `CustomCapabilities` / `CustomMessage` exchange | planned | `[Development]`. Outside the capability bitmask: each side lists supported custom capabilities as reverse-FQDN strings; a `CustomMessage` for an unsupported capability can be ignored. |

## Deviations

Deliberate departures from the Baseline, each with a reason. A deviation is a recorded decision, not
a gap left unexplained — the specification's non-goal *"Forking or extending the protocol"* forbids
resolving one by inventing semantics of this project's own.

| Deviation | Reason |
|---|---|
| *(none yet)* | Nothing implemented diverges from the Baseline's MUSTs. Two SHOULDs are consciously not taken up yet and noted in the matrix: response compression on plain HTTP, and Server-side throttling. |

## Status summary

The base control loop is implemented on both ends and on both transports (ADR-0005 through
ADR-0008): status reporting, remote configuration gated by the config hash, effective-configuration
and health reporting, identity handling (UUID v7, reassignment, server-generated identity), state
recovery via `ReportFullState`, disconnect handling, and TLS. Supervisor Mode (ADR-0011) puts real
processes behind that loop: each configured Supervisor is its own Agent multiplexed over the
Client's one connection, a received configuration restarts the Managed Process on the written
files and is acknowledged `APPLYING` → `APPLIED`/`FAILED` by outcome, and every Supervisor serves
a loopback WebSocket Supervisor Endpoint that folds a Collector `opampextension`'s description,
health, and effective configuration into its Agent. Configuration targeting (ADR-0012) composes
each Agent's Remote configuration from the named Configurations whose Selectors match its
reported attributes — delivered as named `AgentConfigMap` entries, hash-gated per Agent, with the
whole model exposed through the OpenAPI-described REST API v1. Every remaining *planned* row —
packages, connection settings, own telemetry, restart command, heartbeats, available components,
custom messages, authentication, duplicate handling — is future work; the rows above double as that
work list.
