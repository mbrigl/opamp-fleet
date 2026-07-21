# ADR-0007: Self-update of the Supervisor Host via versioned installs with rollback (the Updater)

- **Status:** 🟢 accepted
- **Date:** 2026-07-20
- **Deciders:** Maintainer

## Context

The specification requires the Supervisor Host to "replace its own binary in place — a self-update that
survives the service restart and is rolled back on failure" (**Goal #11**; Strategy, "Ship the Agent as
a self-updating OS service"). The vocabulary already fixes the shape of the answer: the **Updater** is
"the *separate process* that applies a Package: it stops the target, replaces its binary, restarts it,
and rolls back on failure. **A running process cannot reliably replace its own binary, so this work is
handed off across a process boundary.**" None of this exists yet; ADR-0006 introduces the CLI and the
`service stop`/`service start` controls this mechanism drives, and leaves a placeholder `update`
subcommand for it.

The process-boundary rule is a hard platform constraint, not ceremony. On Windows a running `.exe` is
locked and cannot be overwritten at all; on Unix a running binary can be unlinked and replaced, but a
process cannot reliably stop, swap, and restart *itself* through the service manager. A second decision
is *how* the swap is done: **overwrite the installed binary in place** (keeping a `.bak` copy to restore)
versus **install each version side by side and switch a pointer** to the active one. The state of the art
for a fleet agent that self-updates is the latter: Elastic Agent never overwrites its running binary — it
unpacks each version into `data/elastic-agent-<hash>/`, flips a top-level `elastic-agent` symlink, writes
an `.update-marker`, and runs a **watcher** (its own `watch` subcommand, invoked **from the new binary**)
that health-checks the new version for a grace period and rolls back by flipping the symlink back. That
model removes the Windows file-lock problem by construction, makes rollback an atomic pointer swap rather
than a file-restore, and lets several previous versions coexist for a rollback window.

The delivery of the new binary over OpAMP as a **Package** (server package store, `packages_available`,
download, `PackageStatuses`, capability negotiation) is a separate axis the specification sequences after
the loop ("close the loop before widening it"); this ADR is scoped to the **local mechanism** that
installs a new version, switches to it, restarts, health-gates, and rolls back, triggered by a
locally-provided new binary. Choosing the process model, the swap/rollback strategy, and the verification
bar constrains future work, so per AGENTS.md §3 it needs an ADR.

## Decision

We will realize the Updater as the **same `supervisor-host` binary re-invoked with `update`, run as a
detached process from the *newly-installed* binary**, and apply updates by **installing each version
side by side and switching a stable pointer** — never overwriting the running binary. This honours the
specification's process boundary while keeping all functionality in one binary (the Updater is a *role
realized as a distinct process*, not a distinct build artifact).

On-disk layout under the state/install root:

- `versions/<sha256>/supervisor-host[.exe]` — every installed version, kept side by side.
- `current` — a **stable pointer** to the active version directory: a **symlink on Unix**, a **directory
  junction on Windows** (a junction needs no symlink privilege, sidestepping the elevation/Developer-Mode
  requirement that Elastic hit with Windows symlinks). The OS service installed by ADR-0006 points at
  `current/supervisor-host`, so switching the pointer is all it takes to change what the service runs.
- `.update-marker` — records the previous and target versions/hashes, written **atomically**
  (temp file + rename) so it is never observed half-written; it makes an interrupted update
  **recoverable on the next startup** (see below).
- **Shared state** (Instance UID, effective configuration, the health file) lives at the root, **outside**
  any `versions/<sha256>/` directory, so a rollback keeps the Agent's identity and configuration — only
  the binary is versioned, never the state.

The update sequence:

1. **Stage & verify** — place the new binary at `versions/<sha256>/…`, verify its **SHA-256** against the
   expected content hash (fail closed on mismatch), and mark it executable.
2. **Hand off across the process boundary — outside the service's supervision.** The running service
   spawns the *new* binary detached: `supervisor-host update --target <version-dir> --previous
   <current-target>`. The Updater **must not remain in the service's own supervision scope**, or stopping
   the service in step 3 would kill it mid-update and leave the host down: on **systemd** the default
   `KillMode=control-group` kills the whole cgroup on stop, so the Updater is launched as a **transient
   scope** (`systemd-run --scope`, or an equivalent cgroup breakaway) rather than a plain child; on
   **Windows** it is spawned fully detached (`DETACHED_PROCESS`, breaking away from any service job so it
   survives the stop); on **macOS** a launchd daemon does not kill non-tracked children, so a detached
   child suffices. The Updater now executes from the newly-installed copy, not from `current`.
3. **Write the marker, stop the service** (ADR-0006 `service stop`) and wait for it to exit. Stopping
   must **suppress the manager's auto-restart** for the duration (systemd keeps an explicitly stopped
   unit down; launchd `KeepAlive` would restart it, so the Updater uses `bootout`/`kickstart` semantics —
   see ADR-0006 — rather than a plain `stop` that launchd immediately undoes).
4. **Switch the pointer** — atomically repoint `current` at the new version directory.
5. **Restart** the service (`service start`); it loads the new version via `current`.
6. **Health-gate — two-tier, so a Server outage does not roll back a good update.** The freshly started
   daemon writes a **local health file** early in `run` and enriches it as it learns more. The gate
   passes when the new version **stays up for a settle window AND** either (a) it completed a **successful
   OpAMP round-trip and reported Healthy** (the strong signal, following Elastic Agent's control-protocol
   health check) **or** (b) the Server is **demonstrably unreachable** while the process itself stays
   locally healthy — so a bad binary that crashes or never stabilises still fails the gate, but a good
   binary is **not** rolled back merely because the Server happens to be down (which is exactly when the
   Agent must keep running). "Still running but never healthy, with the Server reachable" fails the gate.
7. **Commit or roll back** — healthy → clear the marker and keep the previous version for the rollback
   window; unhealthy or timed out → stop, **repoint `current` back to the previous version**, restart,
   and record the offending version as bad so it is not retried.

**Crash consistency & resume.** Because the marker is written atomically before the switch and cleared
only after a healthy commit, an update interrupted by a crash or power loss at any point leaves a marker
the next startup can act on: the Supervisor Host inspects the marker on boot and **either completes the
switch-and-health-gate forward or rolls `current` back** to the recorded previous version, so the host
never comes up wedged between two versions (the all-or-nothing property that A/B and snapshot-based OS
updaters get from their slots/snapshots, achieved here with one atomic marker + pointer).

Verification is **SHA-256 only** for now (`sha2` is already a workspace dependency); **signature
verification (cosign/TUF) is deferred**. Retention is **keep-N previous versions** (a small bound, e.g.
the last one or two) with older ones garbage-collected once they are neither active nor a rollback
target — which gives the desired rollback "for free" instead of a single `.bak`. No new mandatory
dependency is required beyond the standard library's `fs`/`process` plus `sha2`.

## Alternatives considered

- **Overwrite the installed binary in place, keep a `<name>.bak`** — functionally satisfies rollback but
  is strictly weaker: on Windows it only works because the service is stopped first (a running `.exe` is
  locked), rollback is a copy-restore rather than an atomic switch, a crash mid-swap can leave a
  half-written binary, and only one previous version (N-1) is recoverable. The side-by-side pointer model
  avoids all four (this was the original ADR-0007 draft, replaced after checking the Elastic Agent prior
  art).
- **A/B dual-slot or image-based atomic updates** (Mender, RAUC, SWUpdate) — the most robust rollback
  there is (two independent block devices, power-fail-safe, bootloader-driven switch), but they update
  the **whole root filesystem** and require a partition layout, bootloader integration, and a **reboot**
  — the wrong granularity and a gross YAGNI violation for replacing one self-contained agent binary on
  three general-purpose OSes.
- **OSTree / rpm-ostree or btrfs-snapshot transactional updates** (openSUSE MicroOS
  `transactional-update`) — OSTree is in fact the *same* atomic pointer-swap idea (it atomically swaps a
  `/boot` symlink between deployments) and snapshot updaters give the same all-or-nothing property we
  adopt; but both operate at OS-deployment granularity, are Linux/filesystem-bound, and bring bootloader
  or btrfs machinery irrelevant to a single cross-platform binary. We deliberately take their *pattern*
  (atomic switch, keep the old version, resume/rollback on failure) at application granularity rather
  than their machinery.
- **A separate Updater binary target** — a second artifact to build, sign, ship, install, and keep
  version-locked, for no capability the re-invoked-subcommand model lacks; the same binary run as a
  different process already satisfies the process boundary (YAGNI).
- **In-process self-replacement via `self-replace`/`self_update`** (the rustup approach) — clever Windows
  self-overwrite tricks, but the specification explicitly rejects a process replacing its own running
  binary, and stopping/restarting *oneself* through the service manager is not reliable; running the
  Updater from the new install sidesteps the whole problem.
- **A Windows symlink for `current` (as Elastic uses)** — symlink creation on Windows needs
  `SeCreateSymbolicLinkPrivilege` (elevation or Developer Mode); Elastic carries Windows-specific
  workarounds and a registry-ACL step because of it. A **directory junction** achieves the same stable
  pointer without that privilege, so we prefer it; updating the service's `ImagePath` per version is a
  further fallback if junctions ever prove insufficient.
- **`MoveFileEx` with `MOVEFILE_DELAY_UNTIL_REBOOT`** — the technique for when you *cannot* stop the
  holder; needs administrator rights, fails on network shares, and reports success merely for *queuing*
  the rename. Irrelevant here because nothing is overwritten and the service is stopped first.
- **Health-gate via the Server's view** — couples rollback to Server reachability; a *local* health
  touch-file is simpler and works even when the Server is down, which is exactly when a bad update must
  still self-heal.
- **Signature verification / an unbounded version history now** — more machinery (key distribution, a
  signing toolchain, a trust model the specification's Non-Goals currently defer; retention policy) than
  Goal #11's "rolled back on failure" needs; content-hash + a small keep-N is the simplest robust bar,
  with signatures named as the natural follow-up.

## Sources / Prior art

- **Elastic Agent upgrade/rollback — the primary prior art for a self-updating fleet agent** (versioned
  `data/elastic-agent-<hash>` dirs, `elastic-agent` symlink flip, `.update-marker`, the `watch`
  subcommand run from the new binary, ~10-minute health grace period, symlink-flip rollback, never
  overwriting the running binary): <https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md>,
  <https://www.elastic.co/docs/reference/fleet/upgrade-elastic-agent>,
  <https://deepwiki.com/elastic/elastic-agent/6.2-upgrade-process>. Watcher must run from the new binary:
  <https://github.com/elastic/elastic-agent/issues/2873>; Windows symlink/service pitfalls:
  <https://github.com/elastic/elastic-agent/issues/4443>.
- OTel `opampsupervisor` update algorithm — save current, overwrite, restart, health-check, revert,
  mark bad:
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md>;
  OpAMP Packages/`DownloadableFile` (content hash + signature): <https://opentelemetry.io/docs/specs/opamp/>.
- rustup self-update — re-invoke a staged binary (the detached-new-binary handoff):
  <https://github.com/rust-lang/rustup/blob/main/src/cli/self_update.rs>; `self-replace` (considered
  fallback): <https://github.com/mitsuhiko/self-replace>.
- Chrome/Omaha out-of-process updater: <https://omaha-consulting.com/google-omaha-tutorial-chrome-updater>;
  ChromeOS A/B + rollback (keep the old copy, switch back on failure):
  <https://chromium.googlesource.com/aosp/platform/system/update_engine/+/HEAD/README.md>.
- Robust OS-granular updaters compared/considered (A/B slots, atomic switch, snapshot rollback — the
  source of the crash-consistency/resume property adopted here at application granularity): OSTree /
  rpm-ostree atomic `/boot` symlink swap <https://projectatomic.io/docs/os-updates/>; openSUSE
  `transactional-update` (all-or-nothing snapshot updates) <https://github.com/openSUSE/transactional-update>;
  RAUC/Mender/SWUpdate comparison <https://raymo200915.github.io/2023/02/21/Research-of-OTA-solutions.html>.
- Windows directory junctions vs symlinks (junctions need no elevation) and `MoveFileEx`/
  `MOVEFILE_DELAY_UNTIL_REBOOT` (rejected):
  <https://learn.microsoft.com/en-us/windows/win32/fileio/hard-links-and-junctions> and
  <https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexa>, with
  <https://marc.durdin.net/2011/09/why-you-should-not-use-movefileex-with-movefile_delay_until_reboot-2/>.
- The Update Framework (TUF) — rollback protection and signed, hashed updates (deferred direction):
  <https://theupdateframework.io/>.
- Specification Goal #11, Strategy, and the "Package"/"Updater" vocabulary
  ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)); ADR-0006 (the `service stop`/`start` controls, the
  `current`-pointed service, and the `update` subcommand this mechanism uses).

## Consequences

- Positive: the Supervisor Host switches to a new version across a real process boundary on every
  platform, with a content-hash gate before applying and an **atomic pointer switch** for both the
  upgrade and the rollback; the running binary is never overwritten, so the Windows file-lock problem
  does not arise; keeping the previous version side by side makes rollback instant and gives a rollback
  window at no extra cost; the **health gate requires a real OpAMP "reported healthy" signal** rather
  than bare liveness, and the **atomic marker makes an interrupted update self-heal on the next
  startup** — so a crash mid-update never leaves the host wedged. Goal #11's self-update-with-rollback,
  in one binary, no new mandatory dependency. The updater is **isolated in a self-contained `update`
  module** that touches the service only through ADR-0006's `ServiceControl` seam, so the swap/rollback
  logic is decoupled from the service backends and unit-testable with a fake control.
- Negative / trade-offs: the pointer/versioned-directory machinery is more than a single `.bak`
  (per-OS pointer handling — symlink vs junction — a retention/GC policy, and the service must be
  installed pointing at `current` from the start, so ADR-0006's `service install` must lay out the
  `current` pointer); keeping N versions uses more disk; content-hash verification trusts whoever
  supplied the binary until signatures land. The **risk centre is the process-supervision escape**: the
  Updater must reliably survive the service stop on every platform (systemd cgroup breakaway, Windows job
  detach, launchd non-tracked child) and suppress the manager's auto-restart during the swap — if it dies
  after the stop but before the restart, the host stays down until the next boot (where the marker resume
  recovers it). This path is platform-specific, only smoke-testable on real hosts (CI compiles it but
  cannot exercise the SCM/launchd/cgroup behaviour), and needs regression-grade tests around the pointer
  switch, the two-tier gate, and marker resume. On macOS a freshly written binary must be at least
  **ad-hoc code-signed and cleared of the `com.apple.quarantine` xattr** or Gatekeeper blocks it
  (acute once ADR-0008 delivers binaries over the wire).
- Follow-ups: **ADR-0008 "OpAMP package delivery"** wires this mechanism to the wire — flipping the
  `AcceptsPackages`/`ReportsPackageStatuses` capabilities, handling `packages_available`, downloading and
  verifying `DownloadableFile`, and reporting `PackageStatuses` — so the Server can deliver the version
  this Updater installs; a further ADR may add **signature verification** and a longer, policy-driven
  **manual rollback window** (as Elastic offers) if operators need it.
