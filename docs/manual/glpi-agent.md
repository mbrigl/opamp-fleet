# Recipe: supervising the GLPI Agent

[← User Manual](README.md) · [The Server](server.md) · [The Client](client.md) ·
[Rollout walkthrough](rollout.md)

The [GLPI Agent](https://glpi-agent.readthedocs.io/) is the inventory agent of the GLPI
asset-management suite — a Perl application upstream publishes as a self-contained build for both
platforms. This recipe has the fleet deliver that build and put it under a Supervisor, so it
appears to the Server as its own Agent — health, a restart the operator issues from the fleet
view, a centrally rolled-out configuration, and updates through the same package machinery every
other agent uses — with the same block shape on both platforms.

**The fleet owns the installation.** Nothing is preinstalled, and the host needs neither Perl nor
FUSE. A program is named by a **bare file name**, which is the only shape a block accepts
([ADR-0085](../adr/0085-the-client-manages-only-programs-it-installs.md)), so an agent the
machine's package manager installed cannot be supervised where it sits — the way across is to
repack that version and deliver it, which is what this page does. A host may keep its
`apt`/`dnf`/MSI installation; it simply stays outside the fleet, and
[step 1](#1-hand-over-the-autostart) says what to do about it.

- [How this differs from the package walkthrough](#how-this-differs-from-the-package-walkthrough)
- [The shape: a foreground daemon](#the-shape-a-foreground-daemon)
- [1. Hand over the autostart](#1-hand-over-the-autostart)
- [2. Build and upload the package](#2-build-and-upload-the-package)
- [3. The block on Linux](#3-the-block-on-linux)
- [4. The block on Windows](#4-the-block-on-windows)
- [5. Send its configuration](#5-send-its-configuration)
- [6. What to expect in the fleet view](#6-what-to-expect-in-the-fleet-view)
- [Updating](#updating)
- [Running under an account that is not root or LocalSystem](#running-under-an-account-that-is-not-root-or-localsystem)
- [Troubleshooting](#troubleshooting)

## How this differs from the package walkthrough

The [rollout walkthrough](rollout.md) ships a program the fleet builds. Here the program is a
third party's release, and the only work is repacking it into something the Client can unpack
(ADR-0064) — after that it is an ordinary package, and everything the walkthrough says about
rollout, health-gating and rollback applies unchanged. Three things are specific to this agent:

- **It must be made to run in the foreground**, which its own flags do — see
  [the shape](#the-shape-a-foreground-daemon).
- **Its state must live outside the delivered tree**, or an update discards the `deviceid` and the
  GLPI server sees a new asset — see [step 3](#3-the-block-on-linux).
- **Its configuration is a Configuration**, typed `glpi-agent`, rolled out and applied by restart
  like any other — the agent has no reload signal.

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

**Skip this on a host with no GLPI Agent installed** — the usual case here, since the fleet
brings its own. It applies where a native installation is already present, and there it comes
first: two agents would inventory the host twice and fight over port 62354, so exactly one may
remain, and it is the fleet's. On Linux:

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

Removing the native installation outright works just as well and leaves less to forget; disabling
is described here because it is reversible, which matters while you are still deciding whether the
fleet-delivered agent does everything the machine's did.

## 2. Build and upload the package

**Both artifacts come from one command** —
[`opamp-package-fetch`](tools.md#opamp-package-fetch) fetches the release, verifies it against
the checksum upstream published, repacks what has to be repacked, and uploads if you let it:

```console
$ opamp-package-fetch --agent glpi-agent --version 1.19 \
      --platform windows/amd64 --platform linux/amd64 --server http://127.0.0.1:4320
```

What that does differs per platform, and the difference is worth knowing:

- **Windows takes upstream's portable zip exactly as published.** Nothing repacks it, so the hash
  the Agents verify is the one on the release page — check it against
  `glpi-agent-1.19.sha256` and you have verified what every host will run. The file can stay
  there instead of being uploaded: point a
  [referenced entry](rollout.md#4-give-it-to-the-server) at its URL with that hash.
- **Linux is built from the AppImage**, upstream's self-contained Linux build, because the
  release's `.tar.gz` is source. It is extracted once here — which is what spares every fleet
  host the FUSE dependency — and packed deterministically, so the same release always yields the
  same artifact and a repack never becomes a rollout nobody asked for.

**Linux arm64 is not supported.** Upstream publishes the AppImage for x86_64 only, and there is
no other self-contained Linux build to repack; a block cannot name the machine's own installation
instead, because `command` takes a bare file name and nothing else. An arm64 host keeps its
`apt`/`dnf` GLPI Agent and stays outside the fleet — it gets no health, no fleet-view restart and
no rolled-out configuration — until upstream publishes an arm64 build or someone repacks one.

## 3. The block on Linux

The program is named by a **bare file name** — the only shape a block accepts — and
`program_path` says where it sits inside the delivered tree:

```toml
[[supervisor]]
type = "command"
name = "glpi"
service_name = "glpi-agent"
command = "AppRun"                 # the tree's entry point, in this Supervisor's own program/
program_path = "AppRun"            # …and where it sits inside the package
args = [
    "--script=glpi-agent",         # the AppImage bundles several; this selects the agent
    "--daemon", "--no-fork",
    "--conf-file=${config_dir}/glpi-agent-conf",
    "--vardir=${supervisor_dir}/agent-state",
    "--logger=file", "--logfile=${supervisor_dir}/glpi-agent.log", "--logfile-maxsize=16",
]
version_args = ["--version"]
```

What each line is doing:

- `command` names a file in this Supervisor's own `program/` directory, which is what lets the
  Server replace it. `program_path` names the same file inside the archive, because a tree
  package has to say which member is the program.
- `service_name = "glpi-agent"` is the Agent **type** every GLPI Configuration is aimed at. The
  block's `name` is yours; keeping it short (`glpi`) keeps the directory short.
- `--conf-file=${config_dir}/glpi-agent-conf` points the agent at the written Configuration entry —
  `glpi-agent-conf` is the *name of the Configuration on the Server*, and a name carries no
  extension: Configuration names follow the same grammar as every other name here — 1–32
  lowercase letters, digits and `-`, no dot — while `--conf-file` reads whatever path it is
  given. See
  [step 5](#5-send-its-configuration) for what happens before the first one arrives.
- **`--vardir` must point outside the tree, and the directory must exist.** The agent never
  creates it and exits if it is missing, and anything inside `program/tree/` is replaced
  wholesale by the next update — which would discard the agent's `deviceid` and make the GLPI
  server see a new asset. `${supervisor_dir}/agent-state` is Client-owned, survives every swap,
  and is removed with the Supervisor. Create it once (configuration management, or by hand)
  before the first start.
- The logger flags are optional but earn their keep: the agent logs to stderr by default, and a
  file under `${supervisor_dir}` is one you can find, sized in MB by `--logfile-maxsize`, and one
  the purge takes with the Supervisor.
- `version_args` is best-effort here: the probe accepts strict SemVer only, and GLPI Agent
  releases are usually two-component (`1.15`), so `service.version` will mostly stay absent. A
  three-component release (`1.7.1`) reports one. The package's own version is reported either
  way, so the fleet view always knows which Set is installed.

## 4. The block on Windows

Upstream's build ships no agent executable — it ships a bundled Perl whose interpreter is named
`perl\bin\glpi-agent.exe`, and the agent is the script `perl\bin\glpi-agent` that interpreter
runs. The block invokes it the way the portable `.bat` does — interpreter, the four `-I` library
paths, then the script — with every path inside the delivered tree:

```toml
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
    "--logger=file", "--logfile=${supervisor_dir}/glpi-agent.log", "--logfile-maxsize=16",
]
version_args = ["--version"]
```

- **Never spawn `glpi-agent.bat`.** The batch file is a wrapper: the supervised child would be
  `cmd.exe`, the Perl process its grandchild — the stop would kill the wrapper and orphan the
  agent, and the process telemetry would sample the wrong pid. With the invocation above, the
  direct child *is* the agent.
- `${supervisor_dir}/program/tree` is where a tree package lands — the same path
  [One directory per Agent](client.md#one-directory-per-agent) shows — so the `-I` arguments
  follow the package instead of naming an install directory. They make the invocation
  independent of the working directory, and nothing has to be adjusted when the version changes.
- `reload_signal` must stay unset here twice over: the agent has none, and the key is refused on
  Windows anyway.

## 5. Send its configuration

The configuration the fleet delivers is an ordinary Configuration — typed `glpi-agent` so it
reaches no other kind of Agent, named `glpi-agent-conf` because that is the file name the
`--conf-file` argument expects, written in the GLPI Agent's own `key = value` format:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "glpi-agent", "selector": {}, "body": "server = https://glpi.example.com/front/inventory.php\n"}' \
       http://127.0.0.1:4321/api/v1/configurations/glpi-agent-conf
$ curl -X POST http://127.0.0.1:4321/api/v1/configurations/glpi-agent-conf/rollout
```

A minimal body to start from is `config/examples/glpi-agent-conf.cfg` in this repository;
`scripts/seed_test_configs.sh` PUTs and rolls out exactly that one under this name.

The delivered file is the agent's **whole** configuration — with `--conf-file` it reads nothing
else, including a native `/etc/glpi-agent/agent.cfg` or `etc\agent.cfg` an earlier installation
left behind on the host. Any `agent.cfg` key works in the body; `conf-reload-interval` is
unnecessary, because a Configuration change is applied by restarting the agent on the rewritten
file.

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

## 6. What to expect in the fleet view

| Field | What it shows for this Agent |
|---|---|
| `service_name` | `glpi-agent` — aim Configurations (and Selectors) at this. |
| `capabilities` | `AcceptsPackages` and `AcceptsRestartCommand`: the fleet updates this program and the fleet-view restart works. |
| `packages` | `Installed` with the Set's version once the tree is in place, or `InstallFailed` with the reason the artifact would not run here. |
| `service_version` | Usually absent — see the `version_args` note in [step 3](#3-the-block-on-linux). The Set's version above says which release is installed. |
| `healthy`, `health_status` | The crash-loop hold before the first Configuration; healthy once the daemon runs. |
| `remote_config_status`, `effective_config` | The `glpi-agent-conf` round trip: `APPLIED` once the restarted agent survives `apply_grace_secs`. |

## Updating

The ordinary act: a new version is a new Set, uploaded and rolled out, health-gated on the host
and rolled back if the new tree will not stay up
([walkthrough step 8](rollout.md#8-ship-an-update)). The agent's state survives, because
`--vardir` keeps it outside the tree that gets replaced.

**The block may be rolled out from the Server too.** Bare-named programs are the one shape a
Server-delivered Supervisor set may carry, so this block can travel in a Configuration typed
`supervisor` instead of living in each host's `supervisor.toml` — see
[The Server can manage the set](client.md#the-server-can-manage-the-set).

## Running under an account that is not root or LocalSystem

Under the default service accounts nothing more is needed. A Client
[running under its own account](client.md#running-it-under-its-own-account) spawns the GLPI Agent
as that account, and two things follow:

- **The agent's state directory must be writable**, or it exits at startup with *"Can't write in
  …"*. That is the `--vardir` directory — `${supervisor_dir}/agent-state` — and since the Client
  creates the Supervisor's directory as its own service account, it is writable already. It needs
  attention only where the directory was created by hand as another user, or where `--vardir` was
  pointed somewhere else: grant it to the service account (`chown -R <account> <the directory>`).
- **An unprivileged inventory is a partial inventory.** Hardware probes (DMI, disks) need
  elevated rights; the agent runs and reports, but the GLPI server sees less. Decide whether
  that trade is acceptable before moving the Client off root for a host whose inventory matters.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The Agent holds with *"the program keeps failing to start"* right after setup | No `glpi-agent-conf` Configuration has been rolled out yet — the agent exits on the missing `--conf-file`. Roll it out ([step 5](#5-send-its-configuration)); the apply ends the hold. The agent's log says `Config: non-existing file …`. |
| The hold persists after a rollout | The Configuration does not reach this Agent: its name must be `glpi-agent-conf` (the file name in `args`, and a Configuration name admits no dot), its `service_name` must be `glpi-agent`, and its Selector must match — check the Agent's row for the entry. |
| *"Can't write in …"* in the agent's log | The `--vardir` directory is missing or not writable by the Client's service account — see [the account section](#running-under-an-account-that-is-not-root-or-localsystem). The agent never creates it. |
| The host is inventoried twice, or the agent logs that port 62354 is in use | The native autostart is still active beside the Supervisor — [step 1](#1-hand-over-the-autostart) was skipped or a package upgrade re-enabled the service. |
| Windows: the process exits immediately, log says *"Can't locate … in @INC"* | The `-I` paths do not match the delivered tree — the package's layout differs from `perl/…` under `program/tree`. Check what the artifact actually holds, and fix all five paths in the block together. |
| `service_version` is empty | Expected for two-component GLPI releases; not a fault. `packages[].version` says which Set is installed and is the field to read instead. |
| The agent starts, but the GLPI server shows a new asset after every update | `--vardir` points inside `program/tree`, so the `deviceid` is discarded with the replaced tree. Point it at `${supervisor_dir}/agent-state` and let the GLPI server merge the duplicate. |
| The configuration is `APPLIED` but the agent still contacts the old GLPI server | The block runs without `--conf-file` (the static-`--server` variant), so the written entry is never read. |
