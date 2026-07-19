# ADR-0018: OpAMP package/binary distribution — the Server offers the Collector binary, the Supervisor downloads, hash-verifies, swaps, and rolls back

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The [specification](../SPECIFICATION.md) commits the project to distributing **software, not only
configuration**: *"The Server can deliver and apply agent binaries, not just their configuration —
verified before applying and rolled back on failure — but only as far as the managed agents actually
support it."* This is the last major piece of the vision that is entirely unbuilt. Three accepted ADRs
deferred it explicitly, each to "its own ADR":

- [ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md) notes the upstream supervisor
  implements remote configuration but **not** package/binary updates.
- [ADR-0006](0006-rust-opamp-server-from-spec.md) held OpAMP package delivery out of the initial minimal
  Server.
- [ADR-0008](0008-collector-supervisor-go-reference-compat.md) held **package/binary updates** out of the
  Collector Supervisor's scope. The Supervisor therefore deliberately does **not** declare
  `AcceptsPackages` (there is a test asserting the bit stays clear).

This ADR is that named follow-up for the Agent **and** the Server side together.

**What the protocol already gives us.** The vendored schema
([`opamp.proto`](../../crates/opamp-proto/proto/opamp/v1/opamp.proto)) already carries the full package
surface — `PackagesAvailable` / `PackageAvailable` / `DownloadableFile` (with `content_hash`,
`signature`, and `headers`), `PackageStatuses` / `PackageStatus` / `PackageStatusEnum`
(Installed / InstallPending / Installing / InstallFailed / Downloading), and the
`ServerCapabilities` bits `OffersPackages` / `AcceptsPackagesStatus`. No schema change is needed. The
spec's flow is: the Server sends `PackagesAvailable` **only** to an Agent that declares both
`AcceptsPackages` and `ReportsPackageStatuses`; the Agent downloads each file over HTTP GET, **verifies
its hash and signature**, installs it, and reports lifecycle in `PackageStatuses` carrying
`server_provided_all_packages_hash`. The Server drives the loop by comparing that hash to its own
`all_packages_hash` — the same "a difference is the signal" model as the config loop
([ADR-0006](0006-rust-opamp-server-from-spec.md)).

**Two forces shape the scope.**

1. **The spec makes signature verification a MUST.** The code-signing section states implementations
   **MUST verify the signature** of each downloadable file *before the file is executed or used*, and
   that the Agent **MUST authenticate package authenticity** to prevent executing malicious code. Full
   cryptographic signing (server-held private key, agent-pinned public key, a signing/verification
   method) is a self-contained sub-problem with its own dependency and key-distribution decisions.
2. **There is no behavioural oracle for this feature.** The Go reference Supervisor — the oracle
   [ADR-0008](0008-collector-supervisor-go-reference-compat.md) measures the Collector Supervisor
   against — does **not** implement package management (upstream issues #47272 and #33947 are open). So
   the correctness model of ADR-0008 ("reach the same state as the Go supervisor") **does not apply
   here**; correctness must be established another way. The third-party `bindplane-agent` sidecar does
   perform agent self-upgrades, but through its own non-OpAMP mechanism and is observation-only in this
   project ([ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md)).

Introducing a new protocol surface, a binary-download-and-swap persistence/apply path, and an HTTP
file-serving route is architecture-relevant on both ends — hence this ADR.

## Decision

We will **close the software-distribution loop for a single top-level package — the Collector binary —
end to end**, config-driven and defaulting to off, with **content-hash verification now and
cryptographic signature verification deferred to a named follow-up ADR**.

### Server side

- **A configured package set.** A new Server configuration section names the packages to offer: for each,
  a `name`, `version`, `PackageType` (only `TopLevel` now), and a local **file path** to the binary.
  With none configured, the Server offers no packages and behaves exactly as today.
- **Serve the binary over the existing `:4321` surface.** The Server exposes `GET /packages/{name}` from
  the configured files, over the same listener the REST API/UI use — so the download rides the **same
  TLS + shared-token channel** that [ADR-0012](0012-tls-and-shared-token-auth.md) already secures. The
  Server sets `DownloadableFile.headers` in the offer to carry the `Authorization: Bearer <token>` the
  download must present, and `DownloadableFile.download_url` to the `:4321` route. Range requests are a
  spec SHOULD, deferred (see below).
- **Hashes computed at load.** The Server computes each file's `content_hash` (SHA-256), the package
  `hash`, and the aggregate `all_packages_hash`, per the spec's aggregation
  (SHA-256 over sorted, concatenated component hashes).
- **Drive by hash comparison.** `build_reply` ([`server.rs`](../../crates/opamp-server/src/server.rs))
  includes `PackagesAvailable` when the Agent's reported `server_provided_all_packages_hash` differs from
  the Server's `all_packages_hash`, for an Agent that declares both package capability bits — mirroring
  the existing `remote_config` comparison. The Server declares `OffersPackages` and
  `AcceptsPackagesStatus`.

### Supervisor (Agent) side

- **Declare the capabilities we now implement.** The Collector Supervisor adds `AcceptsPackages` and
  `ReportsPackageStatuses` to its `CAPABILITIES`; the ADR-0008 test that asserted `AcceptsPackages` is
  clear is updated to assert it is now set.
- **Download → verify hash → stage.** On a `PackagesAvailable` whose top-level package differs from the
  installed one, the Supervisor reports `Downloading`, fetches `download_url` with the offered `headers`
  into a temp file under its storage dir, and **verifies `content_hash` (SHA-256) — a mismatch aborts the
  install as `InstallFailed`** without touching the running Collector.
- **Atomic swap + restart, reusing the ADR-0008 rollback.** The Supervisor reports `Installing`, makes
  the staged file executable, **atomically renames it into the Collector's binary path** (same
  filesystem), keeping the previous binary for rollback, and restarts the Collector. It then **waits
  (bounded) for the Collector to report healthy** over the local OpAMP server — the exact
  health-confirmation mechanism [ADR-0008](0008-collector-supervisor-go-reference-compat.md) built for
  `automatic_config_rollback`. On healthy → report `Installed` with `agent_has_version`/`agent_has_hash`.
  On not-healthy (or the process failing to start) → **restore the previous binary, restart, and report
  `InstallFailed`** with the Collector's error; the failed hash is remembered so a re-offer does not loop
  onto the same broken binary.
- **Persist installed package state.** The installed package `name`/`version`/`hash` is persisted next to
  the instance UID and config hash (ADR-0008), so a Supervisor restart resumes without re-installing and
  reports the correct `PackageStatuses` on reconnect.

### Verification scope now — and the honest gap

- **Content-hash only; signatures deferred.** The first increment verifies **integrity** (`content_hash`)
  but not cryptographic **authenticity** (`signature`). This does **not** fully satisfy the spec's
  code-signing MUST. We accept this **temporarily and explicitly**, exactly as
  [ADR-0006](0006-rust-opamp-server-from-spec.md)/[ADR-0007](0007-rest-api-and-fleet-ui.md) accepted the
  unauthenticated surface with a named follow-up, on these mitigations:
  - **Authenticity derives from the authenticated channel, not from nothing.** The binary is served only
    over the TLS + shared-token `:4321` surface (ADR-0012), and the offer carries the token in
    `headers`; a downloader that reaches the file has already proven it belongs to the fleet, and TLS
    authenticates the Server as the source.
  - **Integrity is mandatory.** A `content_hash` mismatch is a hard `InstallFailed`; no unverified bytes
    are ever executed.
  - **The gap is flagged, not hidden.** Until the signature follow-up lands, this capability MUST NOT be
    presented as code-signing-conformant, and running package distribution over the *insecure* dev
    transport MUST be treated as trusted-network-only — the same posture ADR-0012 already sets.
- **Cryptographic signature verification is the immediate follow-on ADR** (server signs; Supervisor
  verifies `DownloadableFile.signature` against a pinned public key in `supervisors.yaml`, before the
  binary is made executable). That ADR closes the MUST.

### Dependencies (justified here)

- **Supervisor:** an HTTP client for the download. We reuse the **`reqwest`** stack over the `rustls`
  already pulled in by [ADR-0012](0012-tls-and-shared-token-auth.md) (no new TLS backend), plus
  **`sha2`** for `content_hash`. No streaming-resume/range support now.
- **Server:** **`sha2`** for the hash computations and a static-file handler on the existing `axum`
  router — no new framework.

### Scope held out (each its own follow-up)

Cryptographic **signatures**; **addon** packages (only `TopLevel` now); updating the **Foreign Agent**
binary via the Custom Supervisor ([ADR-0009](0009-plugin-hexagonal-supervisor-host.md)); **resumable /
range** downloads; and Server-side per-agent package bookkeeping (the Agent already de-duplicates via the
installed-hash check, so the Server may re-offer safely, as with ADR-0011's offers).

## Alternatives considered

- **Full cryptographic signature verification now.** The spec-complete path, but it bundles a crypto
  dependency, a signing tool, and key distribution into the first increment. Deferred to a named
  follow-on so the download/verify/swap/rollback loop lands first and the signing decision gets its own
  focused ADR — the project's established "close the loop before widening it" phasing (ADR-0006 → 0012).
  **Chosen against for now**, with the honesty mitigations above.
- **Skip hashing too and trust the channel entirely.** Rejected: integrity verification is cheap,
  catches corruption independent of transport, and the proto provides `content_hash` for exactly this;
  executing unverified bytes is indefensible.
- **Target the Supervisor's own binary (self-update).** Rejected: OpAMP packages describe the *managed
  agent's* software; the Collector binary the Supervisor launches (ADR-0008) is the natural top-level
  package. Supervisor self-update is a different problem (a running process replacing itself) with no
  present need.
- **Also update the Foreign Agent binary now.** Rejected for this increment: two apply paths at once
  overloads the first loop; the Custom Supervisor's binary swap is a clean follow-up once the Collector
  path holds.
- **Embed the binary in the OpAMP message instead of an HTTP download.** Rejected: the spec models
  packages as `DownloadableFile` over HTTP GET precisely to keep large blobs off the control channel; the
  vendored `download_url` is the intended path.
- **Point `download_url` at an external artifact store.** Deferred: serving from a local packages dir over
  the Server's own authenticated surface mirrors how config is distributed from a local file
  (ADR-0007/[ADR-0014](0014-local-config-files-in-composition.md)) and keeps the dev loop self-contained.
  An external store is a later option, not a first requirement.
- **Addons / multiple packages.** YAGNI: one top-level package exercises the whole loop; the map-based
  proto already accommodates more when a need appears.

## Sources / Prior art

- OpAMP specification — Package Management (download/install flow, `PackagesAvailable` /
  `PackageStatuses`, `all_packages_hash` aggregation) and the **Code Signing** security section
  (the MUST-verify requirement): <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- The vendored schema this builds on — `PackagesAvailable`, `PackageAvailable`, `DownloadableFile`,
  `PackageStatuses`, `PackageStatusEnum`, `ServerCapabilities` in
  [`crates/opamp-proto/proto/opamp/v1/opamp.proto`](../../crates/opamp-proto/proto/opamp/v1/opamp.proto).
- **No oracle:** the Go reference Supervisor does not implement package management — upstream issues
  [#47272](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/47272) (AcceptsPackages /
  ReportsPackageStatuses) and [#33947](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/33947)
  (updates the Collector binary) are open; see the supervisor README capability table
  (<https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>).
- The mechanisms this reuses: [ADR-0008](0008-collector-supervisor-go-reference-compat.md) (health
  confirmation + automatic rollback, storage/persistence, local OpAMP server), the config-loop hash
  comparison [ADR-0006](0006-rust-opamp-server-from-spec.md), the offer-drives-on-difference model
  [ADR-0011](0011-server-agent-control-beyond-config.md), and the TLS + token channel
  [ADR-0012](0012-tls-and-shared-token-auth.md).

## Consequences

- Positive: the specification's **software-distribution** goal moves from unbuilt to a working,
  end-to-end loop — the Server delivers a Collector binary, verified before applying and **rolled back on
  failure**, reported as OpAMP `PackageStatuses` in the fleet. Two dead Agent capabilities
  (`AcceptsPackages`, `ReportsPackageStatuses`) and two Server capabilities become live and
  self-exercisable against our own Server, with **no dependence on a Go/Bindplane server**.
- Negative / trade-offs: the first increment is **deliberately spec-incomplete on the code-signing MUST**
  (content-hash, not signature) — it must not be presented as code-signing-conformant until the follow-up
  lands, and package distribution over the insecure dev transport is trusted-network-only. The blast
  radius grows: the Supervisor now **replaces an executable and restarts it**, a heavier failure mode
  than a config swap (mitigated by hash verification, the bounded health wait, and binary rollback). The
  Server grows an HTTP **file-serving route** and binary/hash handling beyond the "single comparison"
  minimalism. Correctness is **not** oracle-checked (none exists) but established against the spec's
  normative requirements and unit/integration tests over the local server — a weaker guarantee than the
  Collector Supervisor's other behaviours carry, and called out as such.
- Follow-ups, each its own ADR: **cryptographic signature verification** (closes the MUST — the immediate
  next step); **addon** packages; the **Foreign Agent** binary via the Custom Supervisor; **resumable /
  range** downloads; and, if re-offers become costly, Server-side per-agent package bookkeeping.
