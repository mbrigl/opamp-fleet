# ADR-0015: Package delivery for Managed Processes — verified download, Supervisor-applied, health-gated, rolled back

- **Status:** 🟡 proposed
- **Date:** 2026-07-24
- **Deciders:** Markus Brigl

## Context

Goal 10 asks the Server to update *"an agent's binary — the Collector's, and the Client's own —
verifying each Package before it is applied, reporting the outcome, and rolling back on failure. A
failed update is reported, not silent."* The control loop, configuration targeting, authentication,
and credential rotation are all in place (ADR-0005 through ADR-0014); package delivery is the
software half of that loop, and the last remaining Beta feature in [`CONFORMANCE.md`](../CONFORMANCE.md).

The Baseline's mechanism (pinned proto `v0.18.0`) is a hash-gated sync, the same shape this project
already implements twice (remote config, connection settings):

- The Agent reports `PackageStatuses.server_provided_all_packages_hash`. The Server compares it to
  its own aggregate and, on mismatch, sends `PackagesAvailable` — a map of `PackageAvailable`
  (type, version, `DownloadableFile{download_url, content_hash, signature, headers}`, per-package
  `hash`) plus `all_packages_hash`.
- For each offered package whose hash differs from what it holds, the Agent downloads the file,
  **verifies** it, installs it, and reports `PackageStatus` through the
  `InstallPending → Installing → Installed | InstallFailed` lifecycle — carrying `agent_has_version`
  /`agent_has_hash` and, on failure, an `error_message`.

Two forces shape the scope. First, the specification's own vocabulary: *"A running process cannot
reliably replace its own binary, so this work is handed off across a process boundary"* (**Updater**).
For a **Managed Process** that boundary already exists — the Supervisor is a separate process that
owns its Managed Process's lifecycle (stop, spawn, health-gate via `apply_grace_secs`; ADR-0011),
so the Supervisor *is* the updater for the binary it manages. For the **Client's own** binary
(goal 11) the boundary does not exist yet; ADR-0010 built the versioned install layout and the
content-hash manifest but **explicitly deferred** *"the update mechanism itself (staging new
versions over the wire, health gate, rollback, pruning old versions) … to a follow-up decision."*
Those are two different problems, and only the first is unblocked today.

Second, security. The Agent MUST verify a downloaded file before installing it; shipping software
distribution without verification would be indefensible and costly to retrofit. `content_hash` is
the integrity check; `signature` is the authenticity check the Baseline's *Code Signing* section
recommends (method *"is Agent specific"*). The project already carries the rustls **ring** provider
(ADR-0007), which verifies Ed25519 — so signature verification needs no new heavy dependency.

## Decision

We will implement OpAMP package delivery **for Managed Processes**, deferring the Client self-update:

- **Scope.** Supervisor-backed Agents (ADR-0011) declare `AcceptsPackages` and
  `ReportsPackageStatuses`; the self-Agent declares neither (as it declares no
  `AcceptsRestartCommand` — the same "no process boundary yet" reason). One **top-level** package
  per Managed Process — its binary. `Addon` packages, download progress details, and HTTP range
  resumption are recognised on the wire but not acted on yet (noted, not silently dropped).

- **Server** (`OffersPackages`, `AcceptsPackagesStatus`). A **package store** mirrors the
  Configuration store (ADR-0012): package artifacts and their metadata (name, version, type,
  `content_hash`, optional `signature`) persist under a `packages_dir`, are managed through the
  OpenAPI REST API, and survive restarts. The Server computes `all_packages_hash` as the Baseline
  prescribes — *"an aggregate of all packages names and content"* — offers `PackagesAvailable`
  hash-gated on the reported `server_provided_all_packages_hash`, and serves each artifact from its
  one listener (ADR-0005) at a `download_url` under `/api/v1/packages/{name}/file`. The download
  sits on the REST plane, which ADR-0013 deliberately left unauthenticated until operator
  authentication lands — so what protects an installed binary is **verification, not transport
  secrecy**: the artifact's SHA-256 content hash and its Ed25519 signature, both checked before it
  runs. `OffersPackages` (and `AcceptsPackagesStatus`) are declared only while the store is
  non-empty — an undeclared capability is never exercised.

- **Client / Supervisor** (`AcceptsPackages`, `ReportsPackageStatuses`). On an offer whose hash
  differs from the installed package: report `Installing`, **download** the file, **verify** it —
  `content_hash` (SHA-256) always, and the Ed25519 `signature` against an operator-configured
  public key when one is present (a signed package offered without a configured key, or a bad
  signature, is `InstallFailed`, never installed) — then hand the verified artifact to the
  Supervisor, which **applies** it exactly as it applies a configuration: stop the Managed Process,
  swap the staged binary over the target (the previous binary kept for rollback), spawn, and
  **health-gate** on the existing `apply_grace_secs` — surviving the grace is `Installed`, exiting
  within it restores the previous binary, respawns, and reports `InstallFailed`. The installed
  package (path, version, hash) persists in the Supervisor's state dir, so a restarted Client
  reports the version it runs and is not re-offered it. `PackageStatuses` is hash-gated and follows
  the `InstallPending → Installing → Installed | InstallFailed` lifecycle, mirroring the
  `APPLYING → APPLIED | FAILED` config path and the connection-settings status already built.

- **A new Port command.** `ProcessCommand::ApplyPackage { staged_path, version, hash }` extends the
  Supervisor Port (ADR-0011) beside `ApplyConfig`; its `PackageApplied` event closes the lifecycle,
  exactly as `ConfigApplied` closes configuration. No new process is spawned — the Supervisor is
  the process boundary the Updater vocabulary requires.

The operational story: an operator uploads a new Collector binary (with its Ed25519 signature) to
the Server through the REST API; the Server offers it to the matching Agents; each Supervisor
downloads, verifies, swaps, and health-gates it, rolling back a binary that will not stay up; the
outcome is visible per Agent in the fleet view.

## Alternatives considered

- **Implementing the Client self-update (goal 11) in the same ADR** — it needs the separate
  Updater process ADR-0010's layout was built for (a process cannot replace its own running
  binary), plus service-restart survival and version pruning. That is a distinct, harder decision;
  bundling it would double the surface and delay the Managed-Process case that is unblocked now.
  Deferred to a follow-up, which this ADR's download/verify/report machinery will feed.
- **A separate Updater process for Managed-Process packages too** — symmetric with self-update, but
  redundant: the Supervisor is already a distinct process from its Managed Process and already owns
  the stop/swap/spawn/health-gate it would delegate. Spawning another process buys nothing and
  violates simplicity-first.
- **Content-hash only, signatures later** — tempting for scope, but authenticity is exactly what
  "verifying each Package before it is applied" means for a binary, and retrofitting verification
  onto an install path is precisely the costly-to-reverse change ADRs exist to prevent. Ed25519 via
  the already-present ring provider keeps the cost low, so signatures ship now (optional per the
  Baseline, enforced when a key is configured).
- **Hosting artifacts on external object storage** — the `download_url` may point anywhere, but the
  Server already has one listener; serving artifacts there needs no new infrastructure. External
  URLs remain possible later without a protocol change.
- **Authenticating the download endpoint now** — it lives on the REST plane, which ADR-0013 left
  open pending a separate operator-auth decision; guarding one REST route ahead of that would
  prejudge it. The Ed25519 signature is the load-bearing protection against a substituted binary
  (a MITM cannot forge it without the operator's private key), so an unauthenticated download of a
  *signed* artifact is defensible. Authenticating the endpoint follows whenever operator auth does.
- **A bespoke package format (archive, manifest bundle)** — YAGNI; a Managed Process's package is
  its binary. The `PackageType_Addon` and multi-file cases stay on the wire for when a plugin needs
  them, not invented ahead of a consumer.

## Sources / Prior art

- [OpAMP specification — Packages / Downloadable Packages and Code Signing](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md#packages)
  — the hash-gated sync, the `InstallPending/Installing/Installed/InstallFailed` lifecycle, and the
  code-signing recommendation; `PackagesAvailable`, `PackageAvailable`, `DownloadableFile`,
  `PackageStatuses`, `PackageStatus` in the pinned Baseline proto (`crates/opamp/proto/v0.18.0/opamp.proto`).
- [`opamp-go` `PackagesSyncer`](https://github.com/open-telemetry/opamp-go) — the reference
  download-and-report component: compare `all_packages_hash`, per-package hash check, download,
  verify, set `server_offered_version`/`server_offered_hash`, report status.
- [Collector Supervisor](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — signs the package hash server-side and verifies before applying, the model adopted here.
- ADR-0010 — the versioned install layout and content-hash manifest, and its explicit deferral of
  the self-update mechanism this ADR's follow-up will complete; ADR-0011 — the Supervisor Port and
  the `apply_grace_secs` health gate reused as the package health gate; ADR-0012 — the store +
  REST + hash-gating pattern the package store mirrors; ADR-0013 — the credential the download
  reuses; ADR-0007 — the ring provider that verifies Ed25519.

## Consequences

- Positive: goal 10 lands for Managed Processes — the Server updates a Collector's binary
  fleet-wide, every artifact hash- and signature-verified before it runs, health-gated on the same
  grace that gates a configuration, rolled back when it will not stay up, and reported per Agent.
  Four matrix rows (`AcceptsPackages`, `ReportsPackageStatuses`, `OffersPackages`,
  `AcceptsPackagesStatus`) flip to implemented; the package store reuses the ADR-0012 pattern and
  the health gate reuses ADR-0011, so little new machinery is invented.
- Negative / trade-offs: the Client cannot yet update *itself* (goal 11) — a named follow-up, not a
  silent gap; addon/multi-file packages, download-progress reporting, and range-request resumption
  are on the wire but inert; the Server stores binary artifacts, growing its disk footprint and
  making it a distribution point that must be secured like one (the existing auth and TLS apply).
- Follow-ups: the Client self-update via a separate Updater process over ADR-0010's layout
  (staging, service-restart survival, rollback, version pruning); addon packages and multi-file
  layouts when a plugin needs them; download-progress details and range-request resumption for
  large artifacts; key distribution/rotation for signature verification.
