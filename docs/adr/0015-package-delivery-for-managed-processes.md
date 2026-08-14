# ADR-0015: Package delivery for Managed Processes — verified download, Supervisor-applied, health-gated, rolled back

- **Status:** 🟢 accepted
- **Date:** 2026-07-24
- **Deciders:** Markus Brigl

## Context

### Delivery

Goal 10 asks the Server to update *"an agent's binary — the Collector's, and the Client's own —
verifying each Package before it is applied, reporting the outcome, and rolling back on failure. A
failed update is reported, not silent."* The control loop, configuration targeting, authentication,
and credential rotation are all in place (ADR-0005, ADR-0006, ADR-0007, ADR-0008, ADR-0009, ADR-0010, ADR-0011, ADR-0012, ADR-0013 and ADR-0014); package delivery is the
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

### A package as a directory tree

The delivery above installs a Managed Process's program, and
[ADR-0017](0017-package-artifacts.md) lets that program arrive inside an archive the Agent
opens. The single-file path assumes the program is **one file**: `install_executable` in
`crates/client/src/supervisor/process.rs` extracts exactly one member — the one whose file name
equals the configured program's — and renames it over `<supervisor_dir>/<name>/program/<file>`.

That assumption holds for a Go or Rust agent and for the Collector, which is why it is reasonable.
It excludes an entire class of agent, and Fluent Bit is the case that makes it concrete: what
upstream ships for Linux is a `.deb`/`.rpm` installing an executable *plus* the shared objects and
plugins it loads. `config/client.toml` says so, and points the operator at an absolute path
and the machine's package manager. That is an honest workaround and a real gap: the fleet can
configure Fluent Bit centrally, watch its health centrally, and roll its configuration back
centrally — and then needs configuration management to put the binary there in the first place, on
every host, for every version.

Building it statically is not the answer available to us. `FLB_STATIC_BINARY` exists only as a draft
pull request open since 2020 (`fluent/fluent-bit#2558`), unmerged, with unresolved GPL concerns
about static linking, and it drops LuaJIT and SQLDB. Third-party static builds exist; asking an
operator to swap the upstream artifact for someone else's rebuild in order to be managed is a worse
bargain than the absolute path they have today.

Four forces shape what may be done about it:

- **The protocol does not constrain this.** The Baseline is explicit that "the content of the file,
  functionality provided by the packages, how they are stored and used by the Agent side is Agent
  type-specific and is outside the concerns of the OpAMP protocol". A package that unpacks into a
  tree is entirely within it.
- **ADR-0019 made the written shape of the program path the whole of a Supervisor's consent** to
  being updated. Whatever this adds must not make that consent depend on a value the file does not
  show.
- **A single-file archive never chooses a path**, which is exactly why there is no traversal
  defence to get wrong: one member, one destination the Client picked. Unpacking a tree gives the
  archive a say in where bytes land, and that is a security boundary this decision creates rather
  than inherits.
- **A failed update must still be undoable.** The single-file rollback is a sibling rename
  (`<binary>.rollback`) — atomic, free, and available because there is exactly one file.

### Finishing the rollback lifecycle

The health gate above swaps the new program over the old, restarts, and if the process does not
survive `apply_grace_secs` the swap is **rolled back**; a directory tree gets the same gate. The
mechanism lives in `crates/client/src/supervisor/process.rs` (`InstallTarget` +
`Runner::swap_and_gate`): `set_aside` renames the live program to a `.rollback` sibling, `install`
writes the new one, the process is restarted and gated. The bare gate then does this:

- **fail + a predecessor exists** → `restore()` renames the `.rollback` back over live;
- **fail + no predecessor** (a first install onto an empty `program/`) → `discard()` removes the
  just-installed program, leaving `program/` empty;
- **success** → `drop_backup()` deletes the `.rollback` **immediately**.

Operating the bare gate against a real Collector surfaced three gaps, each a way the system harms
itself:

- **Gap 1 — the discard-on-first-install case loops.** A first package that installs but cannot survive the
   grace (a port already bound, an empty config, any crash-on-start) is discarded, `program/` goes
   empty, the Server sees `installed hash ≠ desired` and re-offers, the Client re-downloads and
   re-unpacks (hundreds of megabytes each round), it crashes again — indefinitely. Observed live:
   the same 0.157.0 artifact downloaded and unpacked once per second.
- **Gap 2 — a rolled-back predecessor that also fails to start loops.** After a rollback the restored old
   program is respawned by the Runner, which retries **forever** on a capped backoff and never gives
   up (`Runner::run`, the `exited` arm). If both the new and the old version are broken, the
   Supervisor spins.
- **Gap 3 — the predecessor is deleted the instant the new one is up.** `drop_backup()` runs on success, so
   there is no window in which an operator (or an automatic later health signal) can fall back to the
   version that was running an hour ago — the moment a new version survives its first three seconds,
   the only copy of its predecessor is gone.

None of these is a new feature request in disguise; they are the rollback lifecycle not being
finished. The forces:

- **Health-gating is the right primitive, but "survives the grace" is a *first* signal, not a
  *final* one.** A Collector can pass three seconds and still be the wrong version — a slow leak, a
  dropped exporter. Keeping the predecessor briefly turns a bad rollout into a one-line fix instead
  of a re-delivery.
- **A retry that never gives up is a denial of service against the fleet's own Server.** The
  Client's *self*-update already draws this line — it rolls back after **three** failed attempts
  (ADR-0018) rather than restarting forever. The Managed-Process path should be no less disciplined.
- **A first install has nothing to roll back to.** Discarding what was just written is not a
  rollback; it is throwing away the only artifact the Server has, which is exactly what makes the
  re-offer loop turn. Leaving it in place — reported `InstallFailed` — stops the churn and keeps the
  bytes that were already verified.
- **Retention is a policy, and policy is the operator's.** How long a superseded version is worth
  keeping depends on the host (disk) and the rollout (risk), so it must be configurable — a global
  default with a per-Supervisor override, the shape `apply_grace_secs` already has.

## Decision

### Delivery

We will implement OpAMP package delivery **for Managed Processes**, deferring the Client self-update:

- **Scope.** Supervisor-backed Agents (ADR-0011) declare `AcceptsPackages` and
  `ReportsPackageStatuses`; the self-Agent declares neither (as it declares no
  `AcceptsRestartCommand` — the same "no process boundary yet" reason). One **top-level** package
  per Managed Process — its program, a single file or a directory tree (below). `Addon` packages,
  download progress details, and HTTP range resumption are recognised on the wire but not acted on
  yet (noted, not silently dropped).

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

### Directory-tree packages

We will let a package be a **directory tree**, unpacked whole **beside the one it replaces** and
swapped in by renaming directories — the same move the single-file path already makes with
`<binary>.rollback`, one level up. The principle is the one
[ADR-0010](0010-client-os-service.md) uses for the Client's own versions and
[ADR-0018](0018-client-self-update.md) trusts for replacing a running program: build the new one
somewhere else entirely, switch by a single atomic operation, and keep what ran until the new one
has proved itself.

1. **A `[[supervisor]]` block gains one optional key, `program_path`** — a relative path *inside*
   the package, e.g. `bin/fluent-bit`. Absent, the layout is the single-file one: one member, one
   file. Present, **every member of the archive is extracted**, each keeping its own relative path,
   and `program_path` says which of them is the program.

   `binary`/`command` keeps its ADR-0019 meaning untouched: a bare file name still means "this
   Supervisor's own directory, and therefore it takes packages", an absolute path still means the
   machine's. Consent stays readable in exactly the place it is read today; `program_path` says
   *where inside* the delivered tree, never *whether*. Tree mode is triggered by that key rather
   than by what the archive happens to hold, so what a host will run is readable in the
   configuration before any artifact exists.

2. **`program_path` matches a member by its trailing path components, not from the archive root.**
   A release tarball almost always wraps everything in one version-named directory —
   `fluent-bit-3.1.0/bin/fluent-bit` — and a `program_path` that had to name it would be wrong at
   the next release, silently, which is the failure ADR-0019 exists to make unspellable. So
   `program_path = "bin/fluent-bit"` matches any member whose path *ends* with those components,
   exactly as the single-member rule matches a file name "wherever the archive keeps it". More
   than one match is refused, naming the candidates, and answered by writing more of the path.

3. **The tree is unpacked to `<supervisor_dir>/<name>/program/tree/`**, keeping whatever directory
   structure the archive has below the stripped prefix, and the tree it replaced is kept beside it
   as `program/tree.rollback`. The Managed Process is spawned from `program/tree/<program_path>` —
   a path that follows from the configuration alone, so it is known at startup, before any package
   has ever arrived. The new tree is built in `program/.staging` and moved into place by a single
   rename, so the live name is either the old tree or the new one and never a mixture; a failed
   install renames the previous one back.

   Two fixed names rather than a version-named directory and a `current` pointer: a directory
   rename is atomic on every platform this Client runs on, while a pointer is a symlink on Unix and
   a junction on Windows — machinery ADR-0010 runs once, as an Administrator, at install time, and
   which would here run on every package. It is also the move the single-file swap already makes
   (`<binary>.rollback`), so there is one mechanism to understand rather than two. What is lost is
   the version being legible on disk; the Agent reports it, which is where an operator reads it
   anyway.

4. **The archive's paths are sanitized, and every member is bounded.** A member is refused, and the
   whole install with it, when its path is absolute, contains a `..` component, or is a symlink or
   hard link. Extraction is additionally bounded by a total byte count and a member count, not only
   the per-member limit of the single-file path. Refusing the install is the only correct answer: a
   partially unpacked agent is worse than none.

5. **File modes come from the archive on Unix, plus `program_path` is always made executable.** A
   tree carries its own modes and a `tar` preserves them; the one thing that must not depend on how
   the archive was built is whether the program can be executed at all.

   A `.7z` is the exception: it stores Windows attributes, and a Unix mode survives in them only by
   a convention this Client will not bet an agent's executability on. A tree packed as `.7z` gets
   its program made executable and nothing else, so an agent that ships helper executables beside
   its program is a reason to use `.tar.gz` — the format upstream releases use anyway.

6. **Nothing changes for a single-file package.** No `program_path`, no tree, no `tree.rollback` —
   the existing path stays exactly as it is, including its own rollback. This decision adds a second
   shape; it does not migrate the first.

### Rollback, retention, and no restart loop

We will finish the package-apply lifecycle so that it **rolls back only to a real predecessor,
never loops, and retains a superseded version for a configurable grace period before deleting it.**

7. **Rollback needs a predecessor (gap 1, the first-install case).** A failed apply with a
   `.rollback` present restores it, as the health gate above does. A failed apply with **no**
   predecessor performs **no rollback**: the just-installed (and content-verified) program is **left
   in place**, the package is reported `InstallFailed` with the reason, and nothing is discarded.
   `program/` is never emptied by a failure, so the "installed ≠ desired, re-offer, re-download"
   loop cannot start.

8. **A failed apply is terminal for that package hash — no restart loop (gaps 1 and 2).** After a
   failed apply (whether it rolled back or not), the Supervisor does **not** respawn the Managed
   Process in a tight loop. It reports the failure as health and `InstallFailed` and then **waits**
   for a state change — a new configuration, a different package hash, or an operator restart —
   rather than retrying the same broken artifact. In particular a rolled-back predecessor that then
   also fails to stay up is reported unhealthy and **not** restarted again. The Server's own gate
   (it does not re-offer a hash the Agent reported `InstallFailed`) is the matching half; this
   decision is the Client half. The give-up threshold mirrors the self-update's **three** attempts
   (ADR-0018) so the two update paths behave alike.

9. **A superseded version is kept for a grace period, then deleted (gap 3).** On a **successful**
   apply the predecessor is **not** deleted immediately. The `.rollback` is retained and a persisted
   marker records the deadline (`applied_at + retain_previous`); a cleanup pass on startup and on a
   periodic tick deletes any `.rollback` past its deadline. The period is configured by a new global
   `[updates] retain_previous_secs` (default **86400**, one day) with a per-Supervisor override in
   the `[[supervisor]]` block, exactly as `apply_grace_secs` is global-with-override. A
   subsequent update within the window supersedes the marker (each Supervisor keeps at most one
   predecessor — the immediately previous version). `0` means delete on success.

The persisted marker lives in the Supervisor's own directory (ADR-0010/0021), so the deadline
survives a Client restart the way the self-update outcome marker does. Deletion is best-effort and
logged; a marker whose `.rollback` is already gone is simply cleared.

## Alternatives considered

### Delivery

- **Implementing the Client self-update (goal 11) in the same decision** — it needs the separate
  Updater process ADR-0010's layout was built for (a process cannot replace its own running
  binary), plus service-restart survival and version pruning. That is a distinct, harder decision;
  bundling it would double the surface and delay the Managed-Process case that is unblocked now.
  Deferred to a follow-up, which this decision's download/verify/report machinery will feed.
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
- **A bespoke package format (a manifest bundle)** — YAGNI; a Managed Process's package is its
  program, as one file or as the directory tree of point 1. `PackageType_Addon` stays
  on the wire for when a plugin needs it, not invented ahead of a consumer.

### Directory-tree packages

- **Use the protocol's Addon packages for the supplementary files** — top-level for the executable,
  addons for the rest. It is the mechanism's own vocabulary, and it is the wrong shape here: nothing
  expresses that a set of packages belongs together, so there is no version they are consistent at,
  no ordering, and no atomicity — an Agent could sit with a new executable and old shared objects,
  which is precisely the state that will not start. ADR-0016 also targets *one* top-level package
  per Agent by Selector; addons would need a second targeting model to answer "which addons, at
  which version, for this Agent".
- **A `strip_components` key**, as `tar` has, to drop the release tarball's leading directory.
  Stable across versions — it describes how a publisher builds archives, not which version — and one
  more number an operator has to get right by inspecting an artifact first. Suffix matching needs
  nothing written down and fails loudly when it is ambiguous, which is the better trade for a value
  nobody can verify until a rollout runs.
- **Strip a leading directory automatically** when every member shares one. It reads as the obvious
  convenience and is wrong on an archive whose top level is *meaningful* — one holding only `lib/`
  would be flattened into the root, and the failure would be a program that starts and cannot find
  its libraries. Behaviour that changes with the archive's contents is exactly what should not sit
  under a rollout.
- **Find the program by searching the unpacked tree**, with no `program_path` at all — the natural
  extension of the single-file match-by-file-name. It removes a key and it removes the answer to
  "what will this host run" from the configuration file: before the first package there is nothing
  to search, so the spawn path would exist only after an install, discovered rather than written.
  ADR-0019 put that value in the file on purpose.
- **A versioned directory per package and a `current` pointer** — the shape of ADR-0010's own
  layout. Rejected in point 3: the pointer is a symlink on Unix and a junction on
  Windows, machinery that would run on every package rather than once at install time.
- **A self-extracting single file** — a script carrying an embedded payload, installed as the
  program by the single-file mechanism, unpacking itself on first run. It needs no change here at
  all, which is its whole appeal. It also re-extracts on every update, hides what is installed from
  everything that inspects the host, makes the reported version a property of the wrapper rather
  than the agent, and turns a rollback into "run the old wrapper again and hope". A supported
  mechanism should not be an unsupported one wearing a costume.
- **Unpack the tree over the existing `program/` directory in place**, with no version directory.
  Smaller diff, and it destroys the only copy of the working agent at the moment it is most needed:
  a failed unpack halfway through leaves a tree that is neither version. The rollback that the
  health gate above depends on would stop being available exactly when it fires.
- **Let the operator name a subdirectory in `binary`/`command`** (`bin/fluent-bit`) instead of
  adding a key. Rejected by ADR-0019 on its own terms: anything that is neither a bare file name nor
  an absolute path is refused precisely so consent is unambiguous, and quietly admitting a third
  form would make a fleet-visible capability depend on parsing rather than on shape.
- **Leave it, and let configuration management install multi-file agents.** The status quo, and
  defensible — but it splits the fleet into agents this project can manage and agents it can only
  watch, and the split follows how an agent happens to be linked rather than anything an operator
  chose.

### Rollback, retention, and no restart loop

- **The bare gate (immediate delete, infinite retry, discard-on-first-install).** Rejected: the
  three behaviours above were each observed to harm a running deployment — an unbounded re-download
  loop, a spin between two broken versions, and no rollback window at all.
- **Keep every superseded version, not just the last.** Rejected: it turns `program/` into an
  unbounded version store on a host chosen for its disk budget, and the Managed-Process rollback is
  a *one-step* safety net, not the versioned layout the Client's own self-update keeps (ADR-0010).
  One predecessor is what a rollback needs.
- **Give up after one failed apply rather than three.** Rejected for consistency: the self-update
  path already settled on three (ADR-0018), and a single transient failure (a briefly-held port)
  should not permanently strand a rollout.
- **Retention as time-to-live on the artifact store rather than the `.rollback`.** Rejected: the
  predecessor a rollback needs is the *installed* program, not the downloaded artifact; tying
  retention to the staged download would delete the very thing a fallback restores.

## Sources / Prior art

- [OpAMP specification — Packages / Downloadable Packages and Code Signing](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md#packages)
  — the hash-gated sync, the `InstallPending/Installing/Installed/InstallFailed` lifecycle, and the
  code-signing recommendation; `PackagesAvailable`, `PackageAvailable`, `DownloadableFile`,
  `PackageStatuses`, `PackageStatus` in the pinned Baseline proto (`crates/opamp/proto/v0.18.0/opamp.proto`).
- **OpAMP Baseline** — packages are opaque to the protocol: "The content of the file, functionality
  provided by the packages, how they are stored and used by the Agent side is Agent type-specific
  and is outside the concerns of the OpAMP protocol." `PackageType_TopLevel`/`PackageType_Addon` in
  the pinned proto (`crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto`).
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>
- [`opamp-go` `PackagesSyncer`](https://github.com/open-telemetry/opamp-go) — the reference
  download-and-report component: compare `all_packages_hash`, per-package hash check, download,
  verify, set `server_offered_version`/`server_offered_hash`, report status.
- [Collector Supervisor](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — signs the package hash server-side and verifies before applying, the model adopted here.
- **Elastic Agent** distributes a `.tar.gz` holding a directory tree and states that the archive
  distributions — not the system packages — are the ones Fleet can upgrade: the same conclusion
  this decision reaches, that remote lifecycle management wants a self-contained tree.
  <https://www.elastic.co/docs/reference/fleet/install-standalone-elastic-agent>
- **Datadog Fleet Automation** keeps two installs side by side under `/opt/datadog-packages` "in
  case a rollback is needed" — the versioned-directory-plus-pointer shape, in a fleet product, for
  the same reason.
  <https://docs.datadoghq.com/agent/fleet_automation/upgrade_agents/>
- **Fluent Bit static linking** — `FLB_STATIC_BINARY` is an unmerged draft (open since 2020) with
  GPL concerns raised by a maintainer, and incompatible with `FLB_LUAJIT` and `FLB_SQLDB`. The
  reason "just ship it statically" is not an answer for this agent.
  <https://github.com/fluent/fluent-bit/pull/2558>
- ADR-0010 — the versioned install layout and content-hash manifest, and its explicit deferral of
  the self-update mechanism this decision's follow-up will complete; ADR-0011 — the Supervisor Port
  and the `apply_grace_secs` health gate reused as the package health gate; ADR-0012 — the store +
  REST + hash-gating pattern the package store mirrors; ADR-0013 — the credential the download
  reuses; ADR-0007 — the ring provider that verifies Ed25519.
- **This project's own ADR-0010 install layout** (`versions/`, `current`, side-by-side, pointer
  move) and ADR-0018's use of it to replace a running program — the mechanism is already here, and
  already trusted for the harder case of the Client replacing itself.
- This project's own [ADR-0018](0018-client-self-update.md) (the Client self-update: staged version,
  three-attempt give-up, marker across restart) is the precedent the rollback lifecycle aligns the
  Managed-Process path to.
- The versioned-layout-with-pointer model of [ADR-0010](0010-client-os-service.md) is the
  contrast that justifies keeping only *one* predecessor here rather than a full version store.

## Consequences

### Delivery

- Positive: goal 10 lands for Managed Processes — the Server updates a Collector's binary
  fleet-wide, every artifact hash- and signature-verified before it runs, health-gated on the same
  grace that gates a configuration, rolled back when it will not stay up, and reported per Agent.
  Four matrix rows (`AcceptsPackages`, `ReportsPackageStatuses`, `OffersPackages`,
  `AcceptsPackagesStatus`) are implemented; the package store reuses the ADR-0012 pattern and
  the health gate reuses ADR-0011, so little new machinery is invented.
- Negative / trade-offs: the Client cannot yet update *itself* (goal 11) — a named follow-up, not a
  silent gap; addon packages, download-progress reporting, and range-request resumption are on the
  wire but inert; the Server stores binary artifacts, growing its disk footprint and making it a
  distribution point that must be secured like one (the existing auth and TLS apply).
- Follow-ups: the Client self-update via a separate Updater process over ADR-0010's layout
  (staging, service-restart survival, rollback, version pruning); addon packages when a plugin
  needs them; download-progress details and range-request resumption for large artifacts; key
  distribution/rotation for signature verification.

### Directory-tree packages

- Positive: the class of agent that is an executable plus its libraries stops being second-class. An
  operator installs Fluent Bit the way they install anything else in the fleet — upload the
  artifact, aim its Selector — and the rollback, health gate, and version reporting that already
  exist come with it.
- Positive: the artifact stays the one upstream published, which is what ADR-0017 exists to protect.
  A `.deb` still is not openable, but the `.tar.gz` many projects publish alongside it is —
  wrapper directory and all, since `program_path` is written against the part of the path that does
  not change between releases.
- Negative / trade-offs: **the archive gains a say in where bytes land.** The single-file path's
  "no traversal to defend against" property is genuinely lost, and traded for a sanitizer that has
  to be right — absolute paths, `..`, symlinks, and hard links all become refusals that must be
  tested rather than assumed. This is the real cost of the decision and the part most worth
  reviewing.
- Negative / trade-offs: disk. Two unpacked trees per Supervisor, not two files — an agent with a
  few hundred megabytes of plugins doubles, on hosts where `supervisor_dir` was already the reason
  ADR-0019 made the location movable.
- Negative / trade-offs: a second layout to explain. `program/<file>` and
  `program/tree/<program_path>` coexist, and which one a Supervisor has depends on whether one
  optional key is set.
- Negative / trade-offs: mode and ownership semantics come partly from the archive, so an
  artifact built with sloppy modes produces an agent that does not start — a failure whose cause is
  in the archive rather than on the host.
- Follow-ups: whether `opamp-package-sign pack` should grow a directory mode, so the tool that
  builds artifacts can build this one too — without it an operator packs the tree by hand, which is
  the same gap the signing helper closes for single files. Whether the reported `service.version`
  should be re-probed after an install, which this makes more visible: a tree's version is even less
  likely to match what was probed at startup. And whether a Windows host, where the modes above mean
  nothing and links are a different mechanism, needs its own pass before this is enabled there.

### Rollback, retention, and no restart loop

- Positive: the two loops end. A first package that will not start stays installed and
  `InstallFailed` instead of triggering an endless re-download; a pair of broken versions is
  reported, not spun on. Token, bandwidth, and disk churn against the Server stop.
- Positive: a rollout gains a real fallback window — for `retain_previous_secs` after a successful
  update, the previous version is still on disk and an operator can put it back.
- Positive: the Managed-Process update path and the Client self-update path behave alike
  (three-attempt give-up, marker across restart), which is one rule to reason about instead of two.
- Negative / trade-offs: a superseded version occupies disk for up to a day by default (one
  extra program or tree per Supervisor). The per-Supervisor override and `0` exist for hosts that
  cannot spare it. A persisted marker and a periodic cleanup tick are new moving parts on the
  Managed-Process side.
- Follow-ups: the exact config key names and the cleanup-tick interval are settled in
  implementation; a regression test per behaviour (no-predecessor keeps the binary and does not
  loop; a twice-failing pair is reported and not respawned; a retained predecessor is deleted only
  after its deadline and survives a restart until then).
