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
| **`AgentConfigFile.role`** ([#350](https://github.com/open-telemetry/opamp-spec/pull/350)) | Implemented ([ADR-0016](adr/0016-configuration-content-role.md)): an optional `role` on the Configuration resource travels verbatim into every entry composed from it. |
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
| `AcceptsPackages` | `0x0008` | Beta | optional | implemented | Software distribution for Managed Processes (goal 10, ADR-0015). Declared by a Supervisor-backed Agent whose block names its program with a bare file name, which puts it in a directory this Client owns and is therefore the whole of its consent (ADR-0021); the Server chooses which artifact (ADR-0017); the offered artifact is streamed to disk, verified (content hash always, Ed25519 signature when a key is configured), unpacked when it is a `.tar.gz` or a `.7z` — the member named after the Managed Process's binary, since an upstream release ships an archive and nothing repacks it on the way (ADR-0018); an encrypted `.7z` opens with `[packages] archive_key`, which the Server never learns — swapped over that binary by the Supervisor inside `<supervisor_dir>/<name>/program/`, health-gated on `apply_grace_secs`, and rolled back if it will not stay up. A Managed Process named by an *absolute* path is the machine's, declares no package capability, and is never written to. One that is **more than a single file** — an executable plus the shared objects it loads — is delivered by naming `program_path` in its block (ADR-0023): the whole archive is then unpacked into `<supervisor_dir>/<name>/program/tree/`, the tree it replaced kept beside it and put back whole if the new one will not stay up. Since the archive then names paths, every member is validated before anything is written and one this Client will not write — a `..` or absolute path, a symbolic or hard link, more than 10 000 members, more than 2 GiB unpacked — refuses the whole archive. Only a `TopLevel` package is installed: an `Addon` is what a Supervisor has no way to apply, so it is refused with `InstallFailed` rather than written over the binary it was meant to extend. **The Client updates itself the same way** (goal 11, ADR-0020): it is always its own Agent, and with `[self_update]` in `client.toml` it accepts the one package that section names — anything else is refused, since a package with an empty Selector reaches every consenting Agent and one written over the Client takes the host out of reach. The artifact is staged as a new version *beside* the running one in the ADR-0010 layout, proved by running `opamp-fleet-client self-check` on it before the `current` pointer moves, and the process then exits asking the service manager for a restart. A marker in the state directory carries the outcome across that restart: the new version commits itself once it reaches the Server, and a version that will not stay up is rolled back to its predecessor after three attempts. |
| `ReportsPackageStatuses` | `0x0010` | Beta | optional | implemented | `Downloading` while the artifact is on the wire, `Installing` once the Supervisor applies it, then `Installed`/`InstallFailed` (ADR-0015), with the offered `all_packages_hash` echoed once terminal — which is what stops the Server re-offering. The `Downloading` reports repeat every 5 s and carry `PackageDownloadDetails` (percent, bytes per second), so a transfer of hundreds of megabytes stays distinguishable from a stuck install; both that status and the details are `[Development]` upstream. Survives restarts via the persisted installed-package record. |
| `ReportsOwnTraces` | `0x0020` | Beta | optional | implemented | One span per control-loop operation that already has a lifecycle — applying a remote configuration, installing a package, a self-update — so a failed rollout is one trace rather than a log hunt. Exported OTLP/HTTP with protobuf bodies to the destination the Server names in `ConnectionSettingsOffers.own_*` (ADR-0036) — never to one configured locally, which is what the capability means. Declared unconditionally: it states an ability, and gating it on a destination already in force would mean the Server could never make the first offer. A destination on plain `http://` beyond loopback is **refused and reported**, the Baseline's "MAY refuse" taken; so are its `tls` and `proxy` fields, for the reasons the OpAMP settings row gives. The OTLP Resource carries the Agent's identifying attributes, as the Baseline asks. |
| `ReportsOwnMetrics` | `0x0040` | Beta | optional | implemented | Process metrics per Agent, sampled every 30 s from the outside: the Client's own process for its own Agent, the Managed Process for a Supervisor-backed one, named after the (experimental) process semantic conventions. Exported OTLP/HTTP with protobuf bodies to the destination the Server names in `ConnectionSettingsOffers.own_*` (ADR-0036) — never to one configured locally, which is what the capability means. Declared unconditionally: it states an ability, and gating it on a destination already in force would mean the Server could never make the first offer. A destination on plain `http://` beyond loopback is **refused and reported**, the Baseline's "MAY refuse" taken; so are its `tls` and `proxy` fields, for the reasons the OpAMP settings row gives. The OTLP Resource carries the Agent's identifying attributes, as the Baseline asks. |
| `ReportsOwnLogs` | `0x0080` | Beta | optional | implemented | The Client's own `tracing` output, bridged to OTLP log records; stderr keeps everything it printed before. Exported OTLP/HTTP with protobuf bodies to the destination the Server names in `ConnectionSettingsOffers.own_*` (ADR-0036) — never to one configured locally, which is what the capability means. Declared unconditionally: it states an ability, and gating it on a destination already in force would mean the Server could never make the first offer. A destination on plain `http://` beyond loopback is **refused and reported**, the Baseline's "MAY refuse" taken; so are its `tls` and `proxy` fields, for the reasons the OpAMP settings row gives. The OTLP Resource carries the Agent's identifying attributes, as the Baseline asks. |
| `AcceptsOpAMPConnectionSettings` | `0x0100` | Beta | optional | partial | Server-driven credential rotation (goal 17, ADR-0014) and client-certificate rotation (ADR-0035). An offer is verified by actually connecting (the Baseline's MUST), persisted in the Client's state dir (overriding `client.toml`), then the connection switches — across transports if the offered endpoint demands it. An offered `heartbeat_interval_seconds` becomes the heartbeat (WebSocket) or polling interval (plain HTTP). An offered `certificate` is proved by connecting **with it** before it replaces the one in force, and is then presented on both transports. **Two fields are still not honoured**: `tls` and `proxy` (both `[Development]`) are dropped — but the offer is no longer acknowledged `APPLIED` when they are: the Client applies everything else and reports `FAILED` with an `error_message` naming what it dropped, so a Server learns of the gap instead of being told success. `TLSConnectionSettings` is refused on merit — most of what it can say weakens verification, and `insecure_skip_verify` would let a Server disarm the check that proves it is the Server (ADR-0035). |
| `AcceptsOtherConnectionSettings` | `0x0200` | Beta | optional | planned | Settings for non-OpAMP destinations. |
| `AcceptsRestartCommand` | `0x0400` | Beta | optional | implemented | Declared by Supervisor-backed Agents only — the self-Agent has no process to restart. Queued via `POST /api/v1/agents/{uid}/restart`, delivered as the Baseline's command-only message on both transports (pushed over WebSocket, on the next poll over plain HTTP). |
| `ReportsHealth` | `0x0800` | stable | optional | implemented | Core of the control loop (goal 2). `ComponentHealth.attributes`, new in `v0.19.0` (`[Development]`), is carried through from what a Managed Process reports; this project adds none of its own. |
| `ReportsRemoteConfig` | `0x1000` | stable | optional | implemented | Reports acceptance or rejection (goals 3 and 4). |
| `ReportsHeartbeat` | `0x2000` | Development | optional | implemented | Routine report every `heartbeat_interval_secs` (default 30 s, the Baseline's SHOULD; `0` disables and undeclares the bit) on the WebSocket transport; on plain HTTP every poll is the periodic report. A Server-offered interval (ADR-0014) overrides the configured one on both transports. Declaring this capability is also what lets the Server call the Agent **stale** when it goes quiet for three intervals (ADR-0038) — the capability is the promise that makes silence mean something. |
| `ReportsAvailableComponents` | `0x4000` | Development | optional | implemented | Relayed from the Managed Process's `opampextension` through the Supervisor Endpoint; declared only once components are known. The hash rides full reports, the full map goes out on the Server's `ReportAvailableComponents` flag — which the Server sets while it only holds a hash. |
| `ReportsConnectionSettingsStatus` | `0x8000` | Development | optional | implemented | `APPLYING` on receipt, `APPLIED`/`FAILED` after verification, the hash echoed either way (ADR-0014) — which is what stops the Server re-offering. Survives restarts via the persisted settings. |

## Server capabilities

Bit values are from `ServerCapabilities` in the Baseline's `opamp.proto`.

| Capability | Bit | Maturity | Requirement | Status | Note |
|---|---|---|---|---|---|
| `AcceptsStatus` | `0x0001` | stable | **required** | implemented | MUST be set by every Server. |
| `OffersRemoteConfig` | `0x0002` | stable | optional | implemented | Core of the control loop (goal 1). `AgentConfigObject.role` (the field debuted in `v0.19.0` on `AgentConfigFile`, renamed with the message in `v0.20.0`) carries the optional `role` of the Configuration it was composed from (ADR-0016) — empty, and so unset on the wire, unless an operator set one. It is part of the hash that gates every push, so a role change reaches the fleet like any other edit; an empty role is hashed as nothing, which keeps every hash predating the decision exactly where it was. The Client writes a roled entry to the config directory like any other but leaves it out of what the Managed Process is configured with — the Collector plugin passes one `--config` per *unroled* entry, so `supplementary` content is there to be read by path and never handed over as configuration. |
| `AcceptsEffectiveConfig` | `0x0004` | stable | optional | implemented | Core of the control loop (goal 2). |
| `OffersPackages` | `0x0008` | Beta | optional | implemented | Declared only while a non-empty package store is armed (`packages_dir`, ADR-0015). The store holds **Sets** (ADR-0052): each identified by *(name, Agent type, version)*, holding one entry per platform — an uploaded artifact or a reference (ADR-0018) — persisted and managed through the REST API, uploaded under `max_package_size_bytes` and served straight from disk, so neither the store nor a download holds a program in memory. The offer is composed **per Agent** from the Sets that *fit* it and are then *aimed* at it, as the Baseline's "available on the Server for this Agent" describes, and its `all_packages_hash` is computed over that same set. A Set the operator has not **published** takes part in none of this (ADR-0043, without exception under ADR-0052: saving never distributes, and a published Set's entries are immutable): releasing it through `PUT …/publication` is what makes it a candidate, and withholding one is the Server's own composition of a set the Baseline leaves to it. Fit is then two mandatory steps and runs first: a Set built for another **Agent type** is dropped — the type in its identity is matched against the `service.name` the Agent reports (ADR-0033, ADR-0034), and an Agent reporting none fits nothing — and then every Set without an entry for the Agent's **platform** (ADR-0031). What survives is aimed by Selector (ADR-0017) and resolved to at most one Set per name: most specific Selector wins, and among equally specific ones the greater version (ADR-0029's comparison); an unbreakable tie is offered to nobody and surfaced. An uploaded entry's `download_url` points at the Agent plane's listener — the one the Client is already connected to, which is why the path resolves against its own endpoint (ADR-0066); a **referenced** one carries the address, the operator's SHA-256, and any headers the source needs, verbatim — the Baseline's Download Server *"may be on the same host as the OpAMP Server or a different host"*. A rollback is a publication move (ADR-0052, superseding ADR-0019): retracting the newest version has the fleet offered the newest one still published — an ordinary offer naming an older artifact, which is exactly what the Baseline means by *"an upgrade or downgrade of a package that the Agent already has"*. |
| `AcceptsPackagesStatus` | `0x0010` | Beta | optional | implemented | The Server records each Agent's reported `PackageStatuses` and gates re-offering on the `server_provided_all_packages_hash` (ADR-0015). |
| `OffersConnectionSettings` | `0x0020` | Beta | optional | partial | Declared only while `server.toml` carries a `[connection_offer]` — credential, heartbeat interval, and/or endpoint, compiled into one hash-gated `OpAMPConnectionSettings` offered to Agents declaring `AcceptsOpAMPConnectionSettings` whose reported hash differs (ADR-0014). A credential that `[auth]` would reject fails startup. A `certificate` **is** offered, but only as the answer to a CSR (see the row below), never as a standing setting. `tls` and `proxy` are never offered: there is no configuration surface to put them in, which is the Server half of the gap the Agent row names. The same message carries the own-telemetry destinations of `[telemetry_offer]` (ADR-0036), offered per signal and only to Agents declaring that signal; the hash covers the whole offer, so an Agent acknowledges the message rather than any one part of it. |
| `AcceptsConnectionSettingsRequest` | `0x0040` | Development | optional | implemented | The Agent-initiated CSR flow (ADR-0035). Declared only while `server.toml` carries a `[client_ca]` — the certificate and key the Server signs with, and `validity_days` (default 90). An Agent sends `ConnectionSettingsRequest.opamp.certificate_request.csr`; the Server signs it as a local CA and returns the issued certificate in an ordinary `ConnectionSettingsOffers` under its own hash, so one message can hand over the certificate and whatever standing offer goes with it. The Agent's private key never leaves its host — the Server puts nothing in `private_key`. Admission is the approval: a CSR that arrives has already satisfied every proof the endpoint requires, which is what the Baseline's flow means by awaiting one. A request that does not parse, or one arriving at a Server with no `[client_ca]`, is answered with a `ServerErrorResponse` of type `BadRequest` — the Baseline's MUST. A CSR that *carries* an `instance_uid` is not checked against the sender's, which the Baseline makes a MUST for that case — see [Mutual TLS](#mutual-tls-and-the-two-fields-still-refused), and [`HARDENING.md`](HARDENING.md) for the measure that would close it. |

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
| Connection multiplexing | Distinguish Agents by `instance_uid` | implemented | Both ends. The Server keys all state on `instance_uid` and serves n Agents over one WebSocket connection (tested). The Client carries one Agent per Supervisor over one shared connection, routed by `instance_uid` alone (ADR-0003, ADR-0011), and in **Gateway Mode** (ADR-0037) it folds many downstream Clients onto a pool of upstream connections — grown lazily to a configured cap, each Agent stuck to its connection by `instance_uid`, messages forwarded unchanged. Tested end to end: two downstream peers on two transports arrive at the Server as two Agents over one upstream connection. |
| Duplicate `instance_uid` | Detection and handling | implemented | The Server rekeys an identity that reports over a second live WebSocket connection: a fresh UUID v7 via `AgentIdentification`, which the Client adopts (the Baseline's SHOULD). Stateless plain-HTTP polling offers nothing to tell two pollers apart, so detection is WebSocket-only. |
| Duplicate WebSocket connections | Handling defined by the spec | implemented | The Client holds one connection by construction and sends `agent_disconnect` before a graceful reconnect. The Server tracks per-connection ownership: only the owning connection marks its Agents disconnected, so a stale socket never takes down an Agent another connection carries. |
| Undefined capability bits | MUST be zero | implemented | Both ends declare only defined bits (`opamp` generated enums). |
| Authentication | HTTP auth methods MAY be used; `401` MUST be returned on failure | implemented | `[Beta]`. The Server's optional `[auth]` section guards `/v1/opamp` (ADR-0013): Basic and Bearer accepted, checked on every plain-HTTP POST and before the WebSocket upgrade completes, `401` with a `WWW-Authenticate` challenge otherwise. The Client sends the header on both transports. Without `[auth]` the endpoint stays open; the REST API and the UI are a separate plane with their own optional Basic credentials (`[rest.auth]`, ADR-0067), which the Baseline says nothing about. On WebSocket the check happens once, at that upgrade, so a rotated or withdrawn credential governs only the next connection — which the Baseline asks nothing about, and which is therefore carried as a hardening measure (H1) in [`HARDENING.md`](HARDENING.md) rather than as a conformance item. Underpins goal 17. |
| Transport security | TLS on both transports | implemented | rustls on both ends (ADR-0007); `wss://` and `https://`, with an optional CA file on the Client that replaces the built-in roots for a private CA. Server-authenticated only — see the next row. |
| Mutual TLS | Client certificates as peer proof | implemented | Both ends (ADR-0035). The Server verifies client certificates against `[tls] client_ca_file`; client authentication stays optional at the TLS layer — since ADR-0066 no longer because a browser shares the listener, but because the package download does, and a Client fetches an artifact presenting no certificate — and is required on `/v1/opamp`, where **every configured proof must succeed**: a credential when `[auth]` is set, a certificate when a client CA is, both when both are. The Client presents its certificate on both transports, from `client.toml`'s `[tls] cert_file`/`key_file` or, outranking it, the pair the Server issued into the state directory. A certificate proves fleet membership, never which Agent is speaking: the subject is not matched against `instance_uid`, which the Server may re-key at any time through `AgentIdentification`. |
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

### Mutual TLS and the two fields still refused

Goal 17 asks for three things: TLS on both ends, mutual TLS, and a Server that accepts only
authenticated Agent identities. **All three now hold** — rustls on both transports (ADR-0007), the
optional `[auth]` credential (ADR-0013), and client certificates the Server itself issues
(ADR-0035). What is left is smaller and deliberate, and this is where it is written down, because
none of it is a capability bit.

- **Two offer fields are not honoured.** `TLSConnectionSettings` and `ProxyConnectionSettings`, both
  `[Development]`, are dropped from an offer. The refusal is a decision rather than a gap:
  `insecure_skip_verify`, `ca_pem_contents`, and the rest are largely ways to weaken verification,
  and a Server able to command them could switch off the check that proves it is the Server — trust
  here is an operator's file, not a Server's instruction. There is nothing on the Client for `proxy`
  to configure. **The acknowledgement no longer lies about it**: the Client applies every field it
  does honour and then reports `FAILED` with an `error_message` naming what it dropped, so a Server
  offering either learns that it did not take. The hash is echoed either way, so this does not put
  the Server into a re-offer loop.
- **A certificate proves membership, not identity.** The Server does not require a peer
  certificate's subject to match the Agent's `instance_uid` or anything else it reports. Binding
  them would mean a certificate dies whenever the Server re-keys an Agent through
  `AgentIdentification` — an outage of the Server's own making. Authorization and multi-tenancy
  remain the specification's non-goal.
- **Mutual TLS is hop-by-hop.** Where Gateway Mode eventually stands between an Agent and the
  Server there will be two mutual-TLS connections, and the certificate the Server verifies is the
  Gateway's; the per-Agent proof that survives a terminating hop is the credential, forwarded
  unchanged (ADR-0003). A fleet that gateways therefore keeps `[auth]` configured. A fleet whose
  Clients connect directly can drop it and be certificate-only.
- **Revocation is not implemented.** Short validity plus renewal at two thirds of it is what bounds
  a certificate's reach; there is no CRL and no OCSP. Ejecting a host faster than its certificate
  expires means rotating the CA.

- **A CSR that names an `instance_uid` is not checked against its sender.** The Baseline: *"When the
  Server receives a CSR containing the instance_uid in CSR fields the Server MUST verify that the
  instance_uid field in AgentToServer message matches the instance_uid in the CSR fields"*, which it
  justifies as what *"prevents Agents impersonating other Agents"*. This Server performs no such
  check — `ClientCa::sign` treats the subject as descriptive and drops the request's SANs. That is a
  **different question** from the one the bullet above settles: refusing to bind the *issued*
  certificate to an `instance_uid` is right, because a re-key through `AgentIdentification` would
  then kill a certificate the Server itself issued, whereas the MUST is about rejecting a mismatched
  *request* — and nothing about re-keying argues against that. The MUST is conditional and nothing
  currently triggers it: this project's Client puts its configured name in the CSR's common name and
  no `instance_uid` anywhere, so no request this fleet produces carries one. A peer implementation
  that does would be signed unverified, and interoperability with other Clients is a stated target
  (ADR-0040). Closing it is measure **H3** in [`HARDENING.md`](HARDENING.md), where what has to be
  worked out is set out.

## Deviations

Deliberate departures from the Baseline, each with a reason. A deviation is a recorded decision, not
a gap left unexplained — the specification's non-goal *"Forking or extending the protocol"* forbids
resolving one by inventing semantics of this project's own.

| Deviation | Reason |
|---|---|
| *(none yet)* | Nothing implemented diverges from the Baseline's MUSTs. Two SHOULDs are consciously not taken up yet and noted in the matrix: response compression on plain HTTP, and Server-side throttling. One MUST is *conditional* and currently unmet rather than departed from — the `instance_uid` check on a CSR that carries one, which no Client in this project produces; it is stated under [Mutual TLS](#mutual-tls-and-the-two-fields-still-refused) and tracked as a hardening measure, not as a deviation. |

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
Supervisor Endpoint. The Client also updates **itself** over the same package delivery (goal 11,
ADR-0020): it is always its own Agent now, whether or not it supervises anything, and a named
package is staged beside the running version, proved by running it, and switched into by an exit
the service manager answers with a restart — with a marker carrying the outcome across that
restart and rolling back a version that will not stay up. Mutual TLS closes goal 17 (ADR-0035): the Server verifies client
certificates against a configured CA and requires one on the OpAMP endpoint *in addition to* any
credential, the Client presents one on both transports, and the Server issues them itself through
the Baseline's CSR flow — the Agent keeps its private key, sends a signing request, and receives the
certificate as an ordinary connection-settings offer it proves by connecting with. Own telemetry closes three more bits (ADR-0036): the Client
reports its own process metrics, its logs, and spans over the control loop's own lifecycles, OTLP
over HTTP to whatever destination the Server names — and refuses a cleartext one. Every remaining
*planned* row — other connection settings and custom messages — is future work; the rows above
double as that work list. Two rows still read *partial* rather than
*implemented*: both connection-settings capabilities honour the endpoint, credential, heartbeat, and
certificate an offer carries but not its `tls` and `proxy` fields — deliberately, and no longer
silently (see [Mutual TLS](#mutual-tls-and-the-two-fields-still-refused)).
