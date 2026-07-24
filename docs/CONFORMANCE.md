# OpAMP Conformance

> What this project implements of the OpAMP protocol, and how far it has got. The
> [specification](SPECIFICATION.md) commits to implementing the protocol **in full and in step with
> upstream** (goals 12 and 13); this document is the evidence for that claim. It is a **living
> document**: a change that adds, removes, or alters protocol behaviour updates the matrix in the
> same change.

## Protocol Baseline

The **Protocol Baseline** is the pinned upstream specification version this project implements
against. It is the single authoritative statement of "which OpAMP" this code speaks.

<!-- protocol-baseline: v0.20.0 -->

| | |
|---|---|
| **Baseline version** | `v0.20.0` |
| **Released upstream** | 2026-08-12 |
| **Upstream specification** | <https://github.com/open-telemetry/opamp-spec> |
| **Upstream status** | Beta — the protocol itself is not yet stable |
| **Last reconciled with upstream** | 2026-08-14 |

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
implemented yet; they are what a move past `v0.20.0` would have to take in.

| Upstream change | Effect on this project |
|---|---|
| *(none)* | At the last reconciliation `main` was `v0.20.0` exactly — no commit upstream is ahead of the Baseline. |

### What `v0.20.0` brought

The Baseline moved from `v0.19.0` to `v0.20.0` on 2026-08-14; upstream released it on 2026-08-12.

| Upstream change | Taken up as |
|---|---|
| **`AgentConfigFile` renamed to `AgentConfigObject`, empty map key allowed unconditionally** ([#385](https://github.com/open-telemetry/opamp-spec/pull/385)) | Adopted: the vendored schema renames the message and its use in `AgentConfigMap.config_map`, and the generated Rust type follows (`opamp::proto::AgentConfigObject`). A wire-compatible rename — the field numbers and the `config_map` shape are unchanged, so nothing on the wire moved. The empty-key clarification needed no code change: this project already keys the map by the Configuration name and never rejected an empty one. |

### What `v0.19.0` brought

The Baseline moved from `v0.18.0` to `v0.19.0` on 2026-08-04; this is what came with it and where
each item landed, so a reader can check the claim rather than take it.

| Upstream change | Taken up as |
|---|---|
| **Transport message size limits** ([#346](https://github.com/open-telemetry/opamp-spec/pull/346)) | Implemented on both ends and both transports — see [Message size limits](#message-size-limits). |
| **Proto folders restructured** ([#352](https://github.com/open-telemetry/opamp-spec/pull/352)) | Adopted: the vendored schema now lives at `crates/opamp/proto/v0.20.0/opamp/v1/`. Build inputs only — see [Where the schema lives](#where-the-schema-lives). |
| **`ComponentHealth.attributes`** ([#334](https://github.com/open-telemetry/opamp-spec/pull/334)) | Generated from the schema and relayed as part of the health message the Supervisor Endpoint folds upstream; this project sets none of its own. |
| **`agent_disconnect` recommended for plain HTTP** ([#353](https://github.com/open-telemetry/opamp-spec/pull/353)) | Already the behaviour: the Client sends `agent_disconnect` on shutdown over both transports. |
| **SDK service namespace identifying attribute** ([#381](https://github.com/open-telemetry/opamp-spec/pull/381)) | Documentation of the OpenTelemetry guidelines; no protocol obligation. |

### Where the schema lives

The `v0.19.0` relocation moved the definitions from `proto/` to `proto/opamp/v1/` while leaving the
protobuf package `opamp.proto.v1` — and therefore the wire format and every generated Rust type
path — untouched. Only the build inputs moved, and adopting it was the one-line change it was
prepared to be, because the path lives in exactly one place:

> **Keep the proto path in exactly one place** — [`crates/opamp/build.rs`](../crates/opamp/build.rs),
> which derives both the file path and the include path from `BASELINE`. Never hard-code a proto
> path anywhere else.

Two details are easy to get wrong when a relocation like this happens again. **Both** files move,
not just `opamp.proto` — relocating one and leaving the other behind fails at import resolution.
And the include root is the directory *above* the package path: the import reads
`opamp/v1/anyvalue.proto` and the file sits at `<root>/opamp/v1/anyvalue.proto`, so the two only
compose when the generator's include root is `<root>`. Pointing it at the directory holding the
files puts them in reach but leaves the import unresolvable.

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
- **Status** — where this project stands: **implemented**, **partial**, **planned**, or **not
  planned** (with a reason, listed under [Deviations](#deviations)).

Status values are deliberately coarse. A capability counts as *implemented* only when the code
declares the bit **and** honours the behaviour behind it end to end. Where the bit is declared but
part of the behaviour behind it is not honoured, the status is *partial* and the note says which
part — a declared capability the peer may rely on is the one place where "mostly" has to be
written down rather than rounded up.

## Agent capabilities

The Client declares these on behalf of each Agent it represents. Bit values are from
`AgentCapabilities` in the Baseline's `opamp.proto`.

| Capability | Bit | Maturity | Requirement | Status | Note |
|---|---|---|---|---|---|
| `ReportsStatus` | `0x0001` | stable | **required** | implemented | MUST be set by every Agent. |
| `AcceptsRemoteConfig` | `0x0002` | stable | optional | implemented | Core of the control loop (goal 1). |
| `ReportsEffectiveConfig` | `0x0004` | stable | optional | implemented | Core of the control loop (goal 2). |
| `AcceptsOtherConnectionSettings` | `0x0200` | Beta | optional | planned | Settings for non-OpAMP destinations. |
| `AcceptsRestartCommand` | `0x0400` | Beta | optional | implemented | Declared by Supervisor-backed Agents only — the self-Agent has no process to restart. Queued via `POST /api/v1/agents/{uid}/restart`, delivered as the Baseline's command-only message on both transports (pushed over WebSocket, on the next poll over plain HTTP). |
| `ReportsHealth` | `0x0800` | stable | optional | implemented | Core of the control loop (goal 2). `ComponentHealth.attributes`, new in `v0.19.0` (`[Development]`), is carried through from what a Managed Process reports; this project adds none of its own. |
| `ReportsAvailableComponents` | `0x4000` | Development | optional | implemented | Relayed from the Managed Process's `opampextension` through the Supervisor Endpoint; declared only once components are known. The hash rides full reports, the full map goes out on the Server's `ReportAvailableComponents` flag — which the Server sets while it only holds a hash. |

## Server capabilities

Bit values are from `ServerCapabilities` in the Baseline's `opamp.proto`.

| Capability | Bit | Maturity | Requirement | Status | Note |
|---|---|---|---|---|---|
| `AcceptsStatus` | `0x0001` | stable | **required** | implemented | MUST be set by every Server. |
| `OffersRemoteConfig` | `0x0002` | stable | optional | implemented | Core of the control loop (goal 1). `AgentConfigObject.role` (the field debuted in `v0.19.0` on `AgentConfigFile`, renamed with the message in `v0.20.0`) carries the optional `role` of the Configuration it was composed from (ADR-0025) — empty, and so unset on the wire, unless an operator set one. It is part of the hash that gates every push, so a role change reaches the fleet like any other edit; an empty role is hashed as nothing, which keeps every hash predating the decision exactly where it was. The Client writes a roled entry to the config directory like any other but leaves it out of what the Managed Process is configured with — the Collector plugin passes one `--config` per *unroled* entry, so `supplementary` content is there to be read by path and never handed over as configuration. |
| `AcceptsEffectiveConfig` | `0x0004` | stable | optional | implemented | Core of the control loop (goal 2). |

## Protocol behaviour beyond capabilities

Not everything the protocol requires is expressed as a capability bit. These items are tracked
separately because conformance depends on them just as much.

| Area | Requirement | Status | Note |
|---|---|---|---|
| WebSocket transport | Servers SHOULD accept it; Clients MAY choose either | implemented | Varint header followed by the Protobuf message (`opamp::frame`); both ends (ADR-0012). The Client uses it by default; the Server pushes config changes over it. |
| Plain HTTP transport | Servers SHOULD accept it; Clients MAY choose either | implemented | *"Server implementations SHOULD accept both plain HTTP connections and WebSocket connections. OpAMP Client implementations may choose to support either."* Both ends (ADR-0012). The Client polls, by default every 30 s, with an immediate follow-up after a config outcome. |
| Default endpoint | Port 4320, path `/v1/opamp` | implemented | Both defaults in place; address/endpoint configurable on both ends (ADR-0009). |
| gzip on HTTP | The Server MUST honour `Content-Encoding` | implemented | The Server accepts gzip and identity request bodies; a body that inflates past the message size limit is refused with `413`, so compression buys no memory. Response compression (a SHOULD) is not done yet. |
| `Content-Type` header | The Client MUST set `application/x-protobuf` on plain HTTP | implemented | The Client sets it; the Server requires it on POST (`415` otherwise) and takes a WebSocket upgrade as the other transport. |
| `instance_uid` | MUST be 16 bytes, SHOULD be UUID v7 | implemented | Generated as UUID v7, persisted across restarts (`opamp::uid`); the Server rejects other lengths with `bad_request`. |
| `sequence_num` | Incremented per `AgentToServer` | implemented | The Server detects gaps and requests full state. |
| Unchanged fields omitted | SHOULD be unset when unchanged | implemented | Routine Client polls carry identity and sequence number only; status fields are sent when they change, everything after (re)connect or on demand. |
| `ReportFullState` | The Agent MUST report full state when requested | implemented | The Client complies immediately; the Server sets the flag on sequence gaps and unknown Agents. |
| `agent_disconnect` | MUST be set in the final message; SHOULD be sent on plain HTTP too | implemented | The Client sends it on shutdown on both transports — which `v0.19.0` newly asks of the plain-HTTP transport, so the Server marks the Agent disconnected at once instead of after missed polls; the Server also marks it on abrupt WebSocket loss. |
| `AgentIdentification` | The Agent MUST adopt a new `instance_uid` | implemented | The Client adopts and persists the new identity. |
| `RequestInstanceUid` | Server-generated identity on request | implemented | The Server mints a UUID v7 and re-keys the Agent. The Client does not use the flag (it self-generates), which the protocol permits. |
| Connection multiplexing | Distinguish Agents by `instance_uid` | implemented | Both ends. The Server keys all state on `instance_uid` and serves n Agents over one WebSocket connection (tested). The Client carries one Agent per Supervisor over one shared connection, routed by `instance_uid` alone (ADR-0014, ADR-0017), and in **Gateway Mode** (ADR-0014) it folds many downstream Clients onto a pool of upstream connections — grown lazily to a configured cap, each Agent stuck to its connection by `instance_uid`, messages forwarded unchanged. Tested end to end: two downstream peers on two transports arrive at the Server as two Agents over one upstream connection. |
| Duplicate `instance_uid` | Detection and handling | implemented | The Server rekeys an identity that reports over a second live WebSocket connection: a fresh UUID v7 via `AgentIdentification`, which the Client adopts (the Baseline's SHOULD). Stateless plain-HTTP polling offers nothing to tell two pollers apart, so detection is WebSocket-only. |
| Duplicate WebSocket connections | Handling defined by the spec | implemented | The Client holds one connection by construction and sends `agent_disconnect` before a graceful reconnect. The Server tracks per-connection ownership: only the owning connection marks its Agents disconnected, so a stale socket never takes down an Agent another connection carries. |
| Undefined capability bits | MUST be zero | implemented | Both ends declare only defined bits (`opamp` generated enums). |
| Transport security | TLS on both transports | implemented | rustls on both ends (ADR-0012); `wss://` and `https://`, with an optional CA file on the Client that replaces the built-in roots for a private CA. Server-authenticated only — see the next row. |
| Custom messages | `CustomCapabilities` / `CustomMessage` exchange | planned | `[Development]`. Outside the capability bitmask: each side lists supported custom capabilities as reverse-FQDN strings; a `CustomMessage` for an unsupported capability can be ignored. |

### Message size limits

`v0.19.0` added four rules per transport, and they are not symmetric — two are MUSTs on receiving,
and on sending the Server carries a MUST where the Client carries a SHOULD. What each end does:

| Direction | Requirement | This project |
|---|---|---|
| Server receives, plain HTTP | MUST enforce, including after decompression; answer `413`, and the Client MUST NOT retry | Request bodies are capped before a handler sees them; a gzip body that inflates past the limit is refused the same way, both with `413`. |
| Server receives, WebSocket | MUST enforce after any extension decompression; SHOULD close with `1009` | The socket refuses to buffer past the limit, and the connection is closed with `1009 Message Too Big`. |
| Server sends | MUST NOT send an oversized message; SHOULD record it | A reply or push past the limit is dropped with a log line — never truncated, never shipped. On plain HTTP the exchange fails with `500` rather than carrying a body the Client would have to refuse. |
| Client receives | MUST enforce, including after decompression; discard and record | The WebSocket connection is capped and closed with `1009`; an HTTP response body is read incrementally and abandoned the moment it grows past the limit, so it is never buffered whole. |
| Client sends | SHOULD limit; if exceeded MUST NOT send, SHOULD record | A report past the limit is dropped with a log line; on plain HTTP the request is never made. |

The limit defaults to **64 MiB**, the value upstream recommends, and is configurable on both ends
through `max_message_size_bytes` (ADR-0009). Zero is rejected at startup: the Baseline knows no
"unlimited", so a limit that could carry nothing is a configuration error, not a way to switch the
rule off. The Supervisor Endpoint enforces the same limit as the Server it stands in for.


Goal 17 asks for three things: TLS on both ends, mutual TLS, and a Server that accepts only

## Deviations

Deliberate departures from the Baseline, each with a reason. A deviation is a recorded decision, not
a gap left unexplained — the specification's non-goal *"Forking or extending the protocol"* forbids
resolving one by inventing semantics of this project's own.

| Deviation | Reason |
|---|---|

## Status summary

The base control loop is implemented on both ends and on both transports (ADR-0009 through
ADR-0009): status reporting, remote configuration gated by the config hash, effective-configuration
and health reporting, identity handling (UUID v7, reassignment, server-generated identity), state
recovery via `ReportFullState`, disconnect handling, and TLS. Supervisor Mode (ADR-0017) puts real
processes behind that loop: each configured Supervisor is its own Agent multiplexed over the
Client's one connection, a received configuration restarts the Managed Process on the written
files and is acknowledged `APPLYING` → `APPLIED` only once the process survived the apply grace
(`apply_grace_secs`, default 3 s) — exiting within it reports `FAILED` — and every Supervisor serves
a loopback WebSocket Supervisor Endpoint that folds a Collector `opampextension`'s description,
enforce the protocol's new message size limits in both directions, on both transports and at the
