# The Client

[← User Manual](README.md) · [← The Server](server.md)

The Client is what runs on a managed host: one process, installed as a native operating-system
service on Linux, macOS, and Windows, that supervises local processes, applies the configuration the
Server sends them, reports back what they are doing, and can replace their binaries — and its own.

Everything about the Client is on this page: what it does, how it is built, how to run it, where it
puts things, and every configuration key.

**Understand it**
- [What the Client does](#what-the-client-does)
- [How it is built](#how-it-is-built)

**Run it**
- [Running it](#running-it)
- [Running it as an OS service](#running-it-as-an-os-service)
- [On-disk layout](#on-disk-layout)

**Configure it**
- [Configuration reference](#configuration-reference)
- [Gateway Mode: carrying other Clients](#gateway-mode-carrying-other-clients)
- [Supervisors: putting a process under management](#supervisors-putting-a-process-under-management)
- [Path placeholders](#path-placeholders)

**Keep it current**
- [Package updates](#package-updates)
- [Agents that are more than one file](#agents-that-are-more-than-one-file)
- [Updating the Client itself](#updating-the-client-itself)

**When something is wrong**
- [Connecting to the Server](#connecting-to-the-server)
- [Troubleshooting](#troubleshooting)

## What the Client does

- **Presents one or more Agents to the Server.** The Client is always its own Agent, whether or not
  it supervises anything, so the Server can see which version each host runs. Each configured
  Supervisor is an additional Agent. All of them share one connection. The Client's own Agent
  reports `supervisor.toml` itself as its effective configuration — with credential values (`[auth]`'s
  `bearer_token` and `password`, `[packages]`'s `archive_key`) masked as `***`, since the Server
  persists what it receives.
- **Supervises processes**: starts them, watches them, restarts them on a configuration
  change or a Server-issued restart command, stops them gracefully on shutdown.
- **Applies received Configurations**: writes each entry to disk under its Configuration's name,
  points the process at it, and reports the configuration applied or failed with the hash it
  applied.
- **Reports health, effective configuration, and version** of each Managed Process, and relays what
  a Collector carrying the `opampextension` reports about itself.
- **Installs Server-offered packages** over a Managed Process's binary — verified, health-gated, and
  rolled back if the new version will not stay up.
- **Updates its own binary** when that is switched on, staging the new version beside the running
  one and asking the service manager for a restart.
- **Accepts Server-offered connection settings**: a new credential, heartbeat interval, or endpoint,
  which it verifies by connecting before it switches.

## How it is built
The sections below are the load-bearing ideas — what each buys and where each stops. Skip to
[Running it](#running-it) if you only need to operate a host; come back when something the Client
did surprised you and you want to know whether it was supposed to.

### One process, several Agents

The unit the Server manages is an **Agent**, and one host usually has more than one. The Client is
always an Agent itself; every process it supervises is another. All of them share a single
connection to the Server.

This is why the fleet view has more rows than you have machines, and why the thing you target with a
Selector is not "a host" but an Agent on it.

The multiplexing is possible because **`instance_uid` is the only routing key**. Nothing on either
end is keyed to a connection, which means a connection can drop and reappear, or an Agent can move
between connections, without anything having to be re-established. It is also what makes Gateway
Mode work at all: a Gateway forwards messages it does not interpret, over a pool of upstream
connections it grows lazily, each Agent sticky to one of them.

The two modes — supervising local processes, and carrying other Clients' traffic — are independent
and compose freely. One binary does both, or either, or neither.

### The Client is its own Agent

Not a special case, not a separate code path: the Client presents itself the same way it presents
anything it supervises, with its own identity, its own configuration, its own version.

Three things follow, and they are the reason for the design rather than side effects:

- You can see which version every host runs, whether or not it supervises anything.
- The Client can be sent configuration the same way anything else is.
- The Client can be **updated** the same way anything else is — the update mechanism did not have to
  be invented twice.

Its effective configuration is the configuration file itself, with credentials masked before it goes
out, because the Server stores what it receives.

### Two names, and they are not interchangeable

Every Agent reports two names that are easy to confuse:

| | means | example |
|---|---|---|
| **type** | what kind of thing this is — the same on every host running that kind | `otelcol-contrib`, `supervisor` |
| **instance name** | *your* name for this one Agent | `edge-collector-3` |

Aim at the type to reach every Agent of a kind; aim at the instance name to reach exactly one. A
package is built for a type and reaches no Agent of another, whatever else its Selector says.

The Client's own type is `supervisor` — the Agent that supervises the others. Its instance name
defaults to a display name, deliberately not equal to the type: if both columns of the fleet view
said the same word, the distinction they exist to draw would be invisible on exactly the hosts
nobody has named yet.

Under this the identity proper is `instance_uid`, which the Agent asserts itself. Admission to the
fleet is a trust boundary at the endpoint, not a per-Agent authorization — an Agent that gets in is
believed about who it is.

### The service points at a pointer

The operating-system service is registered against `<root>/current/supervisor`, never against a
version directory. `current` is a pointer; versions sit side by side beneath it.

That indirection is what makes an update a *pointer swap* instead of a re-registration. Registering
would need administrative rights every time, and on Windows the running executable is locked and
cannot be overwritten in place at all. Both problems disappear if the thing the service manager
knows never moves.

The same indirection carries the command on your `PATH`, which is a symlink through `current` — so
the installed command reports the running version rather than whichever one a package happened to
deliver.

That command is named after the **product**, while the file it resolves to is named after the
**program**: `opamp-fleet` on your `PATH`, `supervisor` on disk. The split is the same one the
directories make, and it is what lets two products built from this source sit on one host without
fighting over a single `/usr/bin` entry. From an unpacked archive there is no symlink and you run
the file itself.

Where those directories live, and why the program and the data are sometimes in different places, is
[On-disk layout](#on-disk-layout).

### The configuration file has two owners

The file is TOML, hand-edited, and it fails loudly: an unknown key is refused at startup rather than
ignored, so a typo is a stopped service instead of a setting silently not taking effect. There are
no environment-variable fallbacks — what the file says is what runs.

But the file is not entirely yours. The Server may send the `[[supervisor]]` blocks, and when it
does the Client rewrites that part of the file and leaves the rest alone. Your endpoint, your
credentials, your logging stay yours.

Two guards make that safe to have. The whole offer is **validated before anything is written** — if
one block is bad, nothing changes and the Client reports the failure naming the block. And a
delivered block may name only a program the Client owns, so a Server cannot use configuration
delivery to run an arbitrary binary on your host.

The first configuration is written by the install, not by you finding a template: `service install`
can ask for what a fresh host cannot guess, validates it, and only then registers the service. It
never overwrites a file that already exists.

### Supervising is a closed vocabulary

Every supervised process, whatever it is, is driven through the same seven operations: install,
uninstall, start, stop, update, reload, and apply-configuration. A plugin selected by the block's
`type` implements them for one kind of program.

The vocabulary being closed is the point. A new kind of agent adds an implementation, not a new
concept — so package delivery, health gating and rollback work for it without anyone extending them.
Kinds exist for OpenTelemetry Collectors, for Icinga 2, and a generic one that runs a command; the
generic one covers most things without a plugin at all.

Each supervised process gets one directory holding everything about it: its identity, its
configuration, its program, and the staging its downloads pass through. Keeping them together is
what makes an install a rename inside one filesystem rather than a copy across two — a rename either
happened or did not.

Remove a block and that directory goes with it, whole. A directory no block names is reported at
startup and never deleted on its own, so a mistyped name costs you a warning rather than an Agent's
identity.

### The Client owns what it runs

A supervised program is always one the Client installed into that Agent's own directory, named by a
bare file name. An absolute path — a program the machine's package manager installed — is refused at
startup.

This is a real restriction and worth stating plainly: **the Client does not supervise software it
cannot replace.** A vendor agent installed by its own MSI or by `apt` is not managed here until it
has been repacked as a relocatable package and delivered by the fleet.

What it buys is that there is exactly one kind of Managed Process. Every capability — targeting,
version rules, the health gate, rollback — applies to all of them, and no feature has to be reasoned
about twice. The alternative was a second kind that the fleet could watch but never update, and the
seam between the two ran through every decision that touched packages.

### Packages, updates and connectivity, in one paragraph each
**Packages.** The Server decides which artifact an Agent is offered; the Client fetches it, verifies
it by hash and — where a key is configured — by signature, unpacks it beside the running program,
swaps it in by rename, and watches whether it stays up. If it does not, the predecessor comes back.
The unit is a versioned Set, immutable once published, and publishing is not the same act as rolling
out. In full: [Package updates](#package-updates).

**Versions.** An Agent reports two numbers that can disagree — the version its package record claims
and the version the program says it is. A Set must move the Agent forward from the **lower** of the
two and never below what the record claims, so each number is used only in the direction it is
trustworthy for. A version that cannot be ordered blocks nothing on its own.

**Updating itself.** The same machinery, with two differences that exist because the thing being
replaced is the thing doing the replacing: the staged binary is asked what version it is before
anything is committed, and the Client **does not restart itself** — it swings the pointer and exits,
leaving the restart to the service manager whose job that is. In full:
[Updating the Client itself](#updating-the-client-itself).

**Connectivity.** The URL scheme picks the transport. A static credential travels end to end and is
what the Server checks; mutual TLS, where configured, secures each hop with a certificate whose
private key never leaves the host. The Server can move the fleet to a new endpoint or credential,
and the Client proves an offered setting by connecting with it before it switches. In full:
[Connecting to the Server](#connecting-to-the-server).

### What the Client will not do

Knowing the edges is half of knowing the design.

- **It does not authorize per Agent.** Anything admitted to the endpoint is believed about who it
  is; the trust boundary is the endpoint, not the row.
- **It does not run programs it did not install.** See above — this is a deliberate narrowing.
- **It does not restart itself**, on any platform.
- **It does not invent telemetry semantics.** Its own metrics and logs go out over OTLP as the
  OpenTelemetry conventions define them, and nothing beyond. Its traces are the one place with no
  convention to follow — the standard names none for an agent's own lifecycle — so the spans are
  named after the operations this project already has a vocabulary for, and their status is the
  standard's, not one of ours.
- **It does not keep state keyed to a connection**, which is what makes reconnection and gateways
  uneventful.
- **It does not silently accept a configuration it does not understand** — an unknown key stops the
  start.

On the last point, a related one worth knowing: a Client that cannot find its configuration file
does not fall back to defaults and carry on. Coming up on defaults would mean dialling a development
endpoint and managing nothing, which is the failure hardest to notice — so it refuses to start
instead.

## Running it

```console
$ supervisor --config /etc/opamp/supervisor.toml     # foreground; `run` is implied
$ supervisor run --config /etc/opamp/supervisor.toml # the same thing, said explicitly
$ opamp-fleet --version
```

| Global flag | Meaning |
|---|---|
| `--config <path>` | The TOML configuration file. Defaults to `supervisor.toml`; defaults apply if it does not exist. `service install` is the one place where "not given" means something else: there the file is `supervisor.toml` inside the data root, because a path resolved against this shell's working directory is not one the service manager shares. |
| `--state-dir <dir>` | Overrides the configuration file's `state_dir`. `service install` bakes this into the unit, so an installed service never depends on a relative path. |

There are no environment-variable fallbacks for configuration — the flags say only where
the file is and which instance is meant. Logging goes to stderr and is controlled by `RUST_LOG`
(default `info`).

The Client stops on `SIGTERM`/`Ctrl-C` (an SCM stop control on Windows) and sends the OpAMP
`agent_disconnect` goodbye before it goes, so the fleet view shows it as deliberately gone rather
than as a host that fell off the network.

## Running it as an OS service

The Client registers *itself* with systemd, launchd, or the Windows SCM — there is no
packaging step and no unit file to write:

```console
$ opamp-fleet service install --config /etc/opamp/supervisor.toml   # root / Administrator
$ opamp-fleet service start
$ opamp-fleet service status
$ opamp-fleet service stop
$ opamp-fleet service uninstall      # deregisters; never deletes the install layout or state
```

| Flag | Applies to | Meaning |
|---|---|---|
| `--user` | every `service` action | Target the current user's service manager instead of the system one. Useful in development; the default is a system service that starts at boot. |
| `--root <dir>` | `service install` | The layout root: `versions/` and the `current` pointer. Given alone it also takes the data — `supervisor.toml` and `state/` — so everything lands under the one directory you named, whose file labeling is then yours to manage. Without it the defaults apply per platform and scope, and on Linux system installs they are two directories: `/opt/opamp-fleet` for the layout, `/var/lib/opamp-fleet` for the data, because SELinux never lets systemd start a binary labeled for `/var/lib`. macOS uses `/Library/Application Support/opamp-fleet`, Windows `%ProgramData%\opamp-fleet`, and user scope the user's own data directory — one directory each. No path is ever fixed. See [On-disk layout](#on-disk-layout). |
| `--data-root <dir>` | `service install` | The data root — `supervisor.toml` and `state/` — when it is to differ from the layout root. The MSI passes both, so a Windows host installed that way keeps its program under `Program Files` and its identity under `%ProgramData%`. |
| `--interactive` | `service install` | Ask for the settings a fresh host cannot guess and write the configuration file before registering the service. See below. |
| `--endpoint <url>` | `service install` | Write the configuration file with this endpoint instead of asking for it — the same file, from an answer given rather than typed at a prompt. Mutually exclusive with `--interactive`, and it keeps an existing file just as `--interactive` does. Takes no credential on purpose: a flag stands in the shell history and the process list. |
| `--run-as <account>` | `service install` | Run the service as this account instead of root/`LocalSystem`, and hand its files over to it. See [Running it under its own account](#running-it-under-its-own-account). System scope only — a `--user` service already runs as its user. |

### Running it under its own account

By default the system service runs as root (systemd, launchd) or `LocalSystem` (Windows).
`--run-as` drops that (ADR-0062): the service — and every Managed Process its Supervisors spawn —
runs as the account you name, and the install hands its files over to it: the
configuration file, the state directory, **and the executable layout**. The layout too because
the self-update runs *inside* the service — a layout the account cannot write would silently end
[server-driven updates](#self_update) for that host.

On Linux and macOS the account must already exist; the install refuses early if it does not:

```console
# useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin opamp-fleet
# opamp-fleet service install --run-as opamp-fleet
```

On Windows only **passwordless** account forms are accepted — there is deliberately no password
flag, for the same reason `--endpoint` takes no credential. The recommended form is the service's
own virtual account, which Windows provisions and password-manages by itself:

```console
> opamp-fleet service install --run-as "NT SERVICE\opamp-fleet"
```

A group-managed service account (`DOMAIN\name$`) and the built-ins `NT AUTHORITY\LocalService` /
`NT AUTHORITY\NetworkService` are accepted as well. The install grants the account Modify on its
directories; it does not touch the *Log on as a service* right — the default security
policy grants it to `NT SERVICE\ALL SERVICES` (covering the virtual account), the built-ins carry
it inherently, and a gMSA gets it from its domain's group policy. On a host hardened to remove
that default grant, restore the right for the account or the service will not start.

Two consequences to weigh before using it:

- **The account is a trust boundary.** Whoever holds it can replace the binary in the layout, and
  the packaged `/usr/bin/opamp-fleet` symlink resolves through that layout's `current`
  pointer — an administrator invoking the CLI executes account-owned code.
- **The account's limits are the fleet's.** Anything under this Client that needs a port below
  1024 or root-only telemetry sources will fail — that trade-off is the point of the flag.

Re-running `install` with a different `--run-as` re-registers and re-owns the same directories;
without the flag it registers exactly as before — root/`LocalSystem`, no handover.

### The first configuration, on a host that has none

A release artifact is the bare binary, so a freshly downloaded Client has no `supervisor.toml` to edit.
Without one it still installs and starts — on the development defaults, dialling `127.0.0.1` and
managing nothing. `--interactive` is the way past that:

```console
$ opamp-fleet service install --interactive        # root / Administrator
No configuration at /var/lib/opamp-fleet/supervisor.toml — answering these writes it …
Server OpAMP endpoint [ws://127.0.0.1:4320/v1/opamp]: wss://fleet.example.com/v1/opamp
This Agent's name (service.instance.name) [Supervisor Agent]: host-01
Authentication toward the Server: bearer token
Bearer token: ********
Does the Server present a certificate from a private CA? [y/N]: n
Allow the Server to update this Client's own binary? [y/N]: n
wrote /var/lib/opamp-fleet/supervisor.toml
installed supervisor
```

What it asks about is only what has no useful default here: the endpoint, the Agent's name, the
credential ([`[auth]`](#auth)), a private CA when the endpoint is `wss://` or `https://`
([`[tls]`](#tls)), and last — defaulting to **yes** since ADR-0075 — consent for the Server to
replace this Client's own binary ([`[self_update]`](#self_update)). Everything else is written into the file as commented
defaults. The credential is typed into a hidden prompt rather than passed as a flag, so it stays out
of the shell history and out of the process list; on Unix the file is created mode `0600`.

Four rules worth knowing before you script around it:

- **Interactivity is never assumed.** Without the flag, `install` behaves as it always has — it only
  prints a warning when the path it is about to bake into the unit holds no file.
- **An existing file is kept, never overwritten.** Re-running `--interactive` on a configured host
  says so and carries on, so a re-install cannot eat a credential typed into the first one.
- **No terminal, no questionnaire.** `--interactive` in a provisioning run, a container build, or a
  pipeline fails with a message instead of blocking forever on an answer nobody can give.
- **Where it writes:** the path from `--config` when you name one, and otherwise
  `<root>/supervisor.toml` inside the install root — the same per-platform, per-instance location the
  versions and the state directory already use. The file is validated by the ordinary loader before
  the service is registered; a file that does not parse fails the install and stays on disk for you
  to correct.

Where there is an answer but no terminal — a provisioning run, an MSI dialog, a `%post` script —
`--endpoint` writes the same file without asking:

```console
$ opamp-fleet service install --endpoint wss://fleet.example.com/v1/opamp
wrote /var/lib/opamp-fleet/supervisor.toml for wss://fleet.example.com/v1/opamp
installed supervisor
```

It writes only the endpoint; everything else keeps its default, and the credential goes into the
file afterwards. All four rules above hold unchanged — in particular, an existing file is kept.

### Installing from a native package

A release also ships a `.deb`, an `.rpm` and an `.msi`. They deliver the binary to
`/usr/libexec/opamp-fleet` (Windows: the folder you choose) and then run `service install`
themselves — the layout, the unit and the SCM entry are the same ones this page describes, because
they are made by the same command. No package ships a unit file of its own. What lands on `PATH` —
`/usr/bin/opamp-fleet` — is a symlink through the layout's `current` pointer, so
the command you type is always the binary the service runs.

```console
$ sudo apt install ./supervisor_1.2.3_linux_amd64.deb
$ sudo dnf install ./supervisor_1.2.3_linux_amd64.rpm
```

**The service is registered and left stopped.** That is deliberate: a Client with no configuration
would dial the development default and manage nothing, and a package must not manufacture that state
on every host it touches. Two steps remain, and the post-install prints them:

```console
$ sudo opamp-fleet service install --endpoint wss://fleet.example.com/v1/opamp
$ sudo systemctl start opamp-fleet
```

On Windows the `.msi` asks for the installation folder and the endpoint. The folder is the install
root — the `.exe`, `supervisor.toml`, `versions/`, `current` and `state/` all live under it. The same
file installs unattended with the same two answers, which is how Intune, Group Policy and SCCM
deploy it:

```console
C:\> msiexec /i supervisor_1.2.3_windows_amd64.msi /qn ^
       INSTALLFOLDER="C:\Program Files\opamp-fleet" ^
       ENDPOINT="wss://fleet.example.com/v1/opamp"
```

Two things to know about living with a packaged install:

- **`dpkg -l` reports the version it *delivered*, not the one that is running.** After a fleet
  self-update ([Updating the Client itself](#updating-the-client-itself)) the service runs the binary
  under `<root>/current/`, which no package manager owns — that separation is what keeps the next
  `apt upgrade` from silently reverting the Server's decision. `opamp-fleet --version` goes
  through `current` and answers for the running binary, as does the fleet view; those two
  are the truth.
- **Removing the package stops and unregisters the service and uninstalls every staged version.**
  `versions/` and the `current` pointer go with the package; the state directory and
  `supervisor.toml` stay, for the same reason an install never overwrites a configuration: it may hold
  a credential you typed. `apt purge` deletes those too — the instance directory whole. A reinstall
  after a plain remove keeps the host's identity and configuration and stages its own binary fresh.

macOS has no native installer; there, unpack the `.tar.gz` and run `service install` yourself.

### What the service is called

One name on every platform, and it is the **product's** name — not the program's:

| Platform | Service |
|---|---|
| **Linux** (systemd) | `opamp-fleet.service` |
| **macOS** (launchd) | `opamp-fleet` (job and plist) |
| **Windows** (SCM) | `opamp-fleet` |

So the same command works everywhere it exists: `systemctl status opamp-fleet`,
`launchctl list opamp-fleet`, `sc query opamp-fleet`.

The name is fixed when the program is built, which is why there is no flag to change it. A second
Client on one host is a second build with its own product name, and it therefore has its own
service, its own package and its own directories — nothing about it collides with the first.

Where a platform has a second, human-readable name, it is **OpAMP Fleet Agent**. That is the Windows
services list; systemd shows the unit name as its `Description`, and a launchd job has no name
besides its label.

### Where the service's logs are

A Client started by the service manager writes its own log to **`<state_dir>/logs/`** on every
platform, one file per day, seven days kept:

```
<state_dir>/logs/supervisor.2026-08-09.log
```

**On Windows this is the only copy there is** — the SCM discards a service's stderr, so `sc query`
telling you the service will not start is all the platform itself offers. On Linux and macOS the
same lines are also in `journalctl -u opamp-fleet` and Console/`log show`; the file is
written anyway so the answer to "where are the logs" is the same everywhere, including in a
container where neither exists.

Running the Client **in the foreground writes no file** — stderr is right there in front of you.

The `[logging]` section moves it, changes how many days are kept, or turns it off:

```toml
[logging]
dir = "/var/log/opamp"   # default: <state_dir>/logs
keep = 7                 # daily files kept, then deleted
enabled = false          # write nothing; for a host whose platform already collects stderr
```

`keep = 0` is **refused at startup**. It is a retention bound, not a switch — on a fleet host the
unbounded setting is the one that eventually fills a disk, so turning the log off is spelled
`enabled = false` and cannot be reached by typing a zero. If the directory cannot be written, the
Client says so and runs anyway: a monitoring agent that refuses to start because of its own log
file has turned a diagnostic into an outage.

This is a different thing from the Client's own telemetry (`ReportsOwnLogs`), which ships
log records to a destination the **Server** offers. That needs a Server it can already reach — which
is exactly what a bad `supervisor.toml`, an unusable certificate, or a refused endpoint does not give
it. The file on disk is what explains those.

The Windows services list has a **Description** column beside that name, and it is a separate field
that nothing fills on its own — a service can carry a display name and still show an empty
description, which is what this one did. It now reads **OpAMP Fleet Agent for Windows** — the
display name beside it says what the service is, and the description says what it is for.

Both are set right after the registration, with `sc.exe config` and `sc.exe description`.

The service is registered against `current/supervisor` — a pointer, not a version directory — so
switching versions never re-registers it. Where the layout and the data live on each platform, and
what an uninstall leaves behind, is [On-disk layout](#on-disk-layout).

After a crash the service manager restarts the service; after an explicit stop it stays down. Known
platform gaps: on macOS `service status` is advisory and `install` does not
auto-start, and on Windows the SCM discards stderr, so service logs are lost until logging to a file
lands.

### Windows needs an elevated shell, and says so before it writes

Registering a machine-wide service needs Administrator, and a running process cannot raise its own
rights — there is no UAC prompt to be had from inside a command that has already started. So
`service install` asks the service control manager up front whether this process may register a
service at all, and stops with a message naming the fix if it may not:

```console
C:\> opamp-fleet service install
the Windows service control manager denied access: registering a machine-wide service needs
Administrator, and a running process cannot raise its own rights. Open a shell with "Run as
administrator" — from PowerShell, `Start-Process powershell -Verb RunAs` — and run this command
again. Nothing has been installed or written.
```

That the check comes *before* the first write is the point of it: `%ProgramData%` lets an ordinary
user create folders, so an install refused only at `sc create` had already staged a version directory
and pointed `current` at it, leaving half an install behind. `uninstall`, `start`, and `stop` write
nothing beforehand and simply report the manager's own refusal.

## On-disk layout

Where a managed host keeps things: the program, its configuration, its state, and the Agents it
supervises. One structure everywhere — only the root differs, and on two platforms there are two of
them.

### The shape

Two roots, and everything hangs off them:

```
<layout-root>/                       the program, replaceable
  versions/<product>-<version>-<commit>/supervisor
  current -> versions/<product>-…/

<data-root>/                         everything a reinstall cannot recreate
  supervisor.toml
  state/
```

The split is worth reading twice, because it is the rule the rest of this page follows. The
**layout root holds what a package can put back**: program files, one directory per version, and a
pointer at the live one. The **data root holds what nothing can put back**: the identity this host
reports to the Server, the credential an operator typed, and the configuration the fleet sent.

Directories are named after the **product**; the file inside is the **program**. They are not the
same name and are not meant to be — two products built from this source differ in the first and
share the second, which is what lets one published release update both.

### Where the roots are

| Platform | Scope | Layout root | Data root |
|---|---|---|---|
| Linux | system | `/opt/<product>` | `/var/lib/<product>` |
| Linux | user | `$XDG_DATA_HOME/<product>` | *the same* |
| macOS | system | `/Library/Application Support/<product>` | *the same* |
| macOS | user | `~/Library/Application Support/<product>` | *the same* |
| Windows | system, installed by hand | `%ProgramData%\<product>` | *the same* |
| Windows | user | `%LOCALAPPDATA%\<product>` | *the same* |
| Windows | system, installed by the MSI | `C:\Program Files\<product>` | `%ProgramData%\<product>` |

On Linux, `$XDG_DATA_HOME` falls back to `~/.local/share` when it is unset.

`--root` overrides the layout root and, given alone, collapses both into the one directory it
names — whose file labeling and permissions are then yours to manage. `--data-root` names the other
half; the MSI passes both. No path is ever compiled in.

### Why two roots, and only sometimes

The platforms that split do so for different reasons, and the platforms that do not split have
neither.

**Linux at system scope** splits because of SELinux. A file created under `/var/lib` carries a type
that an enforcing policy will not let the init system execute — the service would register cleanly
and then die at its first start. `/opt` carries a type the policy treats as an entry point for
third-party software, and files staged there later inherit it. This is why an update can put a new
version in place without any relabeling step.

**Windows installed by the MSI** splits because of ownership. `Program Files` belongs to the
installer and is meant to be read-only once installation finishes; a service writes there only
because it runs with system privileges. `%ProgramData%` is where a Windows program keeps machine-wide
data it changes at runtime, which is what the configuration and the state directory are.

**Everywhere else there is one root**, because neither reason applies: macOS has no equivalent
restriction, a user-scope service runs in the user's own context, and a Windows install done by hand
was never under `Program Files` to begin with.

Note the last row of the table against the one above it: the same platform, two roots or one,
depending on how it was installed. That is deliberate — the MSI puts the program where Windows
administrators expect to find installed programs, and a hand-unpacked install has no reason to.

### Inside the layout root

```
<layout-root>/
  versions/
    <product>-1.2.3-a1b2c3d/
      supervisor              (supervisor.exe on Windows)
      manifest.toml           the full version string and the program's hash
    <product>-1.2.2-9f8e7d6/
  current -> versions/<product>-1.2.3-a1b2c3d/
```

Every version sits beside the ones before it, and `current` points at the live one. The service is
registered against `<layout-root>/current/supervisor`, never against a version directory — which is
why switching versions never re-registers the service.

The version part of a directory name is the plain `MAJOR.MINOR.PATCH`; a pre-release suffix is not
in the name. The trailing part is the commit the build came from. `current` is a symlink on Linux
and macOS, and a directory junction on Windows, where junctions need no special privilege.

### Inside the data root

```
<data-root>/
  supervisor.toml           the file you edit
  state/
    instance-uid            this host's identity to the Server
    remote-config.pb        the last configuration it received
    connection-settings.pb  Server-offered settings, if any
    installed-package.json  the update it last installed, if any
    packages/               staging for its own update
    logs/                   the service's rotating log
    supervisors/            one directory per Agent — see below
```

`instance-uid` is the file that matters most. It is what makes this host *the same* Agent across
restarts and upgrades; delete it and the Server sees a host it has never met, while the old row
lingers. Nothing regenerates it, which is why the data root survives an uninstall.

The state directory's location follows `state_dir` in the configuration file when that names an
absolute path. Left alone, it is `<data-root>/state`.

Logs rotate daily and seven files are kept. On Linux and macOS the service's output also reaches the
system journal; on Windows the file is the only copy, because the service manager discards a
service's console output.

### One directory per Agent

Every `[[supervisor]]` block gets a directory of its own, holding everything about that Agent:

```
state/supervisors/<name>/
  instance-uid              this Agent's own identity
  remote-config.pb          the last configuration it received
  installed-package.json    the package its program currently is
  config/                   one file per Configuration it matched
  program/                  its program
  program/tree/             …or the whole unpacked package, for a multi-file one
  packages/                 staging its downloads pass through
```

Two things follow from keeping them together.

**The staging directory sits beside the program**, so installing an update is a rename inside one
filesystem rather than a copy across two. A rename either happened or did not; a copy can be
interrupted halfway.

**Removing an Agent removes the directory**, whole. Take a block out of the configuration and the
Client stops the process and deletes everything above — program, packages, configuration and
identity. A directory that no block names is reported at startup and never deleted on its own, so a
typo in a name does not destroy an Agent's identity.

`config/` is what `${config_dir}` expands to in a process's arguments, which is how a program is
pointed at its own configuration without anyone writing an absolute path into a block. Where a
Configuration carries a role rather than configuration text, it is written here too and named in a
`.supplementary` file beside the entries, so a program is not started with content it is only meant
to read.

### What the packages add

The `.deb` and `.rpm` deliver exactly one file and let the program lay out the rest:

```
/usr/libexec/<product>/supervisor    the payload, off PATH
/usr/bin/<product>  ->  /opt/<product>/current/supervisor
```

The command on your `PATH` resolves through `current`, so it is always the running version rather
than whichever one the package delivered. The payload keeps a separate copy under `/usr/libexec`
because the package manager needs a file it owns — the layout under `/opt` is the program's, not
the package's.

The MSI delivers its program into the layout root and then runs the same install the command line
would, so a Windows host ends up with the same structure by a different route.

### What an uninstall leaves behind

| What you do | What goes | What stays |
|---|---|---|
| `service uninstall` | the service registration | everything on disk |
| `apt remove` / `dnf remove` | the registration, the layout root, the `PATH` symlink | the data root: configuration, state, every Agent's identity |
| `apt purge` | all of the above | nothing |
| MSI uninstall | the registration and the layout root | the data root |

The pattern is the same everywhere: **removing the program is not removing the host from the
fleet.** A reinstall over a surviving data root comes back as the same Agent, with the same identity
and the same credential, and the Agents it supervises come back as themselves too.

This is also why the data root is not under `Program Files` on Windows or under `/opt` on Linux: an
uninstall clears those, and a credential someone typed is not something an uninstall should quietly
take with it.

### Persisted connection settings override the file

`<data-root>/state/connection-settings.pb` takes precedence over `endpoint`, `[auth]`, and the
intervals in `supervisor.toml`. Delete it to revert to what the file says.

## Configuration reference

The full annotated example is [`config/supervisor.toml`](../../config/supervisor.toml). Every key is
optional and shown below with its default; an unknown key fails startup rather than being ignored.

### Top level

| Key | Default | Meaning |
|---|---|---|
| `endpoint` | `"ws://127.0.0.1:4320/v1/opamp"` | The Server's OpAMP endpoint. The scheme selects the transport: `ws://`/`wss://` for WebSocket, `http://`/`https://` for polling. |
| `name` | `"Supervisor Agent"` | The Agent's `service.instance.name` — your name for *this* Client, shown in the fleet view and matchable by a Selector. Its `service.name` is the constant type `supervisor`, the same on every host. |
| `poll_interval_secs` | `30` | How often the plain-HTTP transport polls. Ignored on WebSocket. |
| `heartbeat_interval_secs` | `30` | Heartbeat interval on WebSocket. `0` disables heartbeats and undeclares the capability; on plain HTTP every poll already is the periodic report. |
| `max_message_size_bytes` | `67108864` (64 MiB) | The largest OpAMP message sent or accepted, in either direction — including on the Supervisor Endpoint. A message past it is never sent, and an oversized one from the Server is refused. |
| `state_dir` | `"client-state"` | Where the Client persists its own Agent's identity, its remote configuration, and any Server-offered connection settings. A self-update artifact is streamed here first, so it needs room for one agent binary. |
| `supervisor_dir` | `<state_dir>/supervisors` | Where the per-Supervisor directories live. Set it when the programs must not live where the state does — `/var/lib` is often mounted `noexec`, and sized for state rather than for a few hundred megabytes of Collector. |

> **Moving `supervisor_dir` on a running host leaves the old tree behind**, `instance-uid` included,
> so every Supervisor re-registers as a **new** Agent on the Server. Nothing migrates automatically.

### `[attributes]`

Operator-defined attributes reported by every Agent this Client presents, so the Server's Selectors
can target them. They are reported as non-identifying attributes, and they never override
what the code or the Managed Process reports under the same key.

```toml
[attributes]
env = "prod"
region = "eu-central"
```

A `[[supervisor]]` block may add its own `[supervisor.attributes]` table, which overrides these per
key for that Agent alone.

Every Agent additionally reports, without configuration, everything the protocol names to describe
an Agent and where it runs: `service.name`, `service.instance.name`, `service.instance.id`,
`os.type`, `os.name`, `os.version`, `os.description`, `host.name`, `host.arch`, and `host.id` — plus
`service.version`, which for the Client's own Agent is the Client's baked-in version and for a
Supervisor-backed Agent is whatever the Managed Process reports about itself.

The first two are the pair to keep apart: `service.name` is *what* this Agent is — the
type, shared by every Agent of that kind — and `service.instance.name` is *which* one it is, the
name you gave it. Neither is settable through `[attributes]`: a table entry under either key is
ignored, since the Supervisor already reports both.

An attribute the host cannot answer is **left out, never reported empty** — a container without
`/etc/machine-id` reports no `host.id` at all rather than a blank one a Selector could match. So a
Selector on `host.id` reaches exactly the hosts that have one.

One attribute is configured rather than detected, because only an operator knows it — the protocol
asks for `service.namespace` "if it is used in the environment where the Agent runs":

```toml
service_namespace = "telemetry"
```

Unlike `[attributes]`, it *identifies* the Agent rather than tagging it, which is where the protocol
puts it. Leave it out and nothing is reported.

### `[tls]`

```toml
[tls]
ca_file = "ca.pem"             # trust: replaces the built-in roots
cert_file = "client.pem"       # identity: what this Client presents
key_file = "client-key.pem"
```

Every key is optional on its own, so this section may carry a trust override, a client identity, or
both.

`ca_file` is the trust override for `wss://`/`https://` endpoints whose certificate comes from a
private CA. Without it the platform's trust store applies.

`cert_file` and `key_file` are this Client's own certificate for a Server that requires mutual TLS
— they go together or not at all. This is the identity an operator provisions, including
the **bootstrap certificate** a fresh host enrols with. A certificate the Server issued outranks it:
the Client stores that pair in its state directory as `client-cert.pem` and `client-key.pem` and
prefers it, the same precedence persisted connection settings have over `supervisor.toml`. Deleting the
stored pair reverts to what is written here.

**Enrolment needs nothing in this file.** When the Server declares that it signs certificates, a
Client without one generates a key — which never leaves the host — sends a signing request, and
receives a certificate through the ordinary offer flow, renewing the same way once it is two thirds
through its validity. The private key is written `0600`; on Windows the state directory's ACL is
what protects it.

### `[auth]`

Exactly one scheme: `bearer_token`, **or** `username` and `password` together. Mixing them, or
giving half of one, fails at startup.

```toml
[auth]
bearer_token = "a-long-random-token"
# --- or ---
# username = "fleet"
# password = "a-strong-password"
```

The `Authorization` header rides every plain-HTTP request and the WebSocket upgrade. Without TLS the
credential travels in cleartext; the Client logs a warning when it sends one over `ws://`/`http://`
to anything but the loopback interface, but it does send it. Pair `[auth]` with `wss://` or
`https://` for anything real.

### `[packages]`

```toml
[packages]
verification_key = "<hex-encoded Ed25519 public key>"
archive_key = "the key an encrypted .7z was packed with"
```

| Key | Meaning |
|---|---|
| `verification_key` | With a key set, every Server-offered package **must** carry a valid Ed25519 signature over its artifact. Without it, an unsigned package installs on its content hash alone and a **signed** one is refused. Generate the key with `opamp-package-sign keygen` (see [the Server](server.md#packages-distributing-software)). |
| `archive_key` | Opens an encrypted `.7z` artifact. One secret for the fleet — a single archive serves every Agent — and never the `[auth]` credential, which the Server may rotate on its own: a rotation would leave every archive unopenable. The Server never learns this key; the artifact stays encrypted wherever it is stored and is opened only on the host that runs it. |

Note what is *not* here: which artifact a Supervisor receives is the Server's decision, expressed as
the package's Agent type and its Selector, never a key in this file.

### `[self_update]`

```toml
[self_update]
enabled = true                     # the default; false withdraws the consent
package = "supervisor"             # the default: this Client's own Agent type
```

See [Updating the Client itself](#updating-the-client-itself). **An absent section is the consent**
(ADR-0075): a Client the fleet cannot update is the one program on the host left to patch by hand.
What bounds it is the name — an offer under any other is refused and reported, never applied — and
the default name is the product's own, which is what the release artifact and therefore the Set
carrying this Client is named. Not the Agent type: since ADR-0077 the two are different strings, and
a default taken from the type would narrow the consent to a package nobody publishes.

To withdraw the consent, say so; there is no third state:

```toml
[self_update]
enabled = false
```

An empty `package` with `enabled = true` fails at startup rather than widening the consent to
whatever the Server offers next. Every install path can answer this: `service install
--no-self-update`, `--self-update-package <NAME>`, the `--interactive` questionnaire (which asks and
defaults to yes), and the MSI's checkbox or `SELFUPDATE=0` on a silent deploy.

## Gateway Mode: carrying other Clients

A Client can stand at a network boundary and carry other Clients' Agents upstream over a small pool
of connections — for a segmented network the Server cannot reach into, or simply for a
fleet too large to give every Agent its own connection:

```toml
[gateway]
listen = "0.0.0.0:4320"
upstream_connections = 10          # a cap, not a count
```

Point the Clients behind it at this address instead of the Server's. Nothing else about them
changes: the Server tells Agents apart by `instance_uid`, never by the connection that carried them,
so an Agent behind a Gateway is as manageable as one in front of it. Both transports are served
downstream, so a polling Client works as well as a WebSocket one.

`upstream_connections` is a **ceiling**. Connections are opened as Agents appear, so a Gateway in
front of three Agents holds three, and each Agent stays on its connection while that lives.

This mode composes with `[[supervisor]]` blocks: one host may supervise its own processes *and*
gateway for others.

### What a Gateway does not do

- **It makes no authentication decision.** Each downstream peer's credential is forwarded upstream
  untouched, so policy stays on the Server and rotating a credential never means visiting gateways.
- **It never speaks for an Agent.** If a downstream Client disappears without sending
  `agent_disconnect`, the Gateway forwards nothing — inventing that message would tell the Server
  the Agent said something it did not. What makes such an Agent visible instead is the Server's
  staleness flag: the connection stays up, because it is the Gateway's, and the row reads
  **Connected + Stale**. It needs a heartbeat configured on the Agent to work, since staleness only
  applies to Agents that promised to report periodically.
- **It does not carry a downstream client certificate upstream.** Mutual TLS is per hop:
  `[gateway.tls]` verifies the Agents connecting here, and the identity presented to the Server is
  this Client's own, from the top-level `[tls]` or issued through the CSR flow.

```toml
[gateway.tls]
cert_file = "gateway.pem"          # what this Gateway presents to its Agents
key_file = "gateway-key.pem"
client_ca_file = "client-ca.pem"   # optional: require a certificate from them
```

The upstream endpoint must be `ws://` or `wss://`. A polling connection cannot carry the Server's
pushes to the Agents behind a Gateway, and the configuration refuses it at startup rather than
leaving you to notice that configuration changes never arrive.

## Supervisors: putting a process under management

Each `[[supervisor]]` block runs one Supervisor managing one local process, and appears to the
Server as its own Agent. Without any block the Client presents itself as a single Agent and manages
nothing.

Two plugin types ship today: `collector` for an OpenTelemetry Collector, and `command`
for any other process — a **Foreign Agent** that speaks no OpAMP. A new kind of process means a new
plugin, not a change to the core.

### The Server can manage the set

The `[[supervisor]]` blocks are the fleet-manageable half of `supervisor.toml`. A
Configuration typed for the Client itself — `service_name = "supervisor"` —
carries `[[supervisor]]` blocks in its body, and a matching Client applies them as its new set:

- **Only the blocks are read.** Every other top-level key in the offered document is ignored —
  the endpoint, the credential, the state directory stay the host's, and can never arrive over
  the wire. You may roll out a full `supervisor.toml`-shaped document; exactly its supervisor half
  takes effect. A duplicate `name` fails the offer, as it would fail the file.
- **The apply is a diff, keyed by `name`.** Removed and changed Supervisors are stopped, the
  merged file is written, changed and added ones are started from it. An unchanged Supervisor's
  process is not touched — a fleet-wide change to one collector does not cycle its neighbours.
- **A removed Supervisor is purged**. Once the rewritten file no longer names it, its
  whole directory `<supervisor_dir>/<name>/` is deleted — identity, written configuration,
  staged packages, and the Client-owned program. A changed Supervisor restarts under its name
  and keeps its directory; the program itself is
  never touched. Removal is destructive on the host: re-adding the same name later starts a
  genuinely fresh Agent, restoring service, not history.
- **`supervisor.toml` stays the single truth.** The blocks are written into the file itself,
  surgically: your comments, ordering, and formatting outside them survive. A Client restarting
  offline starts the Server-delivered set, because it is in its file.
- **The outcome is a status, not a silence.** The Client acknowledges `APPLYING`, then `APPLIED`
  once the file is written and the starts are issued — or `FAILED` with the reason when the
  offer does not parse, a block does not validate against this host's globals, or the write
  fails (then nothing is applied and the running set stays in force). A body that is not TOML —
  say, a Collector YAML rolled out fleet-wide with no type — is refused the same way, which is
  one more reason to state whom a Configuration is for.

A Client whose Server never rolls such a Configuration out runs its locally written blocks
exactly as before. Note that once one applied, the Server's set is authoritative: a later local
edit to the blocks stands only until the next rollout act overwrites it.

### Keys every block accepts

| Key | Default | Meaning |
|---|---|---|
| `type` | — | `"collector"` or `"command"`. Required. |
| `name` | — | This Agent's `service.instance.name` — your name for it — and the directory name it owns. Required; 1–32 lowercase letters, digits, and `-`. Must be unique in the file. A Managed Process can never overwrite it. |
| `service_name` | the program's file name | This Agent's `service.name`: the Agent **type** it presents. A Managed Process that reports a type of its own wins over it — a Collector with the `opampextension` states the `dist.name` it was built with — so set it for a process that reports nothing. Unlike `name` it may be a reverse FQDN, as the protocol recommends; only an empty value is refused. |
| `endpoint_port` | `0` (ephemeral) | The port of the Supervisor Endpoint on `127.0.0.1`. The endpoint always comes up; pin the port when something is meant to connect to it. |
| `stop_timeout_secs` | `10` | Graceful-stop budget before the process is killed. |
| `apply_grace_secs` | `3` | How long a restarted process must survive before a received configuration is acknowledged `APPLIED`. `0` acknowledges on start. |
| `retain_previous_secs` | global `[updates]` value | How long the version a successful update supersedes is kept before deletion, overriding the global default for this Supervisor. `0` deletes it on success. See [Package updates: rollback and retention](#package-updates-rollback-and-retention). |
| `program_path` | unset | Where the program sits *inside* a package that is a whole directory tree, e.g. `bin/fluent-bit`. Unset means the package is a single file. See [Agents that are more than one file](#agents-that-are-more-than-one-file). |
| `[supervisor.attributes]` | none | Attributes for this Agent alone, overriding the Client's `[attributes]` per key. |

Two keys were **removed** and now fail at startup with a message saying what to do instead:
`package` (the Server aims packages by Selector now) and `accepts_packages` (every Agent's
path decides).

### `type = "collector"`

| Key | Meaning |
|---|---|
| `binary` | The Collector program, named by a bare file name. See [How a block names its program](#how-a-block-names-its-program). |
| `args` | Extra arguments, appended **after** the `--config` flags the Supervisor builds — with [placeholder expansion](#path-placeholders). |
| `[supervisor.env]` | Additional environment for the Collector process — the natural home for a value the config reads as `${env:VAR}`, e.g. a per-host endpoint. Expanded through the same placeholders. |

The Supervisor writes every received config-map entry into its own `config/` directory under the
Configuration's name and passes each **unroled** entry as its own `--config`; a `supplementary`
entry is written but never passed. A change restarts the Collector so it re-reads them.
The version is probed once with `--version`, so even a Collector without the extension reports one.

Prebuilt Collector distributions are published on the
[collector releases page](https://github.com/open-telemetry/opentelemetry-collector-releases/releases),
one `.tar.gz` per platform — ready to upload as a package as they are; the
[rollout walkthrough](rollout.md#2-build-the-artifact) shows the download and what to check before
uploading. Mind the member name: the binary inside is called after the distribution
(`otelcol-contrib`, `otelcol`), and `binary` must say the same.

A Collector **with** the `opampextension` reports its own description, health, and effective
configuration through the Supervisor Endpoint instead of being watched from outside. The extension
ships only in the Contrib distribution. Pin `endpoint_port` and make sure the configuration the
Server distributes carries the extension pointing at it:

```toml
[[supervisor]]
type = "collector"
name = "otelcol-contrib"
binary = "otelcol-contrib"
endpoint_port = 4321
```

```yaml
extensions:
  opamp:
    server:
      ws:
        endpoint: ws://127.0.0.1:4321/v1/opamp
        tls:
          insecure: true
service:
  extensions: [opamp]
```

A Collector **without** the extension needs no `endpoint_port`: the endpoint still comes up on an
ephemeral port, and nothing ever connects to it. Nothing reports an Agent type either, so state one
with `service_name` — left out, the program's file name is used anyway.

```toml
[[supervisor]]
type = "collector"
name = "otelcol"
binary = "otelcol"
service_name = "otelcol"
```

### `type = "command"`

For a Foreign Agent — anything that speaks no OpAMP and is brought into the fleet by translating its
lifecycle into the protocol.

| Key | Meaning |
|---|---|
| `command` | The program, named by a bare file name. See [How a block names its program](#how-a-block-names-its-program). |
| `args` | Its arguments, verbatim — apart from [placeholder expansion](#path-placeholders). |
| `working_dir` | The directory to start in. Optional. |
| `[supervisor.env]` | Additional environment for the process. |
| `version_args` | Arguments that make the program print its version, e.g. `["--version"]`. The program is invoked once with exactly these, and the first SemVer 2.0.0 version in its output becomes the Agent's `service.version`. Opt-in, because a Foreign Agent's version flag is its own convention. **They are also the preflight**: a package's staged program is run with them before what runs is stopped, and a non-zero exit refuses the package with the program's own message — so a build this host cannot run costs a refusal instead of a stop, a swap, a failed start and a rollback. |
| `reload_signal` | The signal that makes the program re-read its configuration in place: `"HUP"`, `"USR1"`, or `"USR2"` (a `SIG` prefix is accepted). When set, a configuration change is applied by sending this signal instead of restarting, and the process keeps its in-flight state; if the signal cannot be delivered or the process dies on it, the Supervisor falls back to the restart. Opt-in, because whether a daemon reloads on a signal is its own convention — and Linux/macOS only: on Windows the key is refused at startup. |

The Supervisor writes the received configuration entries the same way a Collector's does, but it
cannot know what to do with them — a Foreign Agent reads its own configuration file. So you point it
at the written entry with its own flag, and the Supervisor restarts the process on a change so it
re-reads it — or, with `reload_signal` set, signals it to re-read in place:

```toml
[[supervisor]]
type = "command"
name = "fluent-bit"
command = "fluent-bit"
args = ["-c", "${config_dir}/fluent-bit-conf"]
working_dir = "${supervisor_dir}"
version_args = ["--version"]
[supervisor.attributes]
role = "edge"
[supervisor.env]
FLB_LOG_LEVEL = "info"
```

Here `fluent-bit-conf` is the *name of the Configuration on the Server* — that is what the entry file
is called.

### `type = "icinga2"`

For Icinga 2 in the Agent role, which needs more than a program and arguments: it must be told where
its state, its template library and its account are on **every** invocation, it creates none of
those directories itself, and it obtains a certificate from an Icinga master before it can do
anything ([ADR-0068](../adr/0068-icinga-2-is-supervised-by-a-kind-of-its-own.md)).

| Key | Meaning |
|---|---|
| `binary` | The program, named by a bare file name — it is the delivered tree's, as everywhere. |
| `main_config` | The **name of the Configuration** that is Icinga's root configuration file. Icinga reads one file and `include`s the rest. |
| `include_dir` | Where the template library is inside the tree — reached with `-D IncludeConfDir`, which `include <itl>` resolves against. |
| `plugin_dir` | Where the check plugins are, for `PluginDir`. Optional. |
| `data_dir`, `log_dir`, `cache_dir`, `spool_dir`, `run_dir` | Where Icinga writes. Default to `${supervisor_dir}/…`, i.e. beside the tree, so a package update keeps the certificates. |
| `node_name` | This node's name: `NodeName`, and the common name its certificate is issued for. Defaults to the Supervisor's name. |
| `parent_host`, `parent_port` | The Icinga master or satellite. Absent means a standalone node with no enrolment. |
| `ticket_file`, `trusted_cert_file` | Where the enrolment ticket and the pinned parent certificate are read from — both delivered as `supplementary` Configurations (ADR-0069). The pinned file is the parent's *own* certificate, not its CA. |
| `renew_before_days` | How close to expiry a certificate may come before the Supervisor renews it at start. Default 30. |
| `run_as_user`, `run_as_group` | The account the daemon may drop to. Defaults to the account this Client runs as. |
| `log_level`, `args`, `[supervisor.env]` | Console severity, extra daemon arguments, and additional environment. |

The recipe with everything around it — building the artifact, the ticket, the configuration, and
what the fleet view shows — is [Rolling out and managing Icinga 2](icinga2.md).

A complete worked example — a third party's release repacked and delivered, run as a foreground
daemon, with the Windows interpreter invocation and the bootstrap of its configuration — is the
[GLPI Agent recipe](glpi-agent.md).

## How a block names its program

`binary` and `command` take a **bare file name** — `otelcol-contrib`, not a path. It names a file in
`<supervisor_dir>/<name>/program/`, a directory this Client creates and owns, which is what lets the
Server replace what is in it. Every Agent therefore accepts package updates.

Anything with a path separator in it — `/usr/local/bin/otelcol`, `./x`, `bin/x` — is a startup error
naming the rule, rather than a guess. A program the machine's package manager installed is not
something this Client supervises; to bring one under management, repack it as a package and deliver
it from the Server. The [GLPI Agent](glpi-agent.md) and [Icinga 2](icinga2.md) recipes show what that
looks like for two real agents.

One thing to know about a bare name: it is **not** searched for in `$PATH`. It names a file in that
one directory, and the first copy arrives by package like every later one.

The startup log states, per Supervisor, which program it resolved to.

## Path placeholders

A Foreign Agent is told where its configuration is *through its own command line*, and an absolute
path written there drifts the moment `supervisor_dir` moves or the Supervisor is renamed —
silently, because the process then starts happily on a file nobody writes to. Two placeholders
close that, in a Supervisor's operator-written strings — a `command`'s `args`,
`working_dir`, and `[supervisor.env]`, and a `collector`'s `args` and `[supervisor.env]`:

| Placeholder | Expands to |
|---|---|
| `${supervisor_dir}` | `<supervisor_dir>/<name>` — everything this Supervisor owns |
| `${config_dir}` | `<supervisor_dir>/<name>/config` — where the received configuration's entries are written |

Three rules go with them:

- **Any other `${…}` is passed to the process untouched.** A Foreign Agent's own configuration
  language may use the same syntax — Fluent Bit's does — and a Client that ate or refused those
  would break a working deployment to catch a typo. The flip side is that a *misspelled* placeholder
  (`${config-dir}`) is handed over rather than refused, unlike an unknown TOML key.
- **The program itself is never substituted**, in `binary` or `command`. It is a bare file name in a
  directory this Client owns, so there is no path to expand — and what a block runs must be readable
  in the file itself.
- **Expansion happens once, at startup.** None of these paths change while the Client runs.

## Package updates

Every Supervisor accepts packages, and the
Server offers it one built for the Agent type it reports and aimed at it by that package's
Selector — so a Supervisor reporting `promtail` is never handed the Collector's
binary, whatever anyone forgot to aim. What then happens on the host:

1. The artifact is **streamed to disk** in that Supervisor's `packages/` directory — never held in
   memory.
2. It is **verified**: the content hash always, and the Ed25519 signature whenever
   `[packages] verification_key` is set.
3. It is **unpacked** when it is a `.tar.gz`, a `.7z`, or a `.zip` — the member whose file name
   matches the configured program, so an upstream release can be uploaded exactly as published. An
   encrypted `.7z` opens with `[packages] archive_key`; an encrypted `.zip` is refused, because
   encryption is what `.7z` is for. A bare program artifact is moved into place rather than copied.
4. It is **swapped** over the program in `program/`, the process restarted, and the new version
   **health-gated** on `apply_grace_secs` — one that will not stay up is **rolled back**.
5. Progress is reported throughout: `Downloading` (with percent and bytes per second, repeated every
   5 s, so a transfer of hundreds of megabytes stays distinguishable from a stuck install), then
   `Installing`, then `Installed` or `InstallFailed`.

[The rollout walkthrough](rollout.md) runs this end to end, from packing the artifact to watching it
land.

One limit worth knowing before you plan a rollout: only a **top-level** package is installed. An
addon is something a Supervisor has no way to apply, so it is refused with `InstallFailed` rather
than written over the binary it was meant to extend.

### Package updates: rollback and retention

What happens when step 4's health gate is *not* passed:

- **A failed update rolls back to the version it replaced** — but only when there *is* one. A
  **first** install with nothing behind it is not rolled back to nothing: the verified program is
  **kept in place** and reported `InstallFailed`, so `program/` never goes empty and the Server does
  not re-offer the same artifact in a loop.
- **A program that keeps failing to start is held, not restarted forever.** After a few attempts in
  a row the Supervisor stops trying and waits for a change — a new configuration, a new package, or
  a restart — rather than spinning (which would hammer the Server with re-downloads). A rolled-back
  predecessor that also will not start is held the same way. The Agent reports it plainly
  (`not restarting: the program keeps failing to start`).
- **A successful update keeps the version it superseded for a window, then deletes it**, so an
  operator has a fallback if the new version proves subtly wrong. The window is
  `retain_previous_secs`: global in `[updates]`, overridable per `[[supervisor]]` block, **one day**
  by default. `0` deletes on success. Each Supervisor keeps at most the immediately previous version.

```toml
# Global default for every Supervisor (one day shown; the built-in default):
[updates]
retain_previous_secs = 86400
```

## Agents that are more than one file

An executable plus the shared objects it loads — Fluent Bit is the usual example — is delivered by
naming where the program sits inside the package:

```toml
[[supervisor]]
type = "command"
name = "fluent-bit"
command = "fluent-bit"            # bare: consent, exactly as everywhere else
program_path = "bin/fluent-bit"   # where the program sits inside the package
args = ["-c", "${config_dir}/fluent-bit-conf"]
```

With `program_path` set, the whole archive is unpacked into
`<supervisor_dir>/<name>/program/tree/`, keeping its own structure, and the program is
`program/tree/bin/fluent-bit`. Without it, nothing changes: one member, one file, as before.

**The path is matched from its end.** An upstream release wraps everything in a version-named
directory — `fluent-bit-3.1.0/bin/fluent-bit` — and that prefix is dropped, so `bin/fluent-bit`
keeps being right at the next release instead of naming a version. If it matches several members
the install is refused and they are listed; write more of the path to say which.

**The tree that was running is kept whole** as `program/tree.rollback` until the new one has
survived `apply_grace_secs`, and put back whole if it has not — a rollback of half a tree would run
nothing.

What an archive may contain is checked before anything is written, and one bad member refuses the
whole archive:

| Refused | Why |
|---|---|
| a member with `..` in its path, or an absolute path | It names somewhere outside the directory being unpacked into. |
| a symbolic or hard link | What it points at is not where it sits, which is the one thing a path check cannot judge. |
| more than 10 000 members, or more than 2 GiB unpacked | An archive that expands without end. |

Members outside the program's own directory — a `LICENSE` beside the wrapper — are not written, and
the count is logged rather than passed over in silence.

Two more things worth knowing. **A `.tar.gz` carries file modes and is the right format for a
tree** on Unix; a `.7z` and a `.zip` are opened too, but both store Windows attributes rather than
Unix modes, so only the program is made executable and a helper binary beside it would not be —
which costs nothing for a tree that runs on Windows, and is why the GLPI Agent's portable Windows
build ships as the `.zip` upstream published (see the [GLPI Agent recipe](glpi-agent.md)).

## Updating the Client itself

The Client is always its own Agent, so the Server can always see which version a host runs. Letting
the Server *replace* that version is opt-in, and the opt-in **names the package**:

```toml
[self_update]
package = "supervisor"
```

An offer under any other name is refused and reported, never applied. That is one of two independent
guards, and it is the one on this side of the wire: the Server will not offer a package built for
another Agent type either, and this Client's type is the constant `supervisor` — the same string,
which is why the Set that carries the Client is named and typed alike.
Neither guard replaces the other — an operator who types a Collector artifact as `supervisor` gets
past the Server, and this name is what is left.

On an accepted offer the artifact is verified like any other, staged as a new version *beside* the
running one in the install layout, and proved by running `supervisor self-check` on it before the
`current` pointer moves — which asks two things at once: does this binary run at all on this host,
and is it actually an OpAMP Fleet Agent at the version offered. The process then shuts down exactly
as an ordinary stop does — Managed Processes stopped gracefully, the goodbyes sent — and
exits, asking the service manager to restart it; it does not restart itself. A marker in `state_dir` carries the
outcome across that restart: the new version commits itself once it reaches the Server, and one that
will not stay up is rolled back to its predecessor after three attempts. Either outcome is reported
to the Server by whichever version came up.

Self-update therefore requires an installed service — it is the service manager that performs the
restart.

**What the Client says it has is the version it runs**, whether a package put it there or a `.deb`,
an `.rpm`, an MSI or a hand did. It reports that under the name `[self_update]` consents to from its
very first report, which is what lets the Server hold a Set against it: since
[ADR-0076](../adr/0076-a-set-reaches-an-agent-only-as-an-upgrade.md) a Set reaches an Agent only as
an **upgrade**, so a Client is never offered the version it already runs, and never an older one.
The practical consequence: a Client installed by hand is not taken over by the fleet's package the
moment one is published at the version it already is — it comes under package management with the
next release that is actually newer.

**Replacing the binary by hand does not fool it.** What the Client installed is recorded in
`<state_dir>/installed-package.json`, and `service uninstall` deletes neither the install layout nor
the state — so installing an older Client afterwards comes up on top of that record. A record that
does not name the version the running binary *is* is discarded at startup, with a warning naming
both versions. The Client then reports the version it actually runs, the Server sees its published
package as the upgrade it now is, and the host is updated back to it. To hold a host on an older
Client, retract the package on the Server first
([`publication`](server.md#packages-distributing-software)) — retracting withdraws the offer and
uninstalls nothing.

**Where the artifact comes from.** Every release publishes one archive per platform, named
`supervisor_<version>_<os>_<arch>.tar.gz`
([ADR-0078](../adr/0078-a-release-is-named-after-the-set-it-becomes.md)) — and that file *is* a
package artifact: it holds the Client under the name the install layout gives it, so it is uploaded
exactly as downloaded, and the SHA-256 the release published is the one the Agent verifies. Nothing
repacks it. The files are named after the **Set** they become, not after the product inside them:
the member is `supervisor`, which is what a Client looks for, while the file says which
package it is. The fields are separated by `_` because two of them carry `-` — a package name and a
prerelease version (`1.2.3-dev`) — so the last two fields are the platform and can be read off the
name.

**The version in the query is the release number** — the one in the file name — and the
platform is required with it, spelled as the Agent reports it:

```console
$ curl -X PUT --data-binary @supervisor_1.2.3_linux_amd64.tar.gz \
       "http://<server>:4321/api/v1/packages/supervisor?version=1.2.3&os=linux&arch=amd64"
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "supervisor"}' \
       http://<server>:4321/api/v1/packages/supervisor/type
```

The second call is what arms the package: until a type is set it is offered to nobody, so
an artifact uploaded and left untyped reaches no Client at all. For this one the type is the
Client's own, `supervisor` — and so is the Set's name, which is what `[self_update] package` above
consents to. The *file* keeps its published name; the Set is a label the Server holds.

The staged binary's `self-check` compares that against what it reports, ignoring the commit the
build came from — `1.2.3` and `1.2.3+a1b2c3d` are the same release, and the content hash is what
pins *which* bytes arrived. What is **not** ignored is the pre-release: a `1.2.3-dev` build offered
as `1.2.3` is refused, because a build heading for a release is not that release.

Two things follow. Passing the full string still works, but if you do, remember that a `+` in a URL
query is decoded as a *space* — it has to be written `%2B`, which is the reason the release number
is the better thing to type. And `opamp-fleet --version` prints the full string on any host,
which is what to quote when asking which build a host runs.

## Connecting to the Server

**Choosing a transport.** `ws://`/`wss://` gets configuration changes pushed within a second.
`http://`/`https://` polls every `poll_interval_secs`, which is the option for a host that cannot
hold a long-lived connection. Nothing else differs: both carry the same messages, and the Server
accepts both at once.

**Reconnecting.** A dropped connection is retried with capped exponential backoff, and the Client
honours the Server's `UNAVAILABLE` retry hints.

**Server-offered connection settings**. When the Server offers a new credential,
heartbeat interval, or endpoint, the Client **verifies the offer by actually connecting with it**,
persists it, and only then switches — across transports if the offered endpoint demands it. A
failed verification leaves the current settings in force and is reported as such, so a bad offer
cannot strand the fleet. Three fields of an offer are **ignored** — `certificate`, `tls`, and
`proxy` — and the offer is still acknowledged as applied, which is the missing mutual-TLS support
showing through; see [`docs/CONFORMANCE.md`](../CONFORMANCE.md).

## Troubleshooting

**The Client will not start.** Configuration errors are deliberately fatal and name the key.
The ones you are most likely to meet:

| Message about | What to do |
|---|---|
| `accepts_packages` / `package` in a `[[supervisor]]` block | Both keys were removed. Delete them; every Agent accepts packages, and the Server's Selector decides which artifact. See [`CHANGELOG.md`](../../CHANGELOG.md) for the per-host migration. |
| a program that is not a bare file name | `binary`/`command` name a file in the Supervisor's own `program/` directory, never a path. See [How a block names its program](#how-a-block-names-its-program). |
| an unknown key | Every key is checked; a typo is refused rather than ignored. |
| `[auth]` | Exactly one scheme — a bearer token, or a username *and* a password. |

**A Foreign Agent runs but never gets new configuration.** Its command line is pointing somewhere
the Client does not write. Use `${config_dir}` (see [Path placeholders](#path-placeholders)) and
check that the file name matches the *Configuration's* name on the Server.

**An Agent shows as connected but out of sync.** Look at `remote_config_status` and
`remote_config_error` on its row in `GET /api/v1/agents` — the Client reports why it rejected a
configuration.

**An Agent is offered no package.** Two equally specific Selectors are reaching it;
`package_conflict` on its fleet row names the problem.

**Everything re-registered as new Agents after a move.** `supervisor_dir` changed, and the
`instance-uid` files stayed behind in the old tree. That is expected; nothing migrates
automatically.
