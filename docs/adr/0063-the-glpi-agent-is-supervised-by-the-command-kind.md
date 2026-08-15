# ADR-0063: The GLPI Agent is supervised by the `command` kind — one documented recipe per platform, no new plugin

- **Status:** 🟢 accepted
- **Date:** 2026-08-14
- **Deciders:** Markus Brigl

## Context

The fleet should manage the [GLPI Agent](https://glpi-agent.readthedocs.io/) — the inventory
agent of the GLPI asset-management suite — on Windows and Linux hosts alike: fleet-visible
health, centrally rolled-out configuration, and a restart the operator issues from the Server.

What the GLPI Agent is, from research against its documentation and source:

- A **Perl application**, not a single binary. On Linux the distribution's package installs it
  (e.g. `/usr/bin/glpi-agent` on the system Perl) with a systemd unit; on Windows an MSI
  installs it under `C:\Program Files\GLPI-Agent` with a **bundled Strawberry Perl** whose
  interpreter ships as `perl\bin\glpi-agent.exe`, registers a Windows service `glpi-agent`
  (`EXECMODE=1`, the default) that runs that interpreter with four `-I` library paths and the
  agent script `perl\bin\glpi-agent`, and exposes `glpi-agent.bat` — a batch wrapper around the
  same invocation — as the command line.
- `--daemon --no-fork` runs it as a **foreground daemon on every platform**: the launcher
  script instantiates the same platform-neutral `GLPI::Agent::Daemon` class whether or not the
  process has a console; the Win32-service integration is a separate code path the service
  wrapper uses, not a requirement of daemon mode.
- Configuration is a file, selected with `--conf-file=FILE`. There is **no signal-based
  reload**; the daemon optionally re-reads its file on a timer (`conf-reload-interval`,
  minimum 60 s, default never).
- `--version` prints `GLPI Agent (X.Y[.Z])` — and most releases carry a **two-component**
  version (`1.11`, `1.15`), which the version probe's strict SemVer 2.0.0 matcher does not
  accept.

Forces from this project's architecture:

- The supervision runtime manages **children it spawns** — spawn, watchdog, bounded stop,
  health-gated apply (ADR-0011's `Runner`). Nothing drives a foreign service manager
  (`systemctl`, `sc.exe`) for a Managed Process, and nothing supervises a process it did not
  start.
- A program named by **absolute path is machine-owned**: the Client runs it but never updates
  it, and the Supervisor's Agent does not accept packages (ADR-0021). A Server-pushed
  `[[supervisor]]` block may name only Client-owned programs, so a block with an absolute path
  is written locally in `client.toml`, never pushed (ADR-0057).
- Spawning `glpi-agent.bat` would make `cmd.exe` the supervised child and the Perl process a
  grandchild: the bounded stop would kill the wrapper and **orphan the agent**, and pid-based
  telemetry would sample the wrong process. The batch file is a footgun, not an entry point.

The question is whether any of this needs a new plugin kind — the sanctioned path of ADR-0011,
one module and one registry line — or whether the shipped `command` kind already expresses it.

## Decision

We will supervise the GLPI Agent with the **existing `command` kind — no new plugin, no code**:
one `[[supervisor]]` block per platform, running the machine's own GLPI installation as a
foreground child via `--daemon --no-fork`, documented as a recipe in the manual.

Concretely, the recipe binds:

- **The machine owns the program** (ADR-0021): the block names it absolutely — on Linux the
  packaged `/usr/bin/glpi-agent`, on Windows the MSI's bundled interpreter
  (`perl\bin\glpi-agent.exe`) invoked exactly as the MSI's own service registration does: the
  four `-I` library paths, then the agent script. Never `glpi-agent.bat`.
- **The native autostart is switched off**, so exactly one GLPI Agent runs per host: on
  Windows the MSI is installed with `EXECMODE=3` (or the service is disabled), on Linux the
  distribution's unit is `systemctl disable --now`'d. This hand-over is the operator's step and
  part of the recipe.
- **`service_name = "glpi-agent"`** is the Agent type every GLPI Configuration and package
  selector targets (ADR-0033, ADR-0054).
- **Configuration arrives as a file and applies by restart**: the block passes
  `--conf-file=${config_dir}/glpi-agent-conf` (ADR-0022), so a rolled-out Configuration named
  `glpi-agent-conf` lands exactly where the process reads it — a Configuration name carries no
  extension, following the same grammar as every other name here (ADR-0010: lowercase letters,
  digits and `-`, no dot), while `--conf-file` reads whatever path it is given. No `reload_signal` — the GLPI Agent
  has no reload signal to send.
- **`version_args = ["--version"]` is set, with a known gap**: only three-component releases
  (`1.7.1`) yield a `service.version`; two-component releases report none, because the probe
  accepts strict SemVer only. The gap is accepted — it is display metadata, and no package
  logic depends on it (the program is machine-owned).

A sketch of the Linux block, to fix the shape (the manual carries the full pair):

```toml
[[supervisor]]
type = "command"
name = "glpi"
service_name = "glpi-agent"
command = "/usr/bin/glpi-agent"
args = ["--daemon", "--no-fork", "--conf-file=${config_dir}/glpi-agent-conf"]
```

## Alternatives considered

- **A dedicated `glpi` plugin kind** carrying the platform defaults (`cfg`-gated program
  discovery, fixed arguments). Rejected: every difference between the platforms is a *value*
  the block already expresses — program path, arguments — not a *behavior* the `Runner` lacks.
  A new kind is warranted when the lifecycle differs (ADR-0011); encoding default paths in
  Rust buys convenience at the price of a module, and simplicity first says no.
- **A kind that drives the native service managers** (`systemctl`/`sc.exe` for the GLPI
  service, instead of spawning a child). Rejected: supervising a process the Client did not
  spawn is a different supervision model — no watchdog, no health gate, no bounded stop — and
  nothing in the runtime implements it. The unified-lifecycle direction (currently proposed)
  is about install/uninstall side effects, not about babysitting foreign service managers.
- **Fleet-delivered GLPI as a Client-owned package** (bare program name — ADR-0021 — which
  would also re-enable Server-pushed blocks per ADR-0057). The material exists on both
  platforms: Windows publishes a self-contained portable tree, and Linux an official
  **AppImage** — a single self-contained file that runs the agent directly when told to
  (`--script=glpi-agent`, or `GLPIAGENT_SCRIPT` in the environment; verified). Deferred all
  the same: the AppImage is x86_64-only and wants `libfuse2` on the host (or
  `APPIMAGE_EXTRACT_AND_RUN=1`, which re-extracts its ~40 MB on every start — a price the
  watchdog would pay on every restart), and moving install ownership from the machine's
  package manager to the fleet is its own consent-and-update decision — a follow-up by
  topic, not part of this recipe.
- **Windows Task mode (`EXECMODE=2`) or leaving the native services in place.** Rejected:
  then nothing is under management — no fleet-visible health, no config rollout, no restart —
  which is the task, not an implementation detail.

## Sources / Prior art

- [GLPI Agent man page](https://glpi-agent.readthedocs.io/en/latest/man/glpi-agent.html) —
  `--daemon`, `--no-fork`, `--conf-file`, `--conf-reload-interval`, logger and httpd options.
- [GLPI Agent usage](https://glpi-agent.readthedocs.io/en/latest/usage.html) — managed mode
  is "daemon under Unix, service under Windows"; embedded web interface on port 62354.
- [Windows installer reference](https://glpi-agent.readthedocs.io/en/latest/installation/windows-command-line.html)
  — MSI, `INSTALLDIR`, `EXECMODE` 1/2/3 (service / task / manual).
- [`bin/glpi-agent` source](https://github.com/glpi-project/glpi-agent/blob/develop/bin/glpi-agent)
  — `--daemon` instantiates the platform-neutral `GLPI::Agent::Daemon`; `--version` prints
  `$VERSION_STRING` (`GLPI Agent (X.Y[.Z])`).
- [Windows packaging source](https://github.com/glpi-project/glpi-agent/blob/develop/contrib/windows/glpi-agent-packaging.pl)
  (and `packaging/template.bat.tt`) — the service registration
  (`glpi-agent.exe -I"…perl\agent" -I"…site\lib" -I"…vendor\lib" -I"…perl\lib"
  "…perl\bin\glpi-win32-service"`), the `glpi-agent` service name, and the `.bat` wrapper.
- **Verified against GLPI Agent 1.15 in the Dev Container** (installed via the official Linux
  installer): `--daemon --no-fork` stays one foreground process and exits cleanly on SIGTERM;
  it keeps running when the server is unreachable; a missing `--conf-file` is a hard startup
  failure (*"Config: non-existing file"*); an unwritable state directory is one too (*"Can't
  write in /var/lib/glpi-agent"*); `--version` prints `GLPI Agent (1.15-1)` — no strict-SemVer
  token, so the probe reports nothing. The release's `GLPI-Agent-1.15.tar.gz` is the **source
  distribution** (it rides the system Perl), not a runnable bundle; the self-contained Linux
  build is the AppImage, whose direct-run dispatch was verified the same way
  (`--script=glpi-agent --version`, and a foreground `--daemon --no-fork` run, without root
  and without FUSE via `APPIMAGE_EXTRACT_AND_RUN=1`).
- [GLPI Agent portable discussion](https://github.com/glpi-project/glpi-agent/discussions/273)
  — the Windows tree is self-contained (bundled Perl), relevant to the deferred
  package-delivery alternative.

## Consequences

- Positive: the GLPI Agent appears as its own Agent — health, restart, centrally rolled-out
  configuration — on both platforms, with zero new code and both shipped plugins untouched.
  The recipe is the first documented Foreign Agent beyond the promtail walkthrough, and it
  exercises only accepted machinery.
- Negative / trade-offs: the `[[supervisor]]` block is **local-only** — ADR-0057 rightly
  refuses to push absolute-path blocks, so the block reaches `client.toml` by hand or by the
  operator's configuration management. GLPI Agent **updates stay with the machine's package
  manager**, invisible to the fleet. The disabled native autostart is an operator duty the
  fleet cannot verify — a forgotten hand-over means two agents inventorying the host. The
  reported version is usually absent (two-component releases). Configuration applies by
  restart only, which for an inventory agent is harmless (no in-flight state worth keeping).
- Before the first Configuration rollout, `${config_dir}/glpi-agent-conf` is absent and the agent
  refuses to start (verified): the Supervisor crash-loops three times and holds, and the first
  apply ends the hold by restarting onto the written file. The recipe documents this window —
  and the static-`--server` variant for a host whose configuration stays local — rather than
  hiding it.
- Follow-ups (by topic): fleet-managed GLPI installation (Windows portable tree as a package,
  or a native-installer kind under the proposed unified lifecycle vocabulary); a health probe
  against the agent's embedded httpd (port 62354) instead of process-aliveness.
