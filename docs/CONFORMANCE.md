# OpAMP Conformance

> What this project implements of the OpAMP protocol, and how far it has got. The
> [specification](SPECIFICATION.md) commits to implementing the protocol **in full and in step with
> upstream** (goals 12 and 13); this document is the evidence for that claim. It is a **living
> document**: a change that adds, removes, or alters protocol behaviour updates the matrix in the
> same change.

## Protocol Baseline

The **Protocol Baseline** is the pinned upstream specification version this project implements
against. It is the single authoritative statement of "which OpAMP" this code speaks.

<!-- protocol-baseline: v0.19.0 -->

| | |
|---|---|
| **Baseline version** | `v0.19.0` |
| **Released upstream** | 2026-08-03 |
| **Upstream specification** | <https://github.com/open-telemetry/opamp-spec> |
| **Upstream status** | Beta — the protocol itself is not yet stable |
| **Last reconciled with upstream** | 2026-08-04 |

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
implemented yet; they are what a move past `v0.19.0` would have to take in.

| Upstream change | Effect on this project |
|---|---|
| *(none)* | At the last reconciliation `main` was `v0.19.0` exactly — no commit upstream is ahead of the Baseline. |

### What `v0.19.0` brought

The Baseline moved from `v0.18.0` to `v0.19.0` on 2026-08-04; this is what came with it and where
each item landed, so a reader can check the claim rather than take it.

| Upstream change | Taken up as |
|---|---|
| **Transport message size limits** ([#346](https://github.com/open-telemetry/opamp-spec/pull/346)) | Implemented on both ends and both transports — see [Message size limits](#message-size-limits). |
| **Proto folders restructured** ([#352](https://github.com/open-telemetry/opamp-spec/pull/352)) | Adopted: the vendored schema now lives at `crates/opamp/proto/v0.19.0/opamp/v1/`. Build inputs only — see [Where the schema lives](#where-the-schema-lives). |
| **`ComponentHealth.attributes`** ([#334](https://github.com/open-telemetry/opamp-spec/pull/334)) | Generated from the schema and relayed as part of the health message the Supervisor Endpoint folds upstream; this project sets none of its own. |
| **`agent_disconnect` recommended for plain HTTP** ([#353](https://github.com/open-telemetry/opamp-spec/pull/353)) | Already the behaviour: the Client sends `agent_disconnect` on shutdown over both transports. |
| **`AgentConfigFile.role`** ([#350](https://github.com/open-telemetry/opamp-spec/pull/350)) | Field present on the wire, left unset. Carrying an operator-chosen role through the Configuration model would change the REST API contract — proposed in [ADR-0016](adr/0016-configuration-content-role.md), not implemented. |
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
| `AcceptsPackages` | `0x0008` | Beta | optional | implemented | Software distribution for Managed Processes (goal 10, ADR-0015). Declared by a Supervisor-backed Agent whose block sets `accepts_packages` — it consents, the Server chooses which artifact (ADR-0017); the offered artifact is streamed to disk, verified (content hash always, Ed25519 signature when a key is configured), unpacked when it is a `.tar.gz` or a `.7z` — the member named after the Managed Process's binary, since an upstream release ships an archive and nothing repacks it on the way (ADR-0018); an encrypted `.7z` opens with `[packages] archive_key`, which the Server never learns — swapped over that binary by the Supervisor, health-gated on `apply_grace_secs`, and rolled back if it will not stay up. Only a `TopLevel` package is installed: an `Addon` is what a Supervisor has no way to apply, so it is refused with `InstallFailed` rather than written over the binary it was meant to extend. The Client's own self-update (goal 11) is future work. |
| `ReportsPackageStatuses` | `0x0010` | Beta | optional | implemented | `Downloading` while the artifact is on the wire, `Installing` once the Supervisor applies it, then `Installed`/`InstallFailed` (ADR-0015), with the offered `all_packages_hash` echoed once terminal — which is what stops the Server re-offering. The `Downloading` reports repeat every 5 s and carry `PackageDownloadDetails` (percent, bytes per second), so a transfer of hundreds of megabytes stays distinguishable from a stuck install; both that status and the details are `[Development]` upstream. Survives restarts via the persisted installed-package record. |
| `ReportsOwnTraces` | `0x0020` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `ReportsOwnMetrics` | `0x0040` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `ReportsOwnLogs` | `0x0080` | Beta | optional | planned | Client's own telemetry to a Server-nominated destination. |
| `AcceptsOpAMPConnectionSettings` | `0x0100` | Beta | optional | implemented | Server-driven credential rotation (goal 17, ADR-0014). An offer is verified by actually connecting (the Baseline's MUST), persisted in the Client's state dir (overriding `client.toml`), then the connection switches — across transports if the offered endpoint demands it. An offered `heartbeat_interval_seconds` becomes the heartbeat (WebSocket) or polling interval (plain HTTP). |
| `AcceptsOtherConnectionSettings` | `0x0200` | Beta | optional | planned | Settings for non-OpAMP destinations. |
| `AcceptsRestartCommand` | `0x0400` | Beta | optional | implemented | Declared by Supervisor-backed Agents only — the self-Agent has no process to restart. Queued via `POST /api/v1/agents/{uid}/restart`, delivered as the Baseline's command-only message on both transports (pushed over WebSocket, on the next poll over plain HTTP). |
| `ReportsHealth` | `0x0800` | stable | optional | implemented | Core of the control loop (goal 2). `ComponentHealth.attributes`, new in `v0.19.0` (`[Development]`), is carried through from what a Managed Process reports; this project adds none of its own. |
| `ReportsRemoteConfig` | `0x1000` | stable | optional | implemented | Reports acceptance or rejection (goals 3 and 4). |
| `ReportsHeartbeat` | `0x2000` | Development | optional | implemented | Routine report every `heartbeat_interval_secs` (default 30 s, the Baseline's SHOULD; `0` disables and undeclares the bit) on the WebSocket transport; on plain HTTP every poll is the periodic report. A Server-offered interval (ADR-0014) overrides the configured one on both transports. |
| `ReportsAvailableComponents` | `0x4000` | Development | optional | implemented | Relayed from the Managed Process's `opampextension` through the Supervisor Endpoint; declared only once components are known. The hash rides full reports, the full map goes out on the Server's `ReportAvailableComponents` flag — which the Server sets while it only holds a hash. |
| `ReportsConnectionSettingsStatus` | `0x8000` | Development | optional | implemented | `APPLYING` on receipt, `APPLIED`/`FAILED` after verification, the hash echoed either way (ADR-0014) — which is what stops the Server re-offering. Survives restarts via the persisted settings. |

## Server capabilities

Bit values are from `ServerCapabilities` in the Baseline's `opamp.proto`.

| Capability | Bit | Maturity | Requirement | Status | Note |
|---|---|---|---|---|---|
| `AcceptsStatus` | `0x0001` | stable | **required** | implemented | MUST be set by every Server. |
| `OffersRemoteConfig` | `0x0002` | stable | optional | implemented | Core of the control loop (goal 1). `AgentConfigFile.role`, new in `v0.19.0`, is present on the wire but left unset — see [ADR-0016](adr/0016-configuration-content-role.md). |
| `AcceptsEffectiveConfig` | `0x0004` | stable | optional | implemented | Core of the control loop (goal 2). |
| `OffersPackages` | `0x0008` | Beta | optional | implemented | Declared only while a non-empty package store is armed (`packages_dir`, ADR-0015). Artifacts and metadata persist and are managed through the REST API — uploaded under `max_package_size_bytes` and served straight from disk, so neither the store nor a download holds a program in memory. The offer is composed **per Agent** from the packages whose Selector matches it, as the Baseline's "available on the Server for this Agent" describes (ADR-0017), and its `all_packages_hash` is computed over that same set — so the re-offer gate stays correct for a targeted rollout. The `download_url` is served from the same listener. |
| `AcceptsPackagesStatus` | `0x0010` | Beta | optional | implemented | The Server records each Agent's reported `PackageStatuses` and gates re-offering on the `server_provided_all_packages_hash` (ADR-0015). |
| `OffersConnectionSettings` | `0x0020` | Beta | optional | implemented | Declared only while `server.toml` carries a `[connection_offer]` — credential, heartbeat interval, and/or endpoint, compiled into one hash-gated `OpAMPConnectionSettings` offered to Agents declaring `AcceptsOpAMPConnectionSettings` whose reported hash differs (ADR-0014). A credential that `[auth]` would reject fails startup. |
| `AcceptsConnectionSettingsRequest` | `0x0040` | Development | optional | planned | Agent-initiated certificate signing request flow. |

## Protocol behaviour beyond capabilities

Not everything the protocol requires is expressed as a capability bit. These items are tracked
separately because conformance depends on them just as much.

| Area | Requirement | Status | Note |
|---|---|---|---|
| WebSocket transport | Servers SHOULD accept it; Clients MAY choose either | implemented | Varint header followed by the Protobuf message (`opamp::frame`); both ends (ADR-0007). The Client uses it by default; the Server pushes config changes over it. |
| Plain HTTP transport | Servers SHOULD accept it; Clients MAY choose either | implemented | *"Server implementations SHOULD accept both plain HTTP connections and WebSocket connections. OpAMP Client implementations may choose to support either."* Both ends (ADR-0007). The Client polls, by default every 30 s, with an immediate follow-up after a config outcome. |
| Default endpoint | Port 4320, path `/v1/opamp` | implemented | Both defaults in place; address/endpoint configurable on both ends (ADR-0008). |
| Message size limits | Both ends MUST enforce a receive limit and MUST NOT send past it; `413` on HTTP, close `1009` on WebSocket | implemented | New in `v0.19.0` — see [Message size limits](#message-size-limits). Default 64 MiB, the upstream recommendation; `max_message_size_bytes` in `server.toml` and `client.toml` tightens it (ADR-0008). |
| gzip on HTTP | The Server MUST honour `Content-Encoding` | implemented | The Server accepts gzip and identity request bodies; a body that inflates past the message size limit is refused with `413`, so compression buys no memory. Response compression (a SHOULD) is not done yet. |
| `Content-Type` header | The Client MUST set `application/x-protobuf` on plain HTTP | implemented | The Client sets it; the Server requires it on POST (`415` otherwise) and takes a WebSocket upgrade as the other transport. |
| `instance_uid` | MUST be 16 bytes, SHOULD be UUID v7 | implemented | Generated as UUID v7, persisted across restarts (`opamp::uid`); the Server rejects other lengths with `bad_request`. |
| `sequence_num` | Incremented per `AgentToServer` | implemented | The Server detects gaps and requests full state. |
| Unchanged fields omitted | SHOULD be unset when unchanged | implemented | Routine Client polls carry identity and sequence number only; status fields are sent when they change, everything after (re)connect or on demand. |
| `ReportFullState` | The Agent MUST report full state when requested | implemented | The Client complies immediately; the Server sets the flag on sequence gaps and unknown Agents. |
| `agent_disconnect` | MUST be set in the final message; SHOULD be sent on plain HTTP too | implemented | The Client sends it on shutdown on both transports — which `v0.19.0` newly asks of the plain-HTTP transport, so the Server marks the Agent disconnected at once instead of after missed polls; the Server also marks it on abrupt WebSocket loss. |
| `AgentIdentification` | The Agent MUST adopt a new `instance_uid` | implemented | The Client adopts and persists the new identity. |
| `RequestInstanceUid` | Server-generated identity on request | implemented | The Server mints a UUID v7 and re-keys the Agent. The Client does not use the flag (it self-generates), which the protocol permits. |
| Connection multiplexing | Distinguish Agents by `instance_uid` | implemented | Both ends. The Server keys all state on `instance_uid` and serves n Agents over one WebSocket connection (tested). The Client carries one Agent per Supervisor over one shared connection, routed by `instance_uid` alone (ADR-0003, ADR-0011); connection pools larger than one arrive with Gateway Mode. |
| Duplicate `instance_uid` | Detection and handling | implemented | The Server rekeys an identity that reports over a second live WebSocket connection: a fresh UUID v7 via `AgentIdentification`, which the Client adopts (the Baseline's SHOULD). Stateless plain-HTTP polling offers nothing to tell two pollers apart, so detection is WebSocket-only. |
| Duplicate WebSocket connections | Handling defined by the spec | implemented | The Client holds one connection by construction and sends `agent_disconnect` before a graceful reconnect. The Server tracks per-connection ownership: only the owning connection marks its Agents disconnected, so a stale socket never takes down an Agent another connection carries. |
| Undefined capability bits | MUST be zero | implemented | Both ends declare only defined bits (`opamp` generated enums). |
| Authentication | HTTP auth methods MAY be used; `401` MUST be returned on failure | implemented | `[Beta]`. The Server's optional `[auth]` section guards `/v1/opamp` (ADR-0013): Basic and Bearer accepted, checked on every plain-HTTP POST and before the WebSocket upgrade completes, `401` with a `WWW-Authenticate` challenge otherwise. The Client sends the header on both transports. Without `[auth]` the endpoint stays open. Underpins goal 17. |
| Capability negotiation | Each side MUST stop using capabilities the peer lacks | implemented | The Server offers configuration only to Agents declaring `AcceptsRemoteConfig`; the Client stops reporting effective config to a Server without `AcceptsEffectiveConfig`. |
| Retrying, throttling, bad request | Defined error and backoff behaviour | implemented | The Server answers malformed input with `BAD_REQUEST` error responses; the Client honours `UNAVAILABLE` retry hints and reconnects with capped exponential backoff. The Server does not yet emit throttling itself. |
| Interim status reporting | The Client MAY report progress while it downloads and installs | implemented | While an artifact downloads the Client reports `Downloading` with `PackageDownloadDetails` every 5 s, and reports the terminal status once done — the Baseline's "status reports allow the Server to stay informed" for processing that takes a long time. |
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
through `max_message_size_bytes` (ADR-0008). Zero is rejected at startup: the Baseline knows no
"unlimited", so a limit that could carry nothing is a configuration error, not a way to switch the
rule off. The Supervisor Endpoint enforces the same limit as the Server it stands in for.

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
files and is acknowledged `APPLYING` → `APPLIED` only once the process survived the apply grace
(`apply_grace_secs`, default 3 s) — exiting within it reports `FAILED` — and every Supervisor serves
a loopback WebSocket Supervisor Endpoint that folds a Collector `opampextension`'s description,
health, and effective configuration into its Agent. Configuration targeting (ADR-0012) composes
each Agent's Remote configuration from the named Configurations whose Selectors match its
reported attributes — delivered as named `AgentConfigMap` entries, hash-gated per Agent, with the
whole model exposed through the OpenAPI-described REST API v1. On top of that loop sit the
server-initiated restart command, periodic heartbeats on both transports, available-components
relaying from the Managed Process, and duplicate-`instance_uid` handling with per-connection
disconnect scoping. The OpAMP endpoint optionally requires Basic or Bearer authentication on both
transports (ADR-0013), and the Server rotates those credentials — plus heartbeat interval and
endpoint — fleet-wide through hash-gated connection-settings offers the Client verifies by
actually connecting, persists, and acknowledges (ADR-0014). Software distribution to Managed
Processes is in place (ADR-0015): the Server stores and offers packages, and each Supervisor
downloads the artifact, verifies it (content hash always, Ed25519 signature when a key is
configured), swaps it over its Managed Process's binary, health-gates it on the apply grace, and
rolls back a binary that will not stay up, reporting `Downloading` with its progress while the
artifact is still on the wire. With the move to Baseline `v0.19.0` both ends also
enforce the protocol's new message size limits in both directions, on both transports and at the
Supervisor Endpoint. Every remaining *planned* row — other/telemetry connection settings, the
certificate-request flow, own telemetry, custom messages, and the Client's own self-update
(goal 11) — is future work; the rows above double as that work list.
