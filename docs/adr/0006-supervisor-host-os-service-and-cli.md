# ADR-0006: Supervisor Host as a cross-platform OS service with a subcommand CLI

- **Status:** 🟢 accepted
- **Date:** 2026-07-20
- **Deciders:** Maintainer

## Context

The specification commits the Supervisor Host to run as first-class infrastructure: it "installs as a
native operating-system service … and runs on Linux, macOS, and Windows" (Strategy; Core Concepts,
"Supervisor Host"), and **Goal #11** requires it to "install and run as a native operating-system
service on Linux, macOS, and Windows." Today the binary is none of that — `crates/supervisor/src/main.rs`
is a bare `#[tokio::main]` poll loop configured only through environment variables
(`OPAMP_SERVER_URL`, `OPAMP_STATE_DIR`, `OPAMP_POLL_SECONDS`), with no CLI, no way to register or
control an OS service, and no signal handling. To close the gap the single `supervisor-host` binary must
be able to **register, start, stop, and deregister itself** as a service on all three platforms, and
still **run standalone in the foreground** for development and for hosts that do not want a system
service.

Three forces shape the decision. First, the maintainer's requirement is explicit that *one* binary
carries all of this functionality — so the service management is subcommands of `supervisor-host`, not a
separate tool. Second, the three platforms have genuinely different service models: systemd and launchd
supervise an ordinary foreground process, whereas the **Windows Service Control Manager (SCM)** launches
the process and then expects it to report status back over the SCM protocol within ~30 seconds or it is
killed with error 1053 — so "run under the service" is not the same code path on Windows as on Unix.
Third, introducing a CLI parser, a cross-platform service-management library, and a Windows-service
runtime library are all new dependencies and a new process/interface surface, which per AGENTS.md §3
require an ADR before any code. The specification's "close the loop before widening it" and
"Simplicity first / YAGNI" bound the scope: service *lifecycle* now, not every tunable of every backend.

## Decision

We will turn `supervisor-host` into a **`clap` subcommand CLI** and add native OS-service integration
across Linux, macOS, and Windows from the one binary:

- **Subcommands:** `run` (foreground daemon — the default when no subcommand is given, preserving
  today's "just run it" behaviour), `service install | uninstall | start | stop | status`, and `update`
  (the self-update entrypoint decided separately in ADR-0007). Configuration flags carry environment
  fallbacks (`#[arg(long, env = "OPAMP_SERVER_URL")]`, etc.) so the existing env-only configuration keeps
  working unchanged; `service install` captures the current configuration into the generated unit so the
  installed service is self-contained.
- **Lifecycle** (`install`/`uninstall`/`start`/`stop`/`status`) is implemented over the **`service-manager`**
  crate, which targets the platform's native manager (systemd, launchd, Windows SCM, rc.d, OpenRC) behind
  one API. The default is a **system-level** service (systemd system unit / launchd `LaunchDaemon` /
  Windows `LocalSystem`), because the Supervisor Host manages machine-wide agents and must run without a
  logged-in user and start at boot; `--user` is offered as an opt-in for development.
- **Running under the manager** is a plain foreground process on Linux (systemd `Type=simple`) and macOS
  (launchd `RunAtLoad`+`KeepAlive`) — the same `run` loop plus graceful `SIGTERM`/`SIGINT` shutdown via
  `tokio::signal` (feature already enabled). On **Windows only**, a `cfg(windows)` runtime shim built on
  the **`windows-service`** crate registers the SCM control handler and reports `StartPending`→`Running`
  →`Stopped`, so the service does not die with error 1053; the shim then runs the identical daemon body.
  Because a process cannot reliably detect that the SCM started it (`StartServiceCtrlDispatcher` fails
  with error 1063 when *not* SCM-launched), **`service install` sets a deterministic marker argument**
  (the installed command line is `supervisor-host run --service`); that flag — and only that flag —
  routes into the `windows-service` dispatcher, while a bare `run` stays a foreground process.
- **The installed service points at the `current` pointer, not a fixed binary path.** `service install`
  lays out the versioned-install pointer ADR-0007 needs (`current` → active version dir; a symlink on
  Unix, a directory junction on Windows) and sets the service's program to `current/supervisor-host`, so
  a self-update is a pointer switch with no reinstall.
- **Stop semantics differ by manager and must hold the service down during a self-update.** After an
  explicit stop systemd and the Windows SCM keep the unit stopped; **launchd `KeepAlive` restarts it**,
  so `service stop` on macOS uses `bootout`/`kickstart` (disable-then-act) rather than a plain `stop`
  that launchd would immediately undo. ADR-0007's Updater relies on this to swap the binary without the
  manager racing it back up.

Any richer unit/plist tuning (systemd `Type=notify`/`WatchdogSec`, launchd throttling) is **deferred**.

## Alternatives considered

- **A separate service-management binary or an OS-specific installer** (`.deb`/`.pkg`/MSI wrapping
  `sc.exe`) — spreads the logic across artifacts and contradicts the maintainer's "one binary contains
  all functionality"; subcommands keep a single deployable that manages itself.
- **Hand-write the three backends** (emit systemd units, launchd plists, call `CreateService`/
  `DeleteService`) — maximal control but three code paths to own and test; `service-manager` already
  abstracts exactly install/start/stop/uninstall across them, and its escape hatches cover the rest when
  needed (YAGNI until then).
- **A generic daemonizing crate (`daemonize`) or double-fork** — only forks/detaches on Unix and gives
  nothing on Windows, and modern init systems (systemd, launchd) *want* a foreground process they
  supervise, not a self-daemonizing one; it would fight the platform rather than use it.
- **Skip the Windows SCM shim and run as a console process** — the SCM would kill it with error 1053; a
  service that reports status via `windows-service` is mandatory for a real Windows service.
- **A pure-env configuration with no CLI** — keeps deps minimal but offers no place to hang the
  `service`/`update` verbs the request needs; `clap` with env fallbacks is a strict superset that keeps
  the existing env configuration working.

## Sources / Prior art

- OpAMP Supervisor Host responsibilities (native OS service, self-updating):
  <https://opentelemetry.io/docs/specs/opamp/> and the OTel `opampsupervisor`:
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor>.
- `service-manager` (systemd/launchd/Windows SCM/rc.d/OpenRC; `ServiceLevel`, `RestartPolicy`):
  <https://docs.rs/service-manager/> and <https://crates.io/crates/service-manager>.
- `windows-service` (`define_windows_service!`, control handler, `SetServiceStatus`):
  <https://docs.rs/windows-service/>. Windows error 1053 cause (a service must report status to the SCM):
  <https://learn.microsoft.com/en-us/answers/questions/1389851/>.
- systemd service types and `sd_notify` (deferred `Type=notify`):
  <https://man7.org/linux/man-pages/man3/sd_notify.3.html>; launchd `LaunchDaemon` vs `LaunchAgent`,
  `RunAtLoad`, `KeepAlive`: <https://www.launchd.info/>.
- `clap` derive with environment-variable fallback (`env` feature):
  <https://docs.rs/clap/latest/clap/_derive/index.html>.
- Prior-art service-install UX in comparable agents — Telegraf `service install`:
  <https://github.com/influxdata/telegraf/blob/master/docs/WINDOWS_SERVICE.md>.
- Specification Strategy, Core Concepts ("Supervisor Host"), and Goal #11
  ([`docs/SPECIFICATION.md`](../SPECIFICATION.md)); ADR-0003 (toolchain, `tokio`) and ADR-0004 (the
  OpAMP HTTP client the daemon runs).

## Consequences

- Positive: one self-contained binary installs, controls, and deregisters itself as a native service on
  all three platforms and still runs standalone; existing env configuration keeps working; graceful
  shutdown lets the init system stop it cleanly; the `run`/`service`/`update` split gives ADR-0007 a home.
  The service code is **isolated in a dedicated `service` module** exposing a narrow `ServiceControl`
  seam (`stop`/`start`/`status`), so ADR-0007's updater depends on that interface only — not on service
  internals — and each concern stays independently maintainable and testable.
- Negative / trade-offs: three new dependencies (`clap`, `service-manager`, `windows-service`) and a
  Windows-only runtime code path that Unix never exercises; system-level installs need root/administrator
  and must fail with a clear message rather than a raw permission error. The three managers behave
  differently in ways the code must handle, not paper over — the **SCM marker argument**, the **launchd
  `KeepAlive` restart-on-stop** semantics, and `service-manager`'s **uneven `status`/env/restart support**
  across backends — and these are runtime behaviours that CI cannot exercise. CI runs on ubuntu only
  today, so the Windows and macOS paths are neither compiled nor linted — this ADR therefore also adds a
  **cross-platform build matrix for the agent** (`supervisor` on Linux, macOS, Windows; the Server stays
  Linux-only per the specification) so those paths at least compile and lint; correct service *runtime*
  behaviour still needs manual smoke tests on real Windows and macOS hosts.
- Follow-ups: ADR-0007 builds the `update` subcommand into the self-update/rollback mechanism on top of
  `service stop`/`start`; a later ADR may adopt systemd `Type=notify`/`WatchdogSec` for true readiness
  and watchdog liveness once the update health-gate wants it.
