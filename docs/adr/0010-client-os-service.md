# ADR-0010: The Client as a multi-instance native OS service, installed and run through its own CLI

- **Status:** 🟢 accepted
- **Date:** 2026-07-23
- **Deciders:** Markus Brigl

## Context

### The service, its CLI and its layout

The specification commits the Client to run as first-class infrastructure: it "installs as a native
operating-system service that updates itself in place, and runs on Linux, macOS, and Windows"
(Mission; Strategy), and **Goal #11** requires exactly that — install and run as a native OS service
on all three platforms, with an in-place self-update that survives the service restart. Without this
decision the Client is none of that: `crates/client/src/main.rs` is a hand-rolled two-flag parser
(`--config`, `--version`) in front of a `#[tokio::main]` loop, shutdown is `ctrl_c()` only (systemd's
`SIGTERM` would hit the kill timeout and skip the `agent_disconnect` goodbye), there is no Windows SCM
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

### The first configuration

The service lifecycle below gives the operator `client service install | uninstall | start | stop |
status`, one CLI over the `service-manager` crate, writing the systemd unit (Ubuntu and RHEL are both
systemd), the launchd `LaunchDaemon`, and the Windows SCM registration;
[ADR-0018](0018-client-self-update.md) closes the last platform gap by configuring the SCM recovery
actions the `sc.exe` backend discards. That half of the operator's question — *does this work the
same on all four systems* — is answered.

The **configuration** half is not answered by the lifecycle alone: the Client is file-configured
([ADR-0008](0008-toml-configuration.md)) and the installed unit carries the config *path*, never the
config. On a fresh host that path names nothing, and nothing says so. `ClientConfig::load` returns
the defaults when the file is absent, so `service install` succeeds, the service starts, and the
Client dials `ws://127.0.0.1:4320/v1/opamp` — the development default — forever. The host is not
broken and it is not managed either. That silent half-state is the defect the first-configuration
points below close; it is the same reason ADR-0018 refused to let a Windows
self-update look like it worked on two platforms out of three.

Three forces shape the answer.

**There is nothing on the host to copy from.** [ADR-0021](0021-release-pipeline-and-artifacts.md)
packs each release asset around the binary the Client looks for (`client` / `client.exe`), precisely
so a release asset is also a valid package artifact for ADR-0018. `config/client.toml` is a document
in this repository, not a shipped file. An operator who follows the documented download path has a
binary and no example.

**`install` already runs unattended.** It is the command an Ansible play, an MDM profile, or an MSI
wrapper invokes. A command that blocks on stdin there does not fail — it hangs, which is worse. Any
interactivity has to be something the operator asks for, never something they discover.

**A first configuration contains a secret.** The endpoint is useless without the credential that goes
with it ([ADR-0013](0013-agent-admission-on-the-opamp-endpoint.md)), and asking for a bearer token or a
password puts it on the screen unless the terminal is told otherwise. Suppressing the echo is
platform terminal control (`termios` on Unix, `SetConsoleMode` on Windows), not string handling.

None of this overturns ADR-0008. What gets written is a *starting* file the operator keeps editing by
hand; the Client gains no second configuration mechanism, and no environment fallback. But a new CLI
surface and a new dependency are architecture-relevant under `AGENTS.md` §3, which is why this is an
ADR and not a commit.

There is no upstream answer to adopt wholesale. The OpenTelemetry `opampsupervisor` has a
hand-written configuration file and no scaffolding command at all. Elastic Agent is the closest prior
art and is instructive in both directions: `elastic-agent install` prompts for confirmation and for
Fleet enrollment by default, and offers `--non-interactive` for automation — but it also *overwrites*
`elastic-agent.yml`, and `--force` exists to skip the confirmation for that. Overwriting is exactly
the behaviour we must not copy on a file that holds a credential an operator typed.

### The service log

**On Windows, a Client installed as a service has no logs without a file.** Everything this Client
says goes to stderr through one `tracing_subscriber::fmt` layer installed in
[`main.rs`](../../crates/client/src/main.rs), and the Windows SCM discards a service's stderr. The
[`README.md`](../../README.md) says so in the manual smoke checklist.

Windows is not a lesser target here. [ADR-0022](0022-the-client-is-named-opamp-fleet-client.md) gives it
the same service name as every other platform, the interactive install above treats it like every
other platform, and it has an elevation model of its own that the service integration above goes to
some trouble to satisfy. It is a supported platform on which a failed start produces nothing anyone
can read unless the Client writes a file.

**The obvious substitute does not cover the case.** Through [ADR-0024](0024-agents-report-their-own-telemetry.md)
the Client can bridge its own `tracing` output to OTLP logs — but only to a destination *the Server
offers*, over a connection that must already be up. The failures that most need a log are exactly
the ones where it is not: a malformed `client.toml`, a certificate the Client will not load, an
endpoint that refuses it, a self-update that will not come back. Own-telemetry is for a Client that
is working; the service log is for one that is not.

**Three forces bound the answer.**

- **The log must be bounded.** This runs on a hundred machines that nobody watches. An unbounded
  file on a fleet host eventually fills a disk, and a monitoring agent that takes the host down is
  worse than one that says nothing.
- **The destination is not known when logging starts.** `tracing` takes one subscriber per process
  and [`main.rs`](../../crates/client/src/main.rs) installs it before `cli::parse()` runs, so the
  instance — and therefore the state directory the file belongs in — is not known yet. ADR-0024 hit
  this exact wall and solved it with a `reload::Layer` slot filled later.
- **A rotation implementation is a dependency or a hazard.** Rolling files, pruning old ones, and
  surviving concurrent writes is not code worth writing here, and getting it subtly wrong in the
  component that is supposed to explain failures is its own failure mode.

## Decision

### The service, its CLI and its layout

We will turn the Client into a **`clap` subcommand CLI** that registers, controls, and runs *itself*
as a native OS service on Linux (systemd), macOS (launchd), and Windows (SCM), parameterized by an
explicit instance name, with all daemon code isolated in one `service` module:

- **Subcommands:** `run` (foreground daemon — the default when no subcommand is given, so
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
- **The version in all of this is [ADR-0009](0009-version-baking-comparison-and-display.md)'s.** The
  `versions/opamp-client-<MAJOR.MINOR.PATCH>-<hash>` directory names, the CLI `--version` output,
  and the OpAMP `service.version` attribute all call the single `version()` helper decided there —
  how the string is computed is entirely ADR-0009's contract, this decision only consumes it (and
  renders base and commit into directory names per the naming rule above).
- **Module shape:** everything above lives in `crates/client/src/service/` (`mod.rs` with the narrow
  `ServiceControl` seam — `start`/`stop`/`state` — plus `runtime.rs`, `layout.rs`, `manager.rs`,
  and the Windows-only `windows.rs`), so a future updater depends on the seam, not on service
  internals. Errors stay `Result<_, String>` in the crate's existing style; no `anyhow`.

The update mechanism itself (staging new versions over the wire, health gate, rollback, pruning old
versions) is **out of scope** and deferred to a follow-up decision; this decision only guarantees
the shape it needs. Richer unit/plist tuning (systemd `Type=notify`, launchd throttling) is deferred.

### The first configuration

We will let `client service install` write the Client's **first** configuration file — asked
interactively only when the operator asks for it, never overwriting a file that exists, and validated
before anything is registered.

1. **Interactivity is an opt-in flag on `install`, not a new command.** `client service install
   --interactive` runs the questionnaire; without the flag `install` behaves exactly as the service
   lifecycle above defines it. The default stays non-interactive because that is what every existing
   scripted invocation already relies on. `--interactive` on a stdin that is not a terminal is an
   **error**, not a silent fallback: `std::io::IsTerminal` has been in `std` since Rust 1.70 and the
   workspace MSRV is 1.97, so this costs no dependency and turns "hangs a deploy forever" into a
   message.

2. **A file that exists is never overwritten.** If the target already exists, the questionnaire is
   skipped, `install` proceeds with that file, and prints which file it kept. Re-installing stays
   idempotent, as the version layout above requires, and a second `--interactive` install can
   never eat the credential typed into the first.

3. **When the operator names no path, the file goes to `<root>/client.toml`** — inside the install
   root the layout above already derives per scope and instance. That is one rule for four platforms
   instead of a new `/etc` vs `/Library` vs `%ProgramData%` policy, and the absolute path baked into
   the unit is the one just written. An explicit `--config` wins; since that flag has a default value,
   the two cases are distinguished by clap's value source, not by comparing against the default
   string.

4. **The questionnaire asks only what has no useful default on a fresh host:** the Server `endpoint`,
   the Agent `name`, whether authentication is used and under which scheme (ADR-0013), and — only
   when the endpoint scheme is `wss://` or `https://` — the CA file for a private CA
   ([ADR-0007](0007-dual-transport-and-tls.md)). `[self_update]` (ADR-0018) is offered last and
   defaults to **no**: consent for the Server to replace the binary that manages every other binary
   on the host is the larger grant, and it stays a deliberate answer rather than a default one.
   Everything else is written as commented defaults, in the shape of `config/client.toml`, so the
   file remains a starting point for hand-editing.

5. **The written file is treated as holding a secret:** mode `0600` on Unix; on Windows it inherits
   the install root's ACL, which for a system-scope install is already administrator-owned.

6. **It is validated before the service is registered.** The order is write → load through
   `ClientConfig::load` → lay out the versioned install → register. This preserves the existing
   "fail on a broken configuration now, not at the service's first start" property. A file that
   fails to load is left on disk and named in the error, never silently deleted — a typo is corrected
   by editing, not by answering five questions again.

7. **A non-interactive install with no configuration file warns.** Not an error — automation must not
   break — but a printed line naming the path that will be baked into the unit and saying the Client
   will run on defaults until that file exists. The silence is the bug.

8. **Hidden input comes from `dialoguer`.** It is the one part worth a dependency: the platform
   terminal control behind a password prompt is not something to hand-roll for three operating
   systems. Version 0.12.0 (2025-08-23) is current and maintained, it is MIT-licensed, and it brings
   no TLS or crypto backend, so ADR-0007's constraint on the rustls/ring stack is untouched.

### The service log

We will write the Client's own log to a **rotating file in its state directory whenever it runs as a
service**, on every platform, bounded by a retention it cannot exceed, and switchable from
`client.toml`.

9. **Service mode turns it on; the foreground does not.** `run --service` writes the file; an
   ordinary `opamp-fleet-client run` in a terminal keeps writing to stderr alone, because somebody
   is already reading it there. The condition is "no terminal is watching", which is exactly what
   service mode means.

10. **Every platform, not Windows alone.** systemd and launchd do capture stderr, so on Linux and
   macOS the file duplicates what `journalctl` and `log show` already hold — deliberately. ADR-0022
   made the same trade for the service name: one behaviour an operator can carry between platforms is
   worth more than the bytes a platform-specific rule would save, and the support instruction becomes
   one sentence instead of three. The duplicate is bounded by point 12 and can be switched
   off by point 13.

11. **It lives in the state directory, beside the rest of the instance's state.** The install layout
   above gives every instance a private directory that survives updates and that `uninstall`
   deliberately does not delete, which is precisely the lifetime a log wants — a log that vanished
   with the failed install would be missing when it is needed. Elastic Agent puts its own logs under
   its data directory for the same reason.

12. **Bounded by rotation and retention, always.** The file rotates **daily** and the Client keeps a
   fixed number of days (default 7), deleting the rest. There is no configuration that removes the
   bound — `keep = 0` is refused rather than read as "forever", because "forever" is the setting
   that fills a disk on a host nobody is looking at.

   This bounds **age and file count, not bytes**, and that is a conscious narrowing of Elastic's
   model, which rotates at a size (10 MB by default) and keeps `keepfiles`. A day is bounded here in
   practice because the Client is quiet — a report every 30 s at `info` — and because its one
   pathological case, a Server it cannot reach, backs off exponentially rather than looping hot. If
   a real deployment ever produces a day large enough to matter, a size cap is a follow-up, and it is
   an easier one to add than a retention policy would have been to remove.

13. **`[logging]` in `client.toml`, and it is the machine's.** `dir` moves the file, `keep` changes
   how many days are kept, and `enabled = false` turns it off for an operator whose platform already
   collects stderr and who does not want the copy. It is **not** remotely configurable and never
   arrives over the wire: a Server that could redirect or silence a Client's own log could hide its
   own effects, and the Client's configuration is the machine's (the rule ADR-0014 draws).

14. **The file layer is dropped into a reload slot, like the OTLP bridge.** `main.rs` keeps installing
   stderr first and reserves a second empty `reload::Layer`; once `cli::parse()` has named the
   instance and the layout is known, the file layer is loaded into it. The same mechanism ADR-0024
   already established, for the same reason — and it means the messages emitted before that point
   still reach stderr rather than disappearing into an ordering bug.

15. **`tracing-appender` does the rolling.** It is `tokio-rs/tracing`'s own appender, the sibling of
   the `tracing-subscriber` this project already depends on, and it provides daily rotation with a
   retained-file limit directly. Writing this by hand to avoid one dependency would put custom file
   rotation inside the component whose job is to explain failures.

16. **A log that cannot be written never stops the Client.** If the directory is not writable — a
   permission the installer did not grant, a full disk — the Client says so on stderr once and runs
   without the file. A monitoring agent that refuses to start because it could not open its own log
   has turned a diagnostic into an outage.

## Alternatives considered

### The service, its CLI and its layout

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

### The first configuration

- **A separate `client config init` command** — a second entry point for work the operator is already
  in the middle of when they run `install`, and a second place where the config path must be
  resolved. Rejected for the flag; the cost is recorded under Consequences.
- **Interactive by default, with `--non-interactive` to opt out** (Elastic Agent's shape) — rejected
  because our `install` is *already* the scripted command. Elastic can afford that default because
  their install is the documented first contact with the product; flipping ours would hang every
  existing automated invocation.
- **Overwrite an existing file, with `--force` to confirm** (what Elastic does to
  `elastic-agent.yml`) — rejected: a re-install that discards a credential the operator typed is a
  worse failure than one that refuses to write.
- **Plain `stdin().read_line()`, no dependency at all** — the simplest thing, and it echoes the
  bearer token onto the screen and into the scrollback. Rejected on that point alone.
- **`inquire` 0.9.4 (2026-02-24)** — actively maintained and richer (editor prompts, derive macros
  for enum menus), which is more than four questions need, at roughly a third of `dialoguer`'s
  adoption. **`cliclack`** — a styled multi-step experience we do not need. Both remain viable if
  `dialoguer` ever stops being maintained; nothing outside the questionnaire module would change.
- **Ship `config/client.toml` inside the release artifact** — rejected: ADR-0021 keeps the asset
  installable as a package artifact, and an example copied onto the host still has to be edited
  before the service does anything, which is the actual problem.
- **Environment variables for the first configuration** — already refused by ADR-0008 and by the
  service decision above; nothing here reopens it.

### The service log

- **The Windows Event Log**, the platform-native answer, which Elastic Agent supports as an output
  (`syslog`, `file`, `stderr`, `eventlog`). Rejected on cost and on evidence: it needs an event source
  registered at install time and a message resource for anything better than raw text, it is a second
  code path that only one platform exercises, and the OpenTelemetry Collector's own Event Log output
  is reported as *"somewhat broken"* by the people who use it. A file is inspectable with the tools
  everyone already has, and is identical on all three platforms.
- **A file only on Windows**, since Linux and macOS already capture stderr. Genuinely smaller, and the
  duplicate storage of point 10 disappears. Rejected: it makes the answer to "where are
  the logs" depend on the platform, which is what ADR-0022 argued against for the service name, and it
  leaves the operator who runs the Client in a container — where neither journald nor an SCM is
  present — with the same hole the service log closes.
- **Rely on the OTLP own-logs bridge** (ADR-0024) and require operators to configure a destination.
  Rejected in the Context: it needs a Server that is reachable and an offer already made, so it cannot
  explain the failures that prevent exactly that. It remains the right channel for a healthy fleet.
- **Rotate by size instead of by day**, as Elastic does. Considered and narrowed to point
  12's daily rotation: size rotation is the stronger bound but `tracing-appender` does not
  offer it, so taking it means either a second dependency or hand-written rolling. The Client's volume
  does not warrant either today, and the follow-up is cheap.
- **Let `keep = 0` mean unlimited retention**, the convention many tools use. Rejected: on a fleet host
  the unbounded setting is the one that eventually causes an incident, and it should not be reachable by
  typing a zero.
- **Make logging remotely configurable**, so an operator can raise a Client's log level from the Server
  while chasing a problem. Attractive, and the natural extension. Rejected here: it is the Client's own
  configuration, which ADR-0014 keeps on the machine, and a Server able to silence or redirect a
  Client's log could conceal what it did. If it is ever wanted, it wants its own decision.
- **Write the log into the version directory rather than the state directory**, keeping everything one
  install produced together. Rejected: a self-update would then scatter the history across version
  directories and ADR-0016's rollback would take the log with it — the state directory is the thing
  that deliberately outlives both.

## Sources / Prior art

- **This repository's `main` lineage** — a working single-binary implementation of the same problem
  for the supervisor host: `main:crates/supervisor/src/service/` (clap subcommands,
  `service-manager` lifecycle, `windows-service` SCM shim, `ServiceControl` seam) and
  `main:crates/supervisor/src/update/layout.rs` (versioned `versions/<sha256>/` + `current`
  pointer), with its design records `main:docs/adr/0006-supervisor-host-os-service-and-cli.md` and
  `main:docs/adr/0007-in-place-self-update-with-rollback.md`. This decision ports that design and
  extends it with per-instance identity and operator-chosen roots; those records also flag the
  launchd `KeepAlive` restart-on-stop caveat adopted here.
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

- [Elastic Agent command reference](https://www.elastic.co/docs/reference/fleet/agent-command-reference)
  — `install` prompts for confirmation and enrollment, `--non-interactive` for automation, `--force`
  to overwrite `elastic-agent.yml` without prompting. The closest prior art for the first
  configuration, and the source of the two behaviours deliberately inverted here (default and
  overwrite).
- [`dialoguer` on crates.io](https://crates.io/crates/dialoguer) — 0.12.0, published 2025-08-23, MIT.
- [`inquire` on crates.io](https://crates.io/crates/inquire) — 0.9.4, published 2026-02-24.
- [Comparison of Rust CLI prompts: cliclack, dialoguer, promptly, inquire](https://fadeevab.com/comparison-of-rust-cli-prompts/)
  — the field, side by side.
- [`std::io::IsTerminal`](https://doc.rust-lang.org/stable/std/io/trait.IsTerminal.html) — in `std`
  since 1.70; no `atty`/`is-terminal` dependency is needed for the TTY guard.
- [Command Line Applications in Rust — Communicating with machines](https://rust-cli.github.io/book/in-depth/machine-communication.html)
  and [Improving CLIs with isatty](https://blog.jez.io/cli-tty/) — the convention that a program
  behaves differently, and predictably, when it is not talking to a person.
- The OpenTelemetry `opampsupervisor`, whose configuration is hand-written and which offers no
  scaffolding command — the same absence of an upstream answer ADR-0018 found for self-update.

- **[Elastic Agent logging](https://www.elastic.co/docs/reference/fleet/elastic-agent-standalone-logging-config)**
  and [installation layout](https://www.elastic.co/guide/en/fleet/master/installation-layout.html) — the
  closest working precedent for the service log. It writes its own logs under its data directory
  (`C:\Program Files\Elastic\Agent\data\elastic-agent-*\logs\elastic-agent-*.ndjson` on Windows), can
  emit to `syslog`, `file`, `stderr` or `eventlog`, and rotates **by size** — 10 MB by default — keeping
  a configurable number of files, with names carrying the date and a sequence suffix. Points
  11 and 12 follow it; point 12 deliberately takes the
  age-and-count bound instead of the size one, and says why.
- **[OpenTelemetry Collector issue #5300, "HowTo: Telemetry Logging to File when running as windows
  service"](https://github.com/open-telemetry/opentelemetry-collector/issues/5300)** — this exact
  problem, filed against the Collector, and the confirmation that it is a general one rather than a
  peculiarity of this project. The Collector's answer is a configured output path
  (`service::telemetry::logs::output_paths`), which is the shape point 13 takes. The same
  discussion is where the Collector's Windows Event Log output is described as unreliable, which is
  the evidence behind rejecting that route.
- **[`tracing-appender`](https://crates.io/crates/tracing-appender)** `0.2.5` — `tokio-rs/tracing`'s own
  rolling file appender, beside the `tracing-subscriber` `0.3.23` already in the tree. Daily rotation
  with a retained-file limit out of the box; no size-based rotation, which is what narrowed point
  12.
- This repository: [ADR-0024](0024-agents-report-their-own-telemetry.md), whose `reload::Layer` slot
  point 14 copies and whose OTLP bridge the service log complements rather than replaces;
  [ADR-0022](0022-the-client-is-named-opamp-fleet-client.md), whose one-behaviour-per-platform argument
  point 10 reuses; [ADR-0014](0014-server-driven-connection-settings.md), for the rule that
  the Client's own configuration stays on the machine.

## Consequences

### The service, its CLI and its layout

- Positive: one self-contained binary installs, controls, and deregisters itself as a native
  service on all three platforms, from any location, any number of times with independent
  configurations — and still runs standalone in the foreground. Graceful shutdown covers
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
  layout holds at most the installed versions an operator created).
- Follow-ups: a follow-up ADR on the in-place self-update mechanism on top of this layout and seam
  — wire-driven staging with signature verification, a health gate with rollback (prior art to
  weigh there: Elastic Agent's watcher-from-the-new-version and upgrade marker, OpAMP's
  "mark version bad" memory, Android A/B `successful`-flag semantics), and version-dir pruning /
  retention;
  possibly config reload on `SIGHUP`;
  possibly `Type=notify`/watchdog integration once an update health gate wants it.

### The first configuration

- **Positive:** a fresh host goes from a downloaded binary to a working, registered service with one
  command, identically on Ubuntu, RHEL, macOS, and Windows. The default-endpoint service that looks
  installed and manages nothing stops being reachable by accident. The credential is typed into a
  hidden prompt instead of a command-line flag, so it never lands in shell history or a process list.
- **Negative / trade-offs:** point 3 changes what `install` bakes without `--config`:
  not `client.toml` resolved against the working directory, but `<root>/client.toml`. Resolving
  against the working directory only ever worked when `install` was run from exactly the directory
  holding the file, which is not a property a service can depend on — but a host installed that way
  must name the path explicitly or move the file, and the change belongs in the changelog rather than
  in a release note nobody reads. One new dependency (`dialoguer`) and the terminal handling it
  brings.
  A configuration cannot be generated without installing — an operator who wants only the file must
  write it by hand or install and then uninstall. The questionnaire becomes a second place that has
  to follow the configuration schema, bounded to the handful of keys it asks about. `--interactive`
  is unavailable where it would be most tempting and least appropriate — container builds, image
  bakery, unattended provisioning — which is the point, but it means those paths still ship a config
  file by their own means.
- **Follow-ups:** whether `uninstall` should offer to remove a configuration file it wrote (it
  deletes nothing, deliberately, and that asymmetry is visible); whether the same questionnaire
  should back a later `config validate` / `config show`; and the operator manual's install
  walkthrough, which documents the new flag and the written path at implementation time.

### The service log

- Positive: a Windows service failure becomes readable. The platform that had nothing gets the same
  answer as the others, and the README's manual checklist need not warn that logs are lost.
- Positive: the answer to "where are the logs" is one sentence on every platform, including in a
  container, where neither journald nor an SCM is present.
- Positive: it covers the failures own-telemetry structurally cannot — the ones that happen before or
  instead of a working connection.
- Negative / trade-offs: on Linux and macOS the file duplicates what the platform's journal already
  holds, costing disk for a copy most operators will never read. Bounded by point 12 and
  switchable by point 13, but it is a real cost paid by every host to keep one behaviour
  uniform.
- Negative / trade-offs: **the bound is on days and files, not bytes.** A pathologically chatty day
  produces a large file, and nothing stops it before the next rotation. The reasoning in point
  12 is that the Client is quiet and backs off when it cannot reach the Server; if that
  reasoning is ever wrong, it is wrong on a host nobody is watching, which is where it hurts most.
- Negative / trade-offs: a new dependency in the Client, on a crate whose whole job is writing files
  in the background. It is from the same project as the logging stack already in use, which is the
  reason it is acceptable rather than an argument that it is free.
- Negative / trade-offs: logs on disk are a data question. Anything the Client logs about a
  configuration or a package persists on the host for a week rather than living in a journal an
  operator's policy already governs. Nothing secret is logged today, and this decision makes it more
  important that it stays that way.
- Follow-ups: a size cap beside the day count, if a deployment ever produces a day worth capping;
  whether `uninstall` should offer to remove a log directory it created, which is the same asymmetry
  noted above for the configuration file the install writes; and remotely raising a Client's log
  level while chasing a problem, which is a decision of its own for the reasons this one rejects it.
