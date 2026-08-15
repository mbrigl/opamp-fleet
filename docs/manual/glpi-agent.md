# Recipe: supervising the GLPI Agent

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md) ·
[Rollout walkthrough](rollout.md)

The [GLPI Agent](https://glpi-agent.readthedocs.io/) is the inventory agent of the GLPI
asset-management suite — a Perl application the machine installs through its own channels: a
distribution package on Linux, an MSI on Windows. This recipe puts that installation under a
Supervisor, so it appears to the Server as its own Agent — health, a restart the operator issues
from the fleet view, and a centrally rolled-out configuration — with the same block shape on both
platforms.

Two routes lead there, and the page covers both:

| Route | The agent is | Choose it when |
|---|---|---|
| **Machine-installed** (below) | installed by `apt`/`dnf` or the MSI, named by absolute path | the host's package manager should keep owning GLPI updates |
| **[Fleet-delivered](#fleet-delivered-the-agent-as-a-package)** | a package the Server ships, owned by the Client | the fleet should install and update GLPI itself — nothing preinstalled, no Perl on the host |

- [How this differs from the package walkthrough](#how-this-differs-from-the-package-walkthrough)
- [The shape: a foreground daemon](#the-shape-a-foreground-daemon)
- [1. Hand over the autostart](#1-hand-over-the-autostart)
- [2. The block on Linux](#2-the-block-on-linux)
- [3. The block on Windows](#3-the-block-on-windows)
- [4. Send its configuration](#4-send-its-configuration)
- [5. What to expect in the fleet view](#5-what-to-expect-in-the-fleet-view)
- [Fleet-delivered: the agent as a package](#fleet-delivered-the-agent-as-a-package)
- [Running under an account that is not root or LocalSystem](#running-under-an-account-that-is-not-root-or-localsystem)
- [Troubleshooting](#troubleshooting)

## How this differs from the package walkthrough

The [rollout walkthrough](rollout.md) delivers the program itself from the Server. The GLPI Agent
takes the other path the program rule offers (see
[Which programs take updates](client.md#which-programs-take-updates)): it is **the machine's
program**, named by absolute path, installed and updated by the package manager that put it there.
Three consequences follow:

- **No package updates from the fleet.** The Agent does not declare `AcceptsPackages`; a new GLPI
  Agent version arrives by `apt`/`dnf` upgrade or a new MSI, as before.
- **The block is the operator's to write.** A Server-delivered Supervisor set may name only
  Client-owned programs, so this block lives in each host's `client.toml` — written by hand or by
  the configuration management that installs the GLPI Agent anyway.
- **The configuration is still central.** The agent's configuration file is a Configuration on
  the Server, typed `glpi-agent`, rolled out and applied by restart like any other — that, plus
  health and restart, is what the fleet gains.

## The shape: a foreground daemon

The Supervisor manages processes it spawns as children, so the GLPI Agent must run in the
foreground and stay there. Two of its own flags do exactly that:

- `--daemon` puts it in daemon *mode* — the long-running managed mode with the wake-up schedule
  controlled from the GLPI server and the embedded web interface on port 62354. Without it, the
  agent runs its tasks once and exits, and the watchdog would restart it forever.
- `--no-fork` keeps it from detaching: it stays the Supervisor's direct child, one process, on
  every platform. The daemonizing — start at boot, restart on failure — is the Client's job now.

The agent ends cleanly on the graceful stop, and a configuration change is applied by restart:
the GLPI Agent has no reload signal, so `reload_signal` stays unset.

## 1. Hand over the autostart

The native installation also registered its own autostart, and two agents would inventory the
host twice and fight over port 62354 — so exactly one may remain. On Linux:

```console
# systemctl disable --now glpi-agent
```

On Windows the MSI's `EXECMODE` decides. For a fresh install, `3` means no service and no task:

```console
> msiexec /i GLPI-Agent-1.15-x64.msi /quiet EXECMODE=3
```

For an existing installation, stop and disable the `glpi-agent` service it registered:

```console
> sc.exe stop glpi-agent
> sc.exe config glpi-agent start= disabled
```

Running as a foreground daemon, the agent skips its own PID-file single-instance check — nothing
stops a forgotten native service from running beside it, which is why this step comes first.

## 2. The block on Linux

```toml
[[supervisor]]
type = "command"
name = "glpi"
service_name = "glpi-agent"
command = "/usr/bin/glpi-agent"
args = [
    "--daemon", "--no-fork",
    "--conf-file=${config_dir}/glpi-agent-conf",
    "--logger=file", "--logfile=${supervisor_dir}/glpi-agent.log", "--logfile-maxsize=16",
]
version_args = ["--version"]
```

What each line is doing:

- `command` is the absolute path the distribution package installed — the machine's program,
  supervised but never written to.
- `service_name = "glpi-agent"` is the Agent **type** every GLPI Configuration is aimed at. The
  block's `name` is yours; keeping it short (`glpi`) keeps the directory short.
- `--conf-file=${config_dir}/glpi-agent-conf` points the agent at the written Configuration entry —
  `glpi-agent-conf` is the *name of the Configuration on the Server*, and a name carries no
  extension: Configuration names follow the same grammar as every other name here — 1–32
  lowercase letters, digits and `-`, no dot — while `--conf-file` reads whatever path it is
  given. See
  [step 4](#4-send-its-configuration) for what happens before the first one arrives.
- The logger flags are optional but earn their keep: the agent logs to stderr by default, and a
  file under `${supervisor_dir}` is one you can find, sized in MB by `--logfile-maxsize`, and one
  the purge takes with the Supervisor.
- `version_args` is best-effort here: the probe accepts strict SemVer only, and GLPI Agent
  releases are usually two-component (`1.15`), so `service.version` will mostly stay absent. A
  three-component release (`1.7.1`) reports one.

## 3. The block on Windows

The MSI (default `INSTALLDIR`: `C:\Program Files\GLPI-Agent`) ships no agent executable — it
ships a bundled Perl whose interpreter is named `perl\bin\glpi-agent.exe`, and the agent is the
script `perl\bin\glpi-agent` that interpreter runs. The block invokes it exactly the way the
native service registration does — interpreter, the four `-I` library paths, then the script:

```toml
[[supervisor]]
type = "command"
name = "glpi"
service_name = "glpi-agent"
command = 'C:\Program Files\GLPI-Agent\perl\bin\glpi-agent.exe'
args = [
    '-IC:\Program Files\GLPI-Agent\perl\agent',
    '-IC:\Program Files\GLPI-Agent\perl\site\lib',
    '-IC:\Program Files\GLPI-Agent\perl\vendor\lib',
    '-IC:\Program Files\GLPI-Agent\perl\lib',
    'C:\Program Files\GLPI-Agent\perl\bin\glpi-agent',
    '--daemon', '--no-fork',
    '--conf-file=${config_dir}/glpi-agent-conf',
    '--logger=file', '--logfile=${supervisor_dir}/glpi-agent.log', '--logfile-maxsize=16',
]
version_args = ['--version']
```

- **Never spawn `glpi-agent.bat`.** The batch file is a wrapper: the supervised child would be
  `cmd.exe`, the Perl process its grandchild — the stop would kill the wrapper and orphan the
  agent, and the process telemetry would sample the wrong pid. With the invocation above, the
  direct child *is* the agent.
- The `-I` paths make the invocation independent of the working directory — they are the same
  four the MSI writes into its own service registration. Adjust all six paths together if
  `INSTALLDIR` was changed.
- Single-quoted TOML strings keep the backslashes literal. `reload_signal` must stay unset here
  twice over: the agent has none, and the key is refused on Windows anyway.

## 4. Send its configuration

The configuration the fleet delivers is an ordinary Configuration — typed `glpi-agent` so it
reaches no other kind of Agent, named `glpi-agent-conf` because that is the file name the
`--conf-file` argument expects, written in the GLPI Agent's own `key = value` format:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "glpi-agent", "selector": {}, "body": "server = https://glpi.example.com/front/inventory.php\n"}' \
       http://127.0.0.1:4320/api/v1/configurations/glpi-agent-conf
$ curl -X POST http://127.0.0.1:4320/api/v1/configurations/glpi-agent-conf/rollout
```

A minimal body to start from is `config/examples/glpi-agent-conf.cfg` in this repository;
`scripts/seed_test_configs.sh` PUTs and rolls out exactly that one under this name.

The delivered file is the agent's **whole** configuration — with `--conf-file` it does not read
the native `/etc/glpi-agent/agent.cfg` or `etc\agent.cfg` the installer left behind. Any
`agent.cfg` key works in the body; `conf-reload-interval` is unnecessary, because a Configuration
change is applied by restarting the agent on the rewritten file.

**The first start comes before the first Configuration**, and the GLPI Agent refuses to start
when `--conf-file` names a file that does not exist yet. That is visible, not fatal: the
Supervisor tries three times, then holds, and its Agent reports *"the program keeps failing to
start — holding until a new configuration, package, or restart"*. The first rollout act ends the
hold — the apply restarts the process on the file it just wrote, and the Agent turns healthy.
Roll the Configuration out first and the window never opens; either order lands in the same
place.

Prefer the host to keep its configuration to itself? Drop the `--conf-file` argument and write
`--server=…` into `args` instead — the Supervisor still gives you health and restart, but a
rolled-out Configuration then lands in `config/` with nothing reading it.

## 5. What to expect in the fleet view

| Field | What it shows for this Agent |
|---|---|
| `service_name` | `glpi-agent` — aim Configurations (and Selectors) at this. |
| `capabilities` | No `AcceptsPackages` — by design; the machine's package manager updates this program. `AcceptsRestartCommand` is there: the fleet-view restart works. |
| `service_version` | Usually absent — see the `version_args` note in [step 2](#2-the-block-on-linux). The version is on the GLPI server's inventory anyway. |
| `healthy`, `health_status` | The crash-loop hold before the first Configuration; healthy once the daemon runs. |
| `remote_config_status`, `effective_config` | The `glpi-agent-conf` round trip: `APPLIED` once the restarted agent survives `apply_grace_secs`. |

## Fleet-delivered: the agent as a package

Everything above supervises an agent the machine installed. The other route lets the **fleet**
own the installation — nothing preinstalled, no Perl and no FUSE on the host, updates and
rollback through the same package machinery every other agent uses. It exists because upstream
publishes self-contained builds for both platforms; what changes is only the block's program
(ADR-0064).

**On Windows the artifact is upstream's own portable zip**, uploaded exactly as published:

```console
$ curl -LO https://github.com/glpi-project/glpi-agent/releases/download/1.19/GLPI-Agent-1.19-x64.zip
$ curl -X PUT -H 'Content-Type: application/json' -d '{}' \
       http://127.0.0.1:4320/api/v1/packages/glpi-agent/glpi-agent/1.19
$ curl -X PUT --data-binary @GLPI-Agent-1.19-x64.zip \
       "http://127.0.0.1:4320/api/v1/packages/glpi-agent/glpi-agent/1.19/entries/windows/amd64"
```

Because nothing repacks it, the hash the Agents verify is the one upstream published — check it
against the release's `glpi-agent-1.19.sha256` and you have verified what every host will run.
The same file can stay on the release page instead: point a
[referenced entry](rollout.md#4-give-it-to-the-server) at its URL with that hash.

**On Linux the fleet artifact is built once from the AppImage**, upstream's self-contained Linux
build. `scripts/pack-glpi-agent.sh` verifies the release's own SHA-256, extracts it (no FUSE
needed), drops the symlinks a tree package cannot carry, and packs a deterministic `.tar.gz` —
the same release always yields the same hash, so a repack never becomes a rollout nobody asked
for:

```console
$ sha=$(scripts/pack-glpi-agent.sh 1.19)
$ curl -X PUT --data-binary @glpi-agent_1.19_linux_amd64.tar.gz \
       "http://127.0.0.1:4320/api/v1/packages/glpi-agent/glpi-agent/1.19/entries/linux/amd64"
```

The blocks name the program by **bare name** — that is the consent that makes this Agent accept
packages — and `program_path` says where it sits inside the tree:

```toml
# Linux
[[supervisor]]
type = "command"
name = "glpi"
service_name = "glpi-agent"
command = "AppRun"                 # bare: the Client owns and updates it
program_path = "AppRun"            # …and it is the tree's entry point
args = [
    "--script=glpi-agent",         # the AppImage bundles several; this selects the agent
    "--daemon", "--no-fork",
    "--conf-file=${config_dir}/glpi-agent-conf",
    "--vardir=${supervisor_dir}/agent-state",
]
```

```toml
# Windows
[[supervisor]]
type = "command"
name = "glpi"
service_name = "glpi-agent"
command = "glpi-agent.exe"
program_path = "perl/bin/glpi-agent.exe"
working_dir = "${supervisor_dir}/program/tree"   # what the portable .bat does
args = [
    "-I${supervisor_dir}/program/tree/perl/agent",
    "-I${supervisor_dir}/program/tree/perl/site/lib",
    "-I${supervisor_dir}/program/tree/perl/vendor/lib",
    "-I${supervisor_dir}/program/tree/perl/lib",
    "${supervisor_dir}/program/tree/perl/bin/glpi-agent",
    "--daemon", "--no-fork",
    "--conf-file=${config_dir}/glpi-agent-conf",
    "--vardir=${supervisor_dir}/agent-state",
]
```

`${supervisor_dir}/program/tree` is where a tree package lands — the same path
[Where things live on disk](client.md#where-things-live-on-disk) shows — so the `-I` arguments
follow the package instead of naming an install directory that does not exist on this route.

Three things this route needs that the machine-installed one does not:

- **`--vardir` must point outside the tree, and the directory must exist.** The agent never
  creates it and exits if it is missing, and anything inside `program/tree/` is replaced
  wholesale by the next update — which would discard the agent's `deviceid` and make the GLPI
  server see a new asset. `${supervisor_dir}/agent-state` is Client-owned, survives every swap,
  and is removed with the Supervisor. Create it once (configuration management, or by hand)
  before the first start.
- **The block may be rolled out from the Server.** Bare-named programs are the one shape a
  Server-delivered Supervisor set may carry, so unlike the machine-installed route this block
  can travel in a Configuration typed `opamp-fleet-client` — see
  [The Server can manage the set](client.md#the-server-can-manage-the-set).
- **Nothing native is installed, so [step 1](#1-hand-over-the-autostart) does not apply** —
  there is no service to disable, unless a native GLPI Agent is also present, in which case
  disable it as described there.

Updating is the ordinary act: a new version is a new Set, uploaded and rolled out, health-gated
on the host and rolled back if the new tree will not stay up
([walkthrough step 8](rollout.md#8-ship-an-update-and-take-it-back)). One platform limit:
upstream publishes the AppImage for **x86_64 only**, so Linux arm64 hosts stay on the
machine-installed route above.

## Running under an account that is not root or LocalSystem

Under the default service accounts nothing more is needed. A Client
[running under its own account](client.md#running-it-under-its-own-account) spawns the GLPI Agent
as that account, and two things follow:

- **The agent's state directory must be writable**, or it exits at startup with *"Can't write in
  /var/lib/glpi-agent"*. That directory belongs to the native installation — `/var/lib/glpi-agent`
  on Linux, `<INSTALLDIR>\var` on Windows — so grant it to the service account
  (`chown -R <account> /var/lib/glpi-agent`).
- **An unprivileged inventory is a partial inventory.** Hardware probes (DMI, disks) need
  elevated rights; the agent runs and reports, but the GLPI server sees less. Decide whether
  that trade is acceptable before moving the Client off root for a host whose inventory matters.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The Agent holds with *"the program keeps failing to start"* right after setup | No `glpi-agent-conf` Configuration has been rolled out yet — the agent exits on the missing `--conf-file`. Roll it out ([step 4](#4-send-its-configuration)); the apply ends the hold. The agent's log says `Config: non-existing file …`. |
| The hold persists after a rollout | The Configuration does not reach this Agent: its name must be `glpi-agent-conf` (the file name in `args`, and a Configuration name admits no dot), its `service_name` must be `glpi-agent`, and its Selector must match — check the Agent's row for the entry. |
| *"Can't write in /var/lib/glpi-agent"* in the agent's log | The Client runs under an account that does not own the agent's state directory — see [the account section](#running-under-an-account-that-is-not-root-or-localsystem). |
| The host is inventoried twice, or the agent logs that port 62354 is in use | The native autostart is still active beside the Supervisor — [step 1](#1-hand-over-the-autostart) was skipped or a package upgrade re-enabled the service. |
| Windows: the process exits immediately, log says *"Can't locate … in @INC"* | The `-I` paths do not match the installation — `INSTALLDIR` differs from `C:\Program Files\GLPI-Agent`. Fix all six paths in the block together. |
| `service_version` is empty | Expected for two-component GLPI releases; not a fault. `packages[].version` does not apply — this Agent takes no packages. |
| The configuration is `APPLIED` but the agent still contacts the old GLPI server | The block runs without `--conf-file` (the static-`--server` variant), so the written entry is never read. |
