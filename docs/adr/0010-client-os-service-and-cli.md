# ADR-0010: Client as a multi-instance OS service — clap subcommand CLI, per-instance identity, versioned install layout

- **Status:** 🟢 accepted
- **Date:** 2026-07-23
- **Deciders:** Markus Brigl

## Context

The specification commits the Client to run as first-class infrastructure: it "installs as a native
operating-system service that updates itself in place, and runs on Linux, macOS, and Windows"
(Mission; Strategy), and **Goal #11** requires exactly that — install and run as a native OS service
on all three platforms, with an in-place self-update that survives the service restart. Today the
Client is none of that: `crates/client/src/main.rs` is a hand-rolled two-flag parser (`--config`,
`--version`) in front of a `#[tokio::main]` loop, shutdown is `ctrl_c()` only (systemd's `SIGTERM`
would hit the kill timeout and skip the `agent_disconnect` goodbye), there is no Windows SCM
integration, and the default config and state paths (`client.toml`, `client-state/`) are relative to
the working directory — meaningless under a service manager whose working directory is `/` or
`System32`.

Forces beyond the platform differences themselves:

- **The three service models genuinely differ.** systemd and launchd supervise an ordinary
  foreground process; the **Windows Service Control Manager (SCM)** launches the process and expects
  status reports over the SCM protocol within ~30 seconds or kills it with error 1053 — so "run
  under the manager" is a different code path on Windows. And a process cannot reliably detect an
  SCM launch (`StartServiceCtrlDispatcher` fails with error 1063 when *not* SCM-launched), so the
  installed command line must carry a marker.
- **Multiple instances on one host are a requirement.** One machine may run several Clients with
  different configurations (different Servers, staging vs. production). Service identity, install
  location, and state must therefore be *per instance* — a single fixed service name or path is not
  enough.
- **No fixed installation path.** The Client must be registerable from wherever its binary lives;
  the operator chooses the install root. Nothing may hard-code `/usr/bin` or `Program Files`.
- **Self-update must stay possible** (Goals #10/#11) even though its mechanism is out of scope here.
  What the service points at is the load-bearing choice: registering the raw binary path would force
  a re-registration of every instance on every update; registering a stable *pointer* makes a future
  update a pointer switch. On Windows the running `.exe` is locked, which rules out overwriting in
  place and independently motivates a versioned side-by-side layout.
- A CLI parser, a cross-platform service-management library, and a Windows service runtime are new
  dependencies and a new public interface surface — per `AGENTS.md` §3 that requires this ADR before
  any code. ADR-0007 constrains dependencies to the rustls/ring stack (no competing TLS/crypto
  backends); ADR-0008 fixes configuration as a hand-edited TOML file the `--config` flag points at.
  "Simplicity first / YAGNI" bounds the scope: service *lifecycle* and the *layout* now — not the
  update mechanism, not per-backend unit tuning.

## Decision

We will turn the Client into a **`clap` subcommand CLI** that registers, controls, and runs *itself*
as a native OS service on Linux (systemd), macOS (launchd), and Windows (SCM), parameterized by an
explicit instance name, with all daemon code isolated in one `service` module:

- **Subcommands:** `run` (foreground daemon — the default when no subcommand is given, so today's
  `client --config <path>` keeps working unchanged) and
  `service install | uninstall | start | stop | status`. Global flags: `--config` (ADR-0008),
  `--instance <name>` (default `default`), `--state-dir` (override). No environment-variable
  configuration is added; the Client stays file-configured, and the installed unit carries the
  config *path*, not the config.
- **Per-instance identity.** The service label is `io.opamp-fleet.client.<instance>`; the instance
  name is validated to the intersection of the systemd-unit, launchd-label, SCM-name, and
  directory-name grammars (lowercase `[a-z0-9-]`, no leading/trailing `-`, ≤ 32 chars) and rejects
  the Windows reserved device names (`con`, `prn`, `aux`, `nul`, `com1`–`com9`, `lpt1`–`lpt9`),
  which would otherwise be legal instance names but invalid directory names on Windows. On Windows
  the service additionally gets a human-readable display name, `OpAMP Fleet Client (<instance>)`.
  Each instance has its own install root and its own state directory, so any number of instances
  with different configurations coexist on one host. Instances are an *isolation boundary*
  (separate Server, credentials, lifecycle, rollback); scaling the number of managed Agents happens
  *inside* one instance via the multiplexing of ADR-0003 — most hosts run exactly one instance.
- **Lifecycle** is implemented over the **`service-manager`** crate (systemd, launchd, Windows SCM
  behind one API; verified 2026-07: v0.11.0 of 2026-02 is current and actively maintained, and it
  is effectively the only maintained cross-platform lifecycle crate — comparable Rust agents such
  as Vector and Mullvad hand-roll per-platform installs, so the wrapper around it stays thin enough
  to drop to platform-specific calls where a backend falls short). The default is a
  **system-level** service (systemd system unit / launchd `LaunchDaemon` / Windows `LocalSystem`)
  because a fleet client must run without a logged-in user and start at boot; `--user` is the
  development opt-in. The restart policy is `OnFailure { delay: 5 s }` — restart after a crash,
  never after an explicit stop, which a future updater relies on to swap the binary without the
  manager racing it back up. Backend unevenness is handled, not papered over: the `sc.exe` backend
  cannot express a restart policy (SCM failure actions can be set natively later if Windows crash
  restarts prove necessary); since v0.10 `install` on launchd no longer auto-starts (so `install`
  prints the follow-up `service start` step rather than pretending); and launchd `status` is
  advisory (a known upstream bug reports running services as stopped).
- **Versioned install layout, laid out by `service install`, rooted wherever the operator says**
  (`--root`, defaulting to the platform data directory per scope and instance, e.g.
  `/var/lib/opamp-fleet/client/<instance>`): the running executable is staged into
  `<root>/versions/opamp-client-<MAJOR.MINOR.PATCH>-<hash>/client` — Elastic Agent's directory
  naming (`elastic-agent-<version>-<hash>`) with our component name. The version part is
  ADR-0009's bare `MAJOR.MINOR.PATCH` base — **never the pre-release**; `<hash>` is the commit
  short-hash from the build metadata. The release `1.2.3` and a dev build descending from it thus
  live as `versions/opamp-client-1.2.3-a1b2c3d/` and `versions/opamp-client-1.2.3-b4e5f6a/`,
  distinguished by their commit alone; the **full** ADR-0009 version string (pre-release and
  metadata included) is recorded in the version directory's manifest, which is where
  release-or-dev is answered. Rebuilding the same commit maps to the same directory; staging into
  an already-present version directory replaces its contents and rewrites the manifest (an
  idempotent re-install, never a silent mix of two builds). The binary's full SHA-256 lives in the
  same manifest — the content hash a future self-update verifies staged packages against. A
  stable **`current` pointer** (symlink on Unix, directory junction on Windows —
  junctions need no symlink privilege, the same reason Scoop uses one for its `current` alias)
  points at the active version *directory*, and the service's program is `<root>/current/client`.
  Pointer switches are atomic where the platform allows: on Unix, create a temp symlink and
  `rename` it over `current` — never unlink-then-relink; on Windows the swap happens only while the
  service is stopped and must be idempotent and retried with backoff (antivirus scanners take
  transient locks on fresh executables). On start the daemon self-heals a torn swap: it verifies
  `current` resolves to the directory it actually runs from and repairs or reports the mismatch.
  `<root>/state/` is the default per-instance state directory when the config does not name an
  absolute one. All paths are absolutized at install time; the installed command line is
  `run --service --config <abs> --instance <name> --state-dir <abs>`.
- **Running under the manager** is a plain foreground process on Linux and macOS — the same `run`
  loop plus graceful `SIGTERM`/`SIGINT` shutdown via `tokio::signal` (feature already enabled),
  injected into the transports so the clean-shutdown `agent_disconnect` path fires on a service stop
  too; `SIGHUP` is explicitly ignored rather than left at its default terminate disposition
  (daemon(7) reserves it for config reload — a possible later feature, never an accidental kill).
  Shutdown completes well under launchd's 20-second `ExitTimeOut` default. On **Windows only**, a
  `cfg(windows)` runtime shim built on the **`windows-service`** crate registers the SCM control
  handler, reports `StartPending` → `Running` → `StopPending` (with wait hint) → `Stopped`, and then
  runs the identical daemon body; the `SERVICE_CONTROL_SHUTDOWN` path finishes in under ~5 seconds
  (the `WaitToKillServiceTimeout` default). The hidden `--service` marker flag — and only that flag
  — routes into the SCM dispatcher.
- **The version in all of this is [ADR-0009](0009-version-derivation-and-baking.md)'s.** The
  `versions/opamp-client-<MAJOR.MINOR.PATCH>-<hash>` directory names, the CLI `--version` output,
  and the OpAMP `service.version` attribute all call the single `version()` helper decided there —
  how the string is computed is entirely ADR-0009's contract, this ADR only consumes it (and
  renders base and commit into directory names per the naming rule above).
- **Module shape:** everything above lives in `crates/client/src/service/` (`mod.rs` with the narrow
  `ServiceControl` seam — `start`/`stop`/`state` — plus `runtime.rs`, `layout.rs`, `manager.rs`,
  and the Windows-only `windows.rs`), so a future updater depends on the seam, not on service
  internals. Errors stay `Result<_, String>` in the crate's existing style; no `anyhow`.

The update mechanism itself (staging new versions over the wire, health gate, rollback, pruning old
versions) is **out of scope** and deferred to a follow-up decision; this ADR only guarantees the
shape it needs. Richer unit/plist tuning (systemd `Type=notify`, launchd throttling) is deferred.

## Alternatives considered

- **A separate installer or OS packages registering the service** (`.deb`/`.pkg`/MSI wrapping
  `sc.exe`) — spreads the logic across artifacts, fixes the installation path, and cannot express
  "register this binary from wherever it is, N times". Subcommands keep one self-managing
  deployable; packages can still *ship* the binary later without owning the service.
- **Hand-write the three backends** (emit systemd units and launchd plists, call
  `CreateService`/`DeleteService`) — maximal control, three code paths to own and test;
  `service-manager` abstracts exactly install/start/stop/status/uninstall across them.
- **A daemonizing crate (`daemonize`) or double-fork** — Unix-only, gives nothing on Windows, and
  modern init systems want to supervise a foreground process, not a self-detaching one.
- **Register the binary's own path instead of the `current` pointer** — simpler now, but every
  future self-update would re-register every instance (needing admin rights on every update), and
  Windows locks the running `.exe` against replacement. The pointer costs one indirection at
  install time and makes updates a pointer switch.
- **Derive instance identity from the config path** (hash/slug) instead of `--instance` —
  no extra flag, but unreadable service names and an identity that silently changes when the config
  file moves.
- **Environment-variable configuration baked into the unit** (as the supervisor lineage on `main`
  does) — rejected here: this Client is file-configured by ADR-0008; duplicating settings into the
  unit would create a second, diverging source of truth. The unit carries the config path only.
- **`anyhow` for error handling** (as the `main` lineage uses) — rejected; the crate uniformly uses
  `Result<_, String>` and the new module maps library errors to strings at its boundary.

## Sources / Prior art

- **This repository's `main` lineage** — a working single-binary implementation of the same problem
  for the supervisor host: `main:crates/supervisor/src/service/` (clap subcommands,
  `service-manager` lifecycle, `windows-service` SCM shim, `ServiceControl` seam) and
  `main:crates/supervisor/src/update/layout.rs` (versioned `versions/<sha256>/` + `current`
  pointer), with its design records `main:docs/adr/0006-supervisor-host-os-service-and-cli.md` and
  `main:docs/adr/0007-in-place-self-update-with-rollback.md`. This ADR ports that design and
  extends it with per-instance identity and operator-chosen roots; those records also flag the
  launchd `KeepAlive` restart-on-stop caveat adopted below.
- `service-manager` crate (systemd/launchd/Windows SCM; `ServiceLevel`, `RestartPolicy`):
  <https://docs.rs/service-manager/> — v0.11.0 (2026-02-18) verified current and maintained
  (checked 2026-07-23), and verified against `main`'s lockfile to pull no TLS/crypto backends, so
  ADR-0007's rustls/ring-only constraint holds. Backend caveats from its changelog and tracker:
  `RestartPolicy` rework in 0.9–0.11, launchd `install` no longer auto-starting since 0.10, limited
  `sc.exe` restart support, and the open launchd status bug
  <https://github.com/chipsenkbeil/service-manager-rs/issues/41>.
- `windows-service` crate (`define_windows_service!`, control handler, `SetServiceStatus`):
  <https://docs.rs/windows-service/> — 0.8.1 (2026-05) current; used in production by Vector
  (its hand-rolled `vector service install` on Windows,
  <https://github.com/vectordotdev/vector/pull/2896>) and maintained by Mullvad for their own
  daemon. Microsoft's younger Windows-only `windows-services` runtime crate
  (<https://crates.io/crates/windows-services>) is a watched possible successor for the shim role.
  Windows error 1053 (service must report status to the SCM):
  <https://learn.microsoft.com/en-us/answers/questions/1389851/>; `SERVICE_STATUS` wait-hint and
  checkpoint semantics:
  <https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_status>.
- Versioned-dir + pointer self-update prior art: Elastic Agent's `data/elastic-agent-<version>-<hash>/`
  dirs, top-level symlink, upgrade marker and watcher —
  <https://github.com/elastic/elastic-agent/blob/main/docs/upgrades.md> (both its
  `<component>-<version>-<hash>` directory naming — introduced in 8.13.0 for operator readability —
  and the torn-swap lessons of elastic-agent#2264 and beats#27342 are adopted here); Scoop's
  `current` junction alias
  (<https://github.com/ScoopInstaller/Scoop/wiki/The-'Current'-Version-Alias>); the Chromium/Omaha
  updater's side-by-side versioned installs with a crash-recoverable swap bit
  (<https://chromium.googlesource.com/chromium/src/+/main/docs/updater/design_doc.md>);
  Squirrel.Windows `app-<semver>/` dirs. The OTel `opampsupervisor` specifies overwrite-with-backup
  and has not shipped package updates — the versioned layout here is deliberately the stronger
  pattern. Atomic symlink replacement (temp + `rename`):
  <https://blog.moertel.com/posts/2005-08-22-how-to-change-symlinks-atomically.html>.
- systemd: unit-name grammar and 255-char limit
  (<https://man7.org/linux/man-pages/man5/systemd.unit.5.html>), `SIGTERM`-then-`SIGKILL` stop with
  90 s default timeout (<https://man7.org/linux/man-pages/man5/systemd.kill.5.html>), `SIGHUP`
  reserved for reload (<https://man7.org/linux/man-pages/man7/daemon.7.html>), `Restart=on-failure`
  as the recommended choice for long-running services
  (<https://man7.org/linux/man-pages/man5/systemd.service.5.html>). Multi-instance precedent:
  template units (`openvpn-server@`, `wg-quick@`, `postgresql@`) are the systemd-native idiom;
  programmatically generated independent units are equally accepted practice
  (<https://icinga.com/blog/managing-multiple-service-instances-with-a-systemd-generator/>) and are
  what a label-based cross-platform installer can express.
- launchd: `LaunchDaemon` vs `LaunchAgent`, `RunAtLoad`, `KeepAlive`, `SIGTERM` with 20 s
  `ExitTimeOut`: <https://www.launchd.info/>; label convention and `<Label>.plist` file naming:
  launchd.plist(5).
- Windows service naming: ≤ 256 chars, no slashes
  (<https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-getservicekeynamea>);
  multi-instance precedent — Telegraf `--service-name`/`--service-display-name`
  (<https://github.com/influxdata/telegraf/blob/master/docs/WINDOWS_SERVICE.md>) and SQL Server
  named instances (`MSSQL$INSTANCE`); shutdown budget `WaitToKillServiceTimeout` ≈ 5 s
  (<https://kb.firedaemon.com/support/solutions/articles/4000086193-increasing-service-shutdown-time>).
- `clap` derive: <https://docs.rs/clap/latest/clap/_derive/index.html> — 4.x current (4.6.4,
  2026-07); no clap 5 exists.
- Comparable agents' service UX — Telegraf `service install`; Elastic Agent `install`/`enroll`; the
  OTel `opampsupervisor`:
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor>.
- Specification Mission, Strategy, and Goals #10/#11
  ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)); [ADR-0003](0003-client-modes-and-connection-multiplexing.md)
  (one Client binary), [ADR-0007](0007-dual-transport-and-tls.md) (rustls/ring dependency
  constraint), [ADR-0008](0008-toml-configuration.md) (`--config`-pointed TOML file).

## Consequences

- Positive: one self-contained binary installs, controls, and deregisters itself as a native
  service on all three platforms, from any location, any number of times with independent
  configurations — and still runs standalone in the foreground. Graceful shutdown now covers
  `SIGTERM`, so a service stop sends the OpAMP `agent_disconnect` goodbye instead of being killed.
  A future self-update needs no re-registration: it stages a version and switches `current`, using
  the narrow `ServiceControl` seam.
- Negative / trade-offs: three new dependencies (`clap`, `service-manager`, target-gated
  `windows-service`) plus `sha2` for content addressing; a Windows-only runtime path Unix never
  exercises; system-scope installs need root/Administrator and must fail with a clear message. The
  managers differ in ways the code handles, not papers over — the SCM marker argument, launchd's
  `KeepAlive` restart-on-stop semantics (must hold the service down after an explicit stop; verify
  on real hardware), launchd `status` being advisory until the upstream bug is fixed, `install` not
  auto-starting on launchd, and the `sc.exe` backend not expressing the restart policy (native SCM
  failure actions are the escape hatch if needed). Antivirus scanners can transiently lock freshly
  staged executables on Windows; the pointer swap retries with backoff. Real service registration
  cannot run in CI: CI gains a Windows/macOS compile-lint-test job for the client, while runtime
  behaviour needs a documented manual smoke checklist. Two instances pointed at the *same* explicit
  `--root` would fight over `current`; roots must be per-instance (the defaults are). `uninstall`
  deregisters only and never deletes the layout or state — and because updates stage new version
  directories, a retention policy is deliberately deferred to the update decision (until then the
  layout holds at most the installed versions an operator created). The SCM discards the service's
  stderr, so Windows service logs are effectively lost for now.
- Follow-ups: a follow-up ADR on the in-place self-update mechanism on top of this layout and seam
  — wire-driven staging with signature verification, a health gate with rollback (prior art to
  weigh there: Elastic Agent's watcher-from-the-new-version and upgrade marker, OpAMP's
  "mark version bad" memory, Android A/B `successful`-flag semantics), and version-dir pruning /
  retention; a follow-up on log-to-file for service mode (Windows especially) and possibly config
  reload on `SIGHUP`; possibly `Type=notify`/watchdog integration once an update health gate wants
  it.
