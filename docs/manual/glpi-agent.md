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
FUSE. A program is named by a **bare file name**, which is the only shape a block accepts, so an
agent the machine's package manager installed cannot be supervised where it sits — the way
across is to repack that version and deliver it, which is what this page does. A host may keep its
`apt`/`dnf`/MSI installation; it simply stays outside the fleet, and
[step 1](#1-hand-over-the-autostart) says what to do about it.

- [How this differs from the package walkthrough](#how-this-differs-from-the-package-walkthrough)
- [The shape: a foreground daemon](#the-shape-a-foreground-daemon)
- [1. Hand over the autostart](#1-hand-over-the-autostart)
- [2. Build and upload the package](#2-build-and-upload-the-package)
- [3. The block](#3-the-block)
- [4. Send its configuration](#4-send-its-configuration)
- [5. What to expect in the fleet view](#5-what-to-expect-in-the-fleet-view)
- [Updating](#updating)
- [The account the agent runs as](#the-account-the-agent-runs-as)
- [Troubleshooting](#troubleshooting)

## How this differs from the package walkthrough

The [rollout walkthrough](rollout.md) ships a program the fleet builds. Here the program is a
third party's release, and the only work is repacking it into something the Client can unpack —
after that it is an ordinary package, and everything the walkthrough says about
rollout, health-gating and rollback applies unchanged. These things are specific to this agent:

- **It must be made to run in the foreground**, which its own flags do — see
  [the shape](#the-shape-a-foreground-daemon).
- **Its state must live outside the delivered tree**, or an update discards the `deviceid` and the
  GLPI server sees a new asset — see [step 3](#3-the-block).
- **Its configuration is a Configuration**, typed `glpi-agent`, rolled out and applied by restart
  like any other — the agent has no reload signal.
- **Its inventory and Deploy tasks want root, or an administrator on Windows**, which neither a
  Linux host installed from the packages nor a Windows host gives the Client by default — see
  [The account the agent runs as](#the-account-the-agent-runs-as).
- **Its block is two lines**, the same two on both platforms, because the kind knows the rest —
  see [step 3](#3-the-block).

## The shape: a foreground daemon

The Supervisor manages processes it spawns as children, so the GLPI Agent must run in the
foreground and stay there. Two of its own flags do exactly that:

- `--daemon` puts it in daemon *mode* — the long-running managed mode with the wake-up schedule
  controlled from the GLPI server and the embedded web interface on port 62354. Without it, the
  agent runs its tasks once and exits, and the watchdog would restart it forever.
- `--no-fork` keeps it from detaching: it stays the Supervisor's direct child, one process, on
  every platform. The daemonizing — start at boot, restart on failure — is the Client's job now.

The agent ends cleanly on the graceful stop, and a configuration change is applied by restart: the
GLPI Agent has no reload signal, and the kind says so — there is nothing to declare in a block.

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
[`opamp-fleetctl package fetch`](tools.md#package-fetch) fetches the release, verifies it against
the checksum upstream published, repacks what has to be repacked, and uploads if you let it:

```console
$ opamp-fleetctl package fetch --agent glpi-agent --version 1.19 \
      --platform windows/amd64 --platform linux/amd64 --server https://127.0.0.1:4321
```

`--version` takes upstream's number, and the release is fetched under that tag. The Package is
uploaded under the version written as SemVer, because a Package version is SemVer: `1.19` becomes
Package `1.19.0`, `1.7.1` stays `1.7.1` (see [`package fetch`](tools.md#package-fetch)).
A second repack of the same version, for one packed by a newer tool, is a Package of its own,
numbered with `--repack 2`: it is uploaded as `1.19.1-0.r.2`, above `1.19.0` and below the next
release, so the hosts on `1.19.0` are offered it. Count on from the last repack you uploaded. The
agent on such a host still reports `1.19.0`, its own version; the Server compares the Package
version the Supervisor reports it installed, so the host is offered the repack once.

What that does differs per platform, and the difference is worth knowing:

- **Windows takes upstream's portable zip exactly as published.** Nothing repacks it, so the hash
  the Agents verify is the one on the release page — check it against
  `glpi-agent-1.19.sha256` and you have verified what every host will run. It is uploaded to the
  Server like every artifact, and the Agents download it from there.
- **Linux is built from the AppImage**, upstream's self-contained Linux build, because the
  release's `.tar.gz` is source. Its file system is extracted once here, with `unsquashfs` 4.5.1
  or later and without running the AppImage — which is what spares every fleet host the FUSE
  dependency — with both of upstream's runtimes as built, so a host with glibc newer than 2.27
  runs the agent on its own C library and an older one on the glibc 2.27 the AppImage bundles,
  and packed deterministically, so the same release always yields the
  same artifact and a repack never becomes a rollout nobody asked for. The artifact is named by
  the Package version, `glpi-agent_1.19.0_linux_amd64.tar.gz`, which is what the Packages tab
  reads out of a file name.

**Linux arm64 is not supported.** Upstream publishes the AppImage for x86_64 only, and there is
no other self-contained Linux build to repack; a block cannot name the machine's own installation
instead, because this kind installs and names its own program. An arm64 host keeps its
`apt`/`dnf` GLPI Agent and stays outside the fleet — it gets no health, no fleet-view restart and
no rolled-out configuration — until upstream publishes an arm64 build or someone repacks one.

## 3. The block

Two lines, on Linux and on Windows alike:

```toml
[[supervisor]]
type = "glpi"
name = "glpi"
```

That is the whole block. Everything else — the program, its place in the tree, the arguments and
the working directory, which differ per platform in nearly every element — is the kind's, because
none of it is a decision a host makes:

| | Linux | Windows |
|---|---|---|
| program | `AppRun`, the repacked AppImage's entry point | `glpi-agent.exe`, upstream's bundled Perl interpreter |
| in the tree | `AppRun` at the root | `perl/bin/glpi-agent.exe` |
| picks the agent | `--script=glpi-agent` — the AppImage bundles several | the four `-I` library paths, then the script by path |
| working directory | the tree root, which is where the program already is | the tree root, which upstream's own portable `.bat` sets |

Those differences follow from `EXE_SUFFIX` and from where the AppImage puts its interpreter — facts
of the artifact this project packs, which
[`docs/artifacts/glpi-agent.md`](../artifacts/glpi-agent.md) states once for both sides and pins
with tests. An upstream release that moves one of them is a red test here, not a rollout that fails
on every host.

The rest of the invocation is compiled in for the same reason, and three parts of it are worth
knowing about:

- **`--daemon --no-fork` are supervision requirements, not defaults you may drop.** Without the
  first the agent runs its tasks once and exits, and the watchdog restarts it forever; without the
  second it detaches, and the Supervisor is left holding a pid that ends immediately while the real
  process runs on unsupervised. They are properties of the kind.
- **`--conf-file` points at `glpi-agent-conf`**, the written Configuration entry — the *name of the
  Configuration on the Server*, which carries no extension: names here are 1–32 lowercase letters,
  digits and `-`. See [step 4](#4-send-its-configuration) for what happens before the first one
  arrives.
- **`--vardir` points at `${supervisor_dir}/agent-state`, outside the tree.** Anything inside
  `program/tree/` is replaced wholesale by the next update, which would discard the agent's
  `deviceid` and make the GLPI server see a new asset. The agent never creates that directory and
  exits if it is missing, so **the Client makes it before every spawn** — nothing to prepare on the
  host, and a directory somebody removed comes back on the next restart rather than leaving the
  Supervisor in a crash loop.

The agent also logs to `${supervisor_dir}/agent-log/glpi-agent.log`, rotated at 16 MiB, because a
daemon with no console has nowhere else to write and stderr is not somewhere you can go and look.
The Client makes `agent-log/` before every spawn, as it makes `agent-state/`.

**`service.version` is the agent's own version.** The 1.19 AppImage's `--version` prints
`GLPI Agent (1.19-1)`, the release and the package revision of upstream's build. The kind drops
the revision and writes the release as SemVer the way the Package was named: `1.19-1` reports
`1.19.0`, which is the Package version of the release's first repack. A later
repack of the same release, `1.19.1-0.r.2`, keeps the agent and its banner, so the agent still
reports `1.19.0`; the Package version the Supervisor reports says which repack is installed, and
it is what the Server compares. The version is the one thing the kind reports about the agent;
where the Agent runs is the Supervisor's to say. A banner with no number in it, or with any other
suffix, such as `1.22-dev` or `1.19-1-gabcdef`, reports no `service.version`, and the Package
version still says which Package is installed.

**Never `glpi-agent.bat`.** It is not a choice — the kind names the program — and this is why:
the batch file is a wrapper, so the supervised child would be `cmd.exe` and the Perl process its
grandchild. The stop would kill the wrapper and orphan the agent, and the process telemetry would
sample the wrong pid.

**One block serves the whole fleet.** Nothing in it is per-host and nothing in it is per-platform,
so it can travel as a single Configuration typed `supervisor`
([The Server can manage the set](client.md#the-server-can-manage-the-set)) instead of being written
into every host's file.

## 4. Send its configuration

The configuration the fleet delivers is an ordinary Configuration — typed `glpi-agent` so it
reaches no other kind of Agent, named `glpi-agent-conf` because that is the file name the
`--conf-file` argument expects, written in the GLPI Agent's own `key = value` format. It has no
Selector of its own: it reaches the Agents through a Deployment, the same one that carries the
Package. `--cacert ca.pem` names the CA that signed the Server's certificate, and `glpi` stands for
your Deployment:

```console
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "glpi-agent", "body": "server = https://glpi.example.com/front/inventory.php\n"}' \
       https://127.0.0.1:4321/api/v1/configurations/glpi-agent-conf
$ curl --cacert ca.pem -X PUT https://127.0.0.1:4321/api/v1/deployments/glpi/configurations/glpi-agent-conf
$ curl --cacert ca.pem -X POST https://127.0.0.1:4321/api/v1/deployments/glpi/rollout
```

A minimal body to start from is `config/examples/glpi-agent-conf.cfg` in this repository;
`opamp-fleetctl package fetch` uploads it under this name when the Server holds none, and
`scripts/seed_test_configs.sh` puts exactly that one into a Deployment and rolls it out.

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

Prefer the host to keep its configuration to itself? Name the Supervisor in
`[supervisors] remote_config_disabled` and place `glpi-agent-conf` in
`<supervisor_dir>/glpi/config/` by hand
([Switching remote configuration off for one Supervisor](client.md#switching-remote-configuration-off-for-one-supervisor)).
The Supervisor still gives you health, restart and packages, and the agent reads that file.

## 5. What to expect in the fleet view

| Field | What it shows for this Agent |
|---|---|
| `service_name` | `glpi-agent` — aim Configurations (and Selectors) at this. |
| `capabilities` | `AcceptsPackages` and `AcceptsRestartCommand`: the fleet updates this program and the fleet-view restart works. `AcceptsPackages` needs `[packages] verification_key` on the Client. |
| `packages` | `Installed` with the Package's version once the tree is in place, or `InstallFailed` with the reason the artifact would not run here. |
| `service_version` | The agent's own version, from its banner — `1.19.0` for `GLPI Agent (1.19-1)`, also after a later repack of that release; see [step 3](#3-the-block). |
| `healthy`, `health_status` | The crash-loop hold before the first Configuration; healthy once the daemon runs. |
| `remote_config_status`, `effective_config` | The `glpi-agent-conf` round trip: `APPLIED` once the restarted agent survives `apply_grace_secs`. |

## Updating

The ordinary act: a new version is a new Package, uploaded and rolled out, health-gated on the host
and rolled back if the new tree will not stay up
([walkthrough step 9](rollout.md#9-ship-an-update)). The agent's state survives, because
`--vardir` keeps it outside the tree that gets replaced.

**The block may be rolled out from the Server too.** Bare-named programs are the one shape a
Server-delivered Supervisor set may carry, so this block can travel in a Configuration typed
`supervisor` instead of living in each host's `supervisor.toml` — see
[The Server can manage the set](client.md#the-server-can-manage-the-set).

## The account the agent runs as

The GLPI Agent runs as the account that starts it. Without a process account that is the account
the Client's service runs as, because the Client starts it. With a
[process account](client.md#managed-processes-under-a-process-account) it is that account, because
the second service `opamp-fleet-agents` starts every Managed Process. Two of its tasks need more
than an ordinary account: a **full inventory** reads hardware through DMI, disks and `/dev` on
Linux, and on Windows through queries parts of which only an administrator may make; the
**Deploy** task installs operating-system packages. Both need root on Linux and an administrator
on Windows. Under another account the agent still runs and reports, but the GLPI server sees a
partial inventory and every deployment that needs those rights fails.

Which account that is depends on how the Client was installed
([Running it under its own account](client.md#running-it-under-its-own-account)):

| Host | The agent runs as | For the whole agent |
|---|---|---|
| Linux, from the `.deb` or `.rpm` | the account `opamp-fleet` on a first installation | choose root, below |
| Linux or macOS, `service install` by hand without `--run-as` | root on a first registration | nothing to do |
| Windows, first registration (the MSI's included) | the virtual account `NT SERVICE\opamp-fleet`, which holds no group and no privilege | choose `LocalSystem`, below |
| Windows, registered as `LocalSystem` earlier | `LocalSystem`, kept by every re-registration | nothing to do |
| any host with a process account | the process account, which is never root or `LocalSystem` | drop the process account and choose root or `LocalSystem` |

A re-registration keeps the account the existing registration names: the `User=` of the unit on
Linux, the logon account the SCM holds on Windows. An upgrade therefore never moves a host to
another account; the operator names it.

**Choosing root or `LocalSystem` is the operator's decision, per host**, made with the install
itself. On Linux:

```console
# opamp-fleet service install --run-as root
# systemctl restart opamp-fleet
```

The unit then says `User=root`, every later package upgrade keeps it, and the systemd unit carries
the sandboxing of a root unit. None of it takes anything the GLPI Agent needs: `/etc`, `/usr` and
`/dev` stay writable and readable, and `sudo` and setuid helpers keep working.

On Windows, from an elevated prompt:

```console
> opamp-fleet service install --run-as LocalSystem
```

Where the Client ran, the install starts it again. It warns that every Managed Process the Client
starts runs as `LocalSystem`, which is what the agent needs here.

A host with a process account runs the agent under that account, which the install never lets be
root or `LocalSystem`, so it also takes `--no-process-account` on the same install
([Changing or dropping the process account](client.md#changing-or-dropping-the-process-account)).
A host whose GLPI Agent only inventories and never deploys may stay on its account and accept the
partial inventory. Root or `LocalSystem` makes every Managed Process on that host, and every
Configuration the Server rolls out to them, as strong as the operating system — weigh that before
choosing it for a host.

**The agent writes into two directories of its own.** It keeps its state in
`${supervisor_dir}/agent-state` and its log in `${supervisor_dir}/agent-log`, and the Client makes
both before every spawn. Under a process account these two are the kind's data directories: on
Linux and macOS they belong to the Client and the process account's group with mode `2770`, on
Windows the process account is granted modify, without delete, on them, and everything else in
the Supervisor's directory stays read-only or closed to it
([what the process account may read and write](client.md#managed-processes-under-a-process-account)).
Under any account the state directory must be writable, or the agent exits at startup with
*"Can't write in …"*. That needs attention only where the directory survives from a run under
another account: re-running `service install` with the same `--run-as` and `--process-account`
hands the directories to the accounts again.

## Troubleshooting

| Symptom | Cause |
|---|---|
| The Agent holds with *"the program keeps failing to start"* right after setup | No `glpi-agent-conf` Configuration has been rolled out yet — the agent exits on the missing `--conf-file`. Roll it out ([step 4](#4-send-its-configuration)); the apply ends the hold. The agent's log says `Config: non-existing file …`. |
| The hold persists after a rollout | The Configuration does not reach this Agent: its name must be `glpi-agent-conf` (the file name `--conf-file` reads, and a Configuration name admits no dot), its `service_name` must be `glpi-agent`, and the Deployment that holds it must claim this Agent — check the Agent's row for the entry. |
| *"Can't write in …"* in the agent's log | The `--vardir` directory is not writable by the account the agent runs as — see [the account section](#the-account-the-agent-runs-as). It is never *missing*: the Client makes it before every spawn. |
| The inventory lacks hardware, or every deployment fails | The agent runs under an account that is not root or `LocalSystem`: the Client's own, the Windows virtual account, or a process account — see [the account section](#the-account-the-agent-runs-as). |
| The host is inventoried twice, or the agent logs that port 62354 is in use | The native autostart is still active beside the Supervisor — [step 1](#1-hand-over-the-autostart) was skipped or a package upgrade re-enabled the service. |
| Windows: the process exits immediately, log says *"Can't locate … in @INC"* | The `-I` paths do not match the delivered tree — the package's layout differs from `perl/…` under `program/tree`. The kind names these paths; check what the artifact actually holds against [`docs/artifacts/glpi-agent.md`](../artifacts/glpi-agent.md) — a release that moved them needs the kind changed, not the block. |
| `service_version` is empty | The agent's `--version` banner names no number the kind can read. The Package version the Supervisor reports, `supervisor.package.version`, says which Package is installed and is the field to read instead. |
| `package fetch` refuses the Linux repack naming `unsquashfs` | The packing host has no `unsquashfs`, or one older than 4.5.1, which can write outside the directory it extracts to. Install squashfs-tools 4.5.1 or later, or fetch the Linux artifact on a host that has it, such as the Dev Container. |
