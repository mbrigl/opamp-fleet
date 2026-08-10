# The Client

[← User Manual](README.md) · [← The Server](server.md)

The Client is what runs on a managed host: one process, installed as a native operating-system
service on Linux, macOS, and Windows, that supervises local processes, applies the configuration the
Server sends them, reports back what they are doing, and can replace their binaries — and its own.

- [What the Client does](#what-the-client-does)
- [Running it](#running-it)
- [Running it as an OS service](#running-it-as-an-os-service)
- [Configuration reference](#configuration-reference)
- [Gateway Mode: carrying other Clients](#gateway-mode-carrying-other-clients)
- [Supervisors: putting a process under management](#supervisors-putting-a-process-under-management)
- [Which programs take updates](#which-programs-take-updates)
- [Agents that are more than one file](#agents-that-are-more-than-one-file)
- [Path placeholders](#path-placeholders)
- [Where things live on disk](#where-things-live-on-disk)
- [Package updates](#package-updates)
- [Updating the Client itself](#updating-the-client-itself)
- [Connecting to the Server](#connecting-to-the-server)
- [Troubleshooting](#troubleshooting)

## What the Client does

- **Presents one or more Agents to the Server.** The Client is always its own Agent, whether or not
  it supervises anything, so the Server can see which version each host runs. Each configured
  Supervisor is an additional Agent. All of them share one connection.
- **Supervises processes** (ADR-0011): starts them, watches them, restarts them on a configuration
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

## Running it

```console
$ opamp-fleet-client --config /etc/opamp/client.toml     # foreground; `run` is implied
$ opamp-fleet-client run --config /etc/opamp/client.toml # the same thing, said explicitly
$ opamp-fleet-client --version
```

| Global flag | Meaning |
|---|---|
| `--config <path>` | The TOML configuration file. Defaults to `client.toml`; defaults apply if it does not exist. `service install` is the one place where "not given" means something else: there the file is `<root>/client.toml` inside the install root, because a path resolved against this shell's working directory is not one the service manager shares. |
| `--instance <name>` | Selects the service identity (`opamp-fleet-client-<instance>`) and the default install root, so several differently-configured Clients coexist on one host. Defaults to `default`, whose service is plain `opamp-fleet-client`. Same name grammar as everything else: 1–32 lowercase letters, digits, and `-`. |
| `--state-dir <dir>` | Overrides the configuration file's `state_dir`. `service install` bakes this into the unit, so an installed service never depends on a relative path. |

There are no environment-variable fallbacks for configuration (ADR-0008) — the flags say only where
the file is and which instance is meant. Logging goes to stderr and is controlled by `RUST_LOG`
(default `info`).

The Client stops on `SIGTERM`/`Ctrl-C` (an SCM stop control on Windows) and sends the OpAMP
`agent_disconnect` goodbye before it goes, so the fleet view shows it as deliberately gone rather
than as a host that fell off the network.

## Running it as an OS service

The Client registers *itself* with systemd, launchd, or the Windows SCM (ADR-0010) — there is no
packaging step and no unit file to write:

```console
$ opamp-fleet-client service install --config /etc/opamp/client.toml   # root / Administrator
$ opamp-fleet-client service start
$ opamp-fleet-client service status
$ opamp-fleet-client service stop
$ opamp-fleet-client service uninstall      # deregisters; never deletes the install layout or state
```

| Flag | Applies to | Meaning |
|---|---|---|
| `--user` | every `service` action | Target the current user's service manager instead of the system one. Useful in development; the default is a system service that starts at boot. |
| `--root <dir>` | `service install` | The install root. Defaults to the platform's data directory for the scope and instance — Linux `/var/lib/opamp-fleet/client/<instance>`, macOS `/Library/Application Support/opamp-fleet/client/<instance>`, Windows `%ProgramData%\opamp-fleet\client\<instance>`. No path is ever fixed. |
| `--interactive` | `service install` | Ask for the settings a fresh host cannot guess and write the configuration file before registering the service (ADR-0027). See below. |
| `--endpoint <url>` | `service install` | Write the configuration file with this endpoint instead of asking for it (ADR-0046) — the same file, from an answer given rather than typed at a prompt. Mutually exclusive with `--interactive`, and it keeps an existing file just as `--interactive` does. Takes no credential on purpose: a flag stands in the shell history and the process list. |

### The first configuration, on a host that has none

A release artifact is the bare binary, so a freshly downloaded Client has no `client.toml` to edit.
Without one it still installs and starts — on the development defaults, dialling `127.0.0.1` and
managing nothing. `--interactive` is the way past that:

```console
$ opamp-fleet-client service install --interactive        # root / Administrator
No configuration at /var/lib/opamp-fleet/client/default/client.toml — answering these writes it …
Server OpAMP endpoint [ws://127.0.0.1:4320/v1/opamp]: wss://fleet.example.com/v1/opamp
This Agent's name (service.instance.name) [opamp-fleet-client]: host-01
Authentication toward the Server: bearer token
Bearer token: ********
Does the Server present a certificate from a private CA? [y/N]: n
Allow the Server to update this Client's own binary? [y/N]: n
wrote /var/lib/opamp-fleet/client/default/client.toml
installed opamp-fleet-client
```

What it asks about is only what has no useful default here: the endpoint, the Agent's name, the
credential ([`[auth]`](#auth)), a private CA when the endpoint is `wss://` or `https://`
([`[tls]`](#tls)), and last — defaulting to **no** — consent for the Server to replace this Client's
own binary ([`[self_update]`](#self_update)). Everything else is written into the file as commented
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
  `<root>/client.toml` inside the install root — the same per-platform, per-instance location the
  versions and the state directory already use. The file is validated by the ordinary loader before
  the service is registered; a file that does not parse fails the install and stays on disk for you
  to correct.

Where there is an answer but no terminal — a provisioning run, an MSI dialog, a `%post` script —
`--endpoint` writes the same file without asking:

```console
$ opamp-fleet-client service install --endpoint wss://fleet.example.com/v1/opamp
wrote /var/lib/opamp-fleet/client/default/client.toml for wss://fleet.example.com/v1/opamp
installed opamp-fleet-client
```

It writes only the endpoint; everything else keeps its default, and the credential goes into the
file afterwards. All four rules above hold unchanged — in particular, an existing file is kept.

### Installing from a native package

A release also ships a `.deb`, an `.rpm` and an `.msi` (ADR-0046). They deliver the binary to
`/usr/bin/opamp-fleet-client` (Windows: the folder you choose) and then run `service install`
themselves — the layout, the unit and the SCM entry are the same ones this page describes, because
they are made by the same command. No package ships a unit file of its own.

```console
$ sudo apt install ./opamp-fleet-client_1.2.3_linux_amd64.deb
$ sudo dnf install ./opamp-fleet-client_1.2.3_linux_amd64.rpm
```

**The service is registered and left stopped.** That is deliberate: a Client with no configuration
would dial the development default and manage nothing, and a package must not manufacture that state
on every host it touches. Two steps remain, and the post-install prints them:

```console
$ sudo opamp-fleet-client service install --endpoint wss://fleet.example.com/v1/opamp
$ sudo systemctl start opamp-fleet-client
```

On Windows the `.msi` asks for the installation folder and the endpoint. The folder is the install
root — the `.exe`, `client.toml`, `versions/`, `current` and `state/` all live under it. The same
file installs unattended with the same two answers, which is how Intune, Group Policy and SCCM
deploy it:

```console
C:\> msiexec /i opamp-fleet-client_1.2.3_windows_amd64.msi /qn ^
       INSTALLFOLDER="C:\Program Files\OpAMP Fleet Client" ^
       ENDPOINT="wss://fleet.example.com/v1/opamp"
```

Two things to know about living with a packaged install:

- **`dpkg -l` reports the version it *delivered*, not the one that is running.** After a fleet
  self-update ([Updating the Client itself](#updating-the-client-itself)) the service runs the binary
  under `<root>/current/`, which no package manager owns — that separation is what keeps the next
  `apt upgrade` from silently reverting the Server's decision. `opamp-fleet-client --version` and the
  fleet view are the truth.
- **Removing the package stops and unregisters the service, and deletes nothing else.** The install
  root, the state directory and `client.toml` stay, for the same reason an install never overwrites
  a configuration: it may hold a credential you typed.

macOS has no native installer; there, unpack the `.7z` and run `service install` yourself.

### What the service is called

One name on every platform (ADR-0030) — the default instance is the product's name, and any other
instance appends its own:

| | `--instance default` | `--instance prod` |
|---|---|---|
| **Linux** (systemd) | `opamp-fleet-client.service` | `opamp-fleet-client-prod.service` |
| **macOS** (launchd) | `opamp-fleet-client` (job and plist) | `opamp-fleet-client-prod` |
| **Windows** (SCM) | `opamp-fleet-client` | `opamp-fleet-client-prod` |

So the same command works everywhere it exists: `systemctl status opamp-fleet-client`,
`launchctl list opamp-fleet-client`, `sc query opamp-fleet-client`.

Where a platform has a second, human-readable name, it is **OpAMP Fleet Client** (`OpAMP Fleet
Client (prod)` for a named instance). That is the Windows services list; systemd shows the unit name
as its `Description`, and a launchd job has no name besides its label.

### Where the service's logs are

A Client started by the service manager writes its own log to **`<state_dir>/logs/`** on every
platform (ADR-0041), one file per day, seven days kept:

```
<state_dir>/logs/opamp-fleet-client.2026-08-09.log
```

**On Windows this is the only copy there is** — the SCM discards a service's stderr, so `sc query`
telling you the service will not start is all the platform itself offers. On Linux and macOS the
same lines are also in `journalctl -u opamp-fleet-client` and Console/`log show`; the file is
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

This is a different thing from the Client's own telemetry (`ReportsOwnLogs`, ADR-0036), which ships
log records to a destination the **Server** offers. That needs a Server it can already reach — which
is exactly what a bad `client.toml`, an unusable certificate, or a refused endpoint does not give
it. The file on disk is what explains those.

The Windows services list has a **Description** column beside that name, and it is a separate field
that nothing fills on its own — a service can carry a display name and still show an empty
description, which is what this one did. It now reads **OpAMP Fleet Client for Windows**, the same
on every instance: the display name beside it is what says *which* Client this is.

Both are set right after the registration, with `sc.exe config` and `sc.exe description`.

The root holds versioned installs side by side, a `current` pointer the service is registered
against, and the default state directory:

```
<root>/versions/opamp-fleet-client-<version>-<commit>/opamp-fleet-client   # every version
<root>/current -> versions/opamp-fleet-client-…/    # symlink (Unix), junction (Windows)
<root>/state/                                            # the default state_dir
```

Because the service runs `<root>/current/opamp-fleet-client`, switching versions never re-registers
the service.

After a crash the service manager restarts the service; after an explicit stop it stays down. Known
platform gaps, tracked in ADR-0010: on macOS `service status` is advisory and `install` does not
auto-start, and on Windows the SCM discards stderr, so service logs are lost until logging to a file
lands.

### Windows needs an elevated shell, and says so before it writes

Registering a machine-wide service needs Administrator, and a running process cannot raise its own
rights — there is no UAC prompt to be had from inside a command that has already started. So
`service install` asks the service control manager up front whether this process may register a
service at all, and stops with a message naming the fix if it may not:

```console
C:\> opamp-fleet-client service install
the Windows service control manager denied access: registering a machine-wide service needs
Administrator, and a running process cannot raise its own rights. Open a shell with "Run as
administrator" — from PowerShell, `Start-Process powershell -Verb RunAs` — and run this command
again. Nothing has been installed or written.
```

That the check comes *before* the first write is the point of it: `%ProgramData%` lets an ordinary
user create folders, so an install refused only at `sc create` had already staged a version directory
and pointed `current` at it, leaving half an install behind. `uninstall`, `start`, and `stop` write
nothing beforehand and simply report the manager's own refusal.

## Configuration reference

The full annotated example is [`config/client.toml`](../../config/client.toml). Every key is
optional and shown below with its default; an unknown key fails startup rather than being ignored.

### Top level

| Key | Default | Meaning |
|---|---|---|
| `endpoint` | `"ws://127.0.0.1:4320/v1/opamp"` | The Server's OpAMP endpoint. The scheme selects the transport (ADR-0007): `ws://`/`wss://` for WebSocket, `http://`/`https://` for polling. |
| `name` | `"opamp-fleet-client"` | The Agent's `service.instance.name` — your name for *this* Client, shown in the fleet view and matchable by a Selector. Its `service.name` is the constant type `opamp-fleet-client`, the same on every host (ADR-0033). |
| `poll_interval_secs` | `30` | How often the plain-HTTP transport polls. Ignored on WebSocket. |
| `heartbeat_interval_secs` | `30` | Heartbeat interval on WebSocket. `0` disables heartbeats and undeclares the capability; on plain HTTP every poll already is the periodic report. |
| `max_message_size_bytes` | `67108864` (64 MiB) | The largest OpAMP message sent or accepted, in either direction — including on the Supervisor Endpoint. A message past it is never sent, and an oversized one from the Server is refused. |
| `state_dir` | `"client-state"` | Where the Client persists its own Agent's identity, its remote configuration, and any Server-offered connection settings. A self-update artifact is streamed here first, so it needs room for one agent binary. |
| `supervisor_dir` | `<state_dir>/supervisors` | Where the per-Supervisor directories live (ADR-0021). Set it when the programs must not live where the state does — `/var/lib` is often mounted `noexec`, and sized for state rather than for a few hundred megabytes of Collector. |

> **Moving `supervisor_dir` on a running host leaves the old tree behind**, `instance-uid` included,
> so every Supervisor re-registers as a **new** Agent on the Server. Nothing migrates automatically.

### `[attributes]`

Operator-defined attributes reported by every Agent this Client presents, so the Server's Selectors
can target them (ADR-0012). They are reported as non-identifying attributes, and they never override
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

The first two are the pair to keep apart (ADR-0033): `service.name` is *what* this Agent is — the
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
private CA (ADR-0007). Without it the platform's trust store applies.

`cert_file` and `key_file` are this Client's own certificate for a Server that requires mutual TLS
(ADR-0035) — they go together or not at all. This is the identity an operator provisions, including
the **bootstrap certificate** a fresh host enrols with. A certificate the Server issued outranks it:
the Client stores that pair in its state directory as `client-cert.pem` and `client-key.pem` and
prefers it, the same precedence persisted connection settings have over `client.toml`. Deleting the
stored pair reverts to what is written here.

**Enrolment needs nothing in this file.** When the Server declares that it signs certificates, a
Client without one generates a key — which never leaves the host — sends a signing request, and
receives a certificate through the ordinary offer flow, renewing the same way once it is two thirds
through its validity. The private key is written `0600`; on Windows the state directory's ACL is
what protects it.

### `[auth]`

Exactly one scheme: `bearer_token`, **or** `username` and `password` together. Mixing them, or
giving half of one, fails at startup (ADR-0013).

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
| `archive_key` | Opens an encrypted `.7z` artifact (ADR-0018). One secret for the fleet — a single archive serves every Agent — and never the `[auth]` credential, which the Server may rotate on its own: a rotation would leave every archive unopenable. The Server never learns this key; the artifact stays encrypted wherever it is stored and is opened only on the host that runs it. |

Note what is *not* here: which artifact a Supervisor receives is the Server's decision, expressed as
the package's Agent type (ADR-0034) and its Selector (ADR-0017), never a key in this file.

### `[self_update]`

```toml
[self_update]
package = "opamp-fleet-client"
```

See [Updating the Client itself](#updating-the-client-itself). Absent — the default — the Client's
own Agent declares no package capability at all and no offer can reach it.

## Gateway Mode: carrying other Clients

A Client can stand at a network boundary and carry other Clients' Agents upstream over a small pool
of connections (ADR-0037) — for a segmented network the Server cannot reach into, or simply for a
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
  staleness flag (ADR-0038): the connection stays up, because it is the Gateway's, and the row reads
  **Connected + Stale**. It needs a heartbeat configured on the Agent to work, since staleness only
  applies to Agents that promised to report periodically.
- **It does not carry a downstream client certificate upstream.** Mutual TLS is per hop (ADR-0035):
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

Two plugin types ship today (ADR-0011): `collector` for an OpenTelemetry Collector, and `command`
for any other process — a **Foreign Agent** that speaks no OpAMP. A new kind of process means a new
plugin, not a change to the core.

### Keys every block accepts

| Key | Default | Meaning |
|---|---|---|
| `type` | — | `"collector"` or `"command"`. Required. |
| `name` | — | This Agent's `service.instance.name` — your name for it — and the directory name it owns. Required; 1–32 lowercase letters, digits, and `-`. Must be unique in the file. A Managed Process can never overwrite it. |
| `service_name` | the program's file name | This Agent's `service.name`: the Agent **type** it presents (ADR-0033). A Managed Process that reports a type of its own wins over it — a Collector with the `opampextension` states the `dist.name` it was built with — so set it for a process that reports nothing. Unlike `name` it may be a reverse FQDN, as the protocol recommends; only an empty value is refused. |
| `endpoint_port` | `0` (ephemeral) | The port of the Supervisor Endpoint on `127.0.0.1`. The endpoint always comes up; pin the port when something is meant to connect to it. |
| `stop_timeout_secs` | `10` | Graceful-stop budget before the process is killed. |
| `apply_grace_secs` | `3` | How long a restarted process must survive before a received configuration is acknowledged `APPLIED`. `0` acknowledges on start. |
| `program_path` | unset | Where the program sits *inside* a package that is a whole directory tree (ADR-0023), e.g. `bin/fluent-bit`. Unset means the package is a single file. See [Agents that are more than one file](#agents-that-are-more-than-one-file). |
| `[supervisor.attributes]` | none | Attributes for this Agent alone, overriding the Client's `[attributes]` per key. |

Two keys were **removed** and now fail at startup with a message saying what to do instead:
`package` (the Server aims packages by Selector now, ADR-0017) and `accepts_packages` (the program's
path decides, ADR-0021).

### `type = "collector"`

| Key | Meaning |
|---|---|
| `binary` | The Collector program. See [Which programs take updates](#which-programs-take-updates). |
| `args` | Extra arguments, appended **after** the `--config` flags the Supervisor builds. |

The Supervisor writes every received config-map entry into its own `config/` directory under the
Configuration's name and passes each **unroled** entry as its own `--config`; a `supplementary`
entry is written but never passed (ADR-0016). A change restarts the Collector so it re-reads them.
The version is probed once with `--version`, so even a Collector without the extension reports one.

A Collector **with** the `opampextension` reports its own description, health, and effective
configuration through the Supervisor Endpoint instead of being watched from outside. The extension
ships only in the Contrib distribution. Pin `endpoint_port` and make sure the configuration the
Server distributes carries the extension pointing at it:

```toml
[[supervisor]]
type = "collector"
name = "otelcol"
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
ephemeral port, and nothing ever connects to it.

```toml
[[supervisor]]
type = "collector"
name = "otelcol-plain"
binary = "/usr/local/bin/otelcol"
```

### `type = "command"`

For a Foreign Agent — anything that speaks no OpAMP and is brought into the fleet by translating its
lifecycle into the protocol.

| Key | Meaning |
|---|---|
| `command` | The program. See [Which programs take updates](#which-programs-take-updates). |
| `args` | Its arguments, verbatim — apart from [placeholder expansion](#path-placeholders). |
| `working_dir` | The directory to start in. Optional. |
| `[supervisor.env]` | Additional environment for the process. |
| `version_args` | Arguments that make the program print its version, e.g. `["--version"]`. The program is invoked once with exactly these, and the first SemVer 2.0.0 version in its output becomes the Agent's `service.version`. Opt-in, because a Foreign Agent's version flag is its own convention. |

The Supervisor writes the received configuration entries the same way a Collector's does, but it
cannot know what to do with them — a Foreign Agent reads its own configuration file. So you point it
at the written entry with its own flag, and the Supervisor restarts the process on a change so it
re-reads it:

```toml
[[supervisor]]
type = "command"
name = "fluent-bit"
command = "/opt/fluent-bit/bin/fluent-bit"
args = ["-c", "${config_dir}/fluent-bit-conf"]
working_dir = "/var/lib/fluent-bit"
version_args = ["--version"]
[supervisor.attributes]
role = "edge"
[supervisor.env]
FLB_LOG_LEVEL = "info"
```

Here `fluent-bit-conf` is the *name of the Configuration on the Server* — that is what the entry file
is called.

## Which programs take updates

How a block names its program is also what decides whether the Server may replace it (ADR-0021),
because replacing a program means writing in the directory it sits in. The same rule applies to
`binary` and `command` alike:

| What you write | What it means |
|---|---|
| a **bare file name** — `otelcol-contrib` | The program lives in `<supervisor_dir>/<name>/program/`, a directory this Client creates and owns. **It takes package updates.** |
| an **absolute path** — `/usr/local/bin/otelcol` | The machine's program, put there by a distribution package or configuration management. It is started and supervised, never written to. |
| anything else — `./x`, `bin/x` | A startup error, rather than a guess. |

Two things to know about a bare name: it is **not** searched for in `$PATH` — it names a file in that
one directory, and you put the first copy there yourself; every later one arrives by package. And on
Windows "absolute" means the path names a **drive**: `\Program Files\otelcol\otelcol.exe` carries a
root but no drive, so it resolves against whichever drive the process happens to be on. It is
refused at startup with a message saying what is missing. Write `C:\Program Files\otelcol\otelcol.exe`.

The startup log states, per Supervisor, which program it resolved to and whether packages are
accepted.

## Path placeholders

A Foreign Agent is told where its configuration is *through its own command line*, and an absolute
path written there drifts the moment `supervisor_dir` moves or the Supervisor is renamed —
silently, because the process then starts happily on a file nobody writes to. Two placeholders
(ADR-0022) close that, in a `command` Supervisor's `args`, `working_dir`, and `[supervisor.env]`
values:

| Placeholder | Expands to |
|---|---|
| `${supervisor_dir}` | `<supervisor_dir>/<name>` — everything this Supervisor owns |
| `${config_dir}` | `<supervisor_dir>/<name>/config` — where the received configuration's entries are written |

Three rules go with them:

- **Any other `${…}` is passed to the process untouched.** A Foreign Agent's own configuration
  language may use the same syntax — Fluent Bit's does — and a Client that ate or refused those
  would break a working deployment to catch a typo. The flip side is that a *misspelled* placeholder
  (`${config-dir}`) is handed over rather than refused, unlike an unknown TOML key.
- **The program itself is never substituted**, in `binary` or `command`. Its written form is what
  decides package consent (see above), and that must be readable in the file.
- **Expansion happens once, at startup.** None of these paths change while the Client runs.

## Where things live on disk

The Client's own Agent keeps its state in `state_dir`:

```
<state_dir>/instance-uid              # this Client's Agent identity
<state_dir>/remote-config.pb          # the last configuration it received
<state_dir>/connection-settings.pb    # Server-offered settings, if any
<state_dir>/installed-package.json    # the self-update it last installed, if any
<state_dir>/packages/                 # staging for a self-update artifact
<state_dir>/logs/                     # the service's own rotating log (ADR-0041)
```

Each Supervisor owns everything under its own directory (ADR-0021):

```
<supervisor_dir>/<name>/instance-uid      # this Agent's identity
<supervisor_dir>/<name>/remote-config.pb  # the last configuration it received
<supervisor_dir>/<name>/config/           # one entry file per matching Configuration
<supervisor_dir>/<name>/program/          # the program, when it is named by a bare file name
<supervisor_dir>/<name>/program/tree/     # …or the whole unpacked package (ADR-0023)
<supervisor_dir>/<name>/packages/         # staging its package downloads go through
```

**Persisted connection settings override the file.** `<state_dir>/connection-settings.pb` takes
precedence over `endpoint`, `[auth]`, and the intervals in `client.toml`. Delete it to revert to
what the file says.

## Package updates

A Supervisor whose program is its own (a bare file name) declares that it accepts packages, and the
Server offers it a package built for the Agent type it reports and aimed at it by that package's
Selector (ADR-0034, ADR-0017) — so a Supervisor reporting `promtail` is never handed the Collector's
binary, whatever anyone forgot to aim. What then happens on the host:

1. The artifact is **streamed to disk** in that Supervisor's `packages/` directory — never held in
   memory.
2. It is **verified**: the content hash always, and the Ed25519 signature whenever
   `[packages] verification_key` is set.
3. It is **unpacked** when it is a `.tar.gz` or a `.7z` (ADR-0018) — the member whose file name
   matches the configured program, so an upstream release can be uploaded exactly as published. An
   encrypted `.7z` opens with `[packages] archive_key`. A bare program artifact is moved into place
   rather than copied.
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

## Agents that are more than one file

An executable plus the shared objects it loads — Fluent Bit is the usual example — is delivered by
naming where the program sits inside the package (ADR-0023):

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
tree**; a `.7z` is opened too, but 7z stores Windows attributes, so only the program is made
executable and a helper binary beside it would not be. And **`program_path` and an absolute
`binary`/`command` are refused together** — the machine's program is not something this Client
unpacks into.

## Updating the Client itself

The Client is always its own Agent, so the Server can always see which version a host runs. Letting
the Server *replace* that version is opt-in, and the opt-in **names the package**:

```toml
[self_update]
package = "opamp-fleet-client"
```

An offer under any other name is refused and reported, never applied. That is one of two independent
guards, and it is the one on this side of the wire: the Server will not offer a package built for
another Agent type either (ADR-0034), and this Client's type is the constant `opamp-fleet-client`.
Neither replaces the other — an operator who types a Collector artifact as `opamp-fleet-client` gets
past the Server, and this name is what is left.

On an accepted offer the artifact is verified like any other, staged as a new version *beside* the
running one in the install layout, and proved by running `opamp-fleet-client self-check` on it before the
`current` pointer moves — which asks two things at once: does this binary run at all on this host,
and is it actually an OpAMP Fleet Client at the version offered. The process then exits and asks the
service manager to restart it; it does not restart itself. A marker in `state_dir` carries the
outcome across that restart: the new version commits itself once it reaches the Server, and one that
will not stay up is rolled back to its predecessor after three attempts. Either outcome is reported
to the Server by whichever version came up.

Self-update therefore requires an installed service — it is the service manager that performs the
restart.

**Replacing the binary by hand does not fool it.** What the Client installed is recorded in
`<state_dir>/installed-package.json`, and `service uninstall` deletes neither the install layout nor
the state — so installing an older Client afterwards comes up on top of that record. A record that
does not name the version the running binary *is* is discarded at startup, with a warning naming
both versions. The Client then reports no package, the Server offers the published one again, and
the host is updated back to it. To hold a host on an older Client, retract the package on the Server
first ([`publication`](server.md#packages-distributing-software)) — retracting withdraws the offer
and uninstalls nothing.

**Where the artifact comes from.** Every release publishes one archive per platform, named
`opamp-fleet-client_<version>_<os>_<arch>.7z`
([ADR-0025](../adr/0025-release-pipeline-and-artifacts.md),
[ADR-0032](../adr/0032-release-artifacts-separate-their-fields-with-underscores.md)) — and that file
*is* a package artifact: it holds the Client under the name the install layout gives it, so it is
uploaded exactly as downloaded, and the SHA-256 the release published is the one the Agent verifies.
Nothing repacks it. The fields are separated by `_` because two of them carry `-` — the name
(`opamp-fleet-client`) and a prerelease version (`1.2.3-dev`) — so the last two fields are the
platform and can be read off the name.

**The version in the query is the release number** — the one in the file name
([ADR-0029](../adr/0029-a-version-is-compared-and-shown-without-its-build-metadata.md)) — and the
platform is required with it (ADR-0031), spelled as the Agent reports it:

```console
$ curl -X PUT --data-binary @opamp-fleet-client_1.2.3_linux_amd64.7z \
       "http://<server>:4320/api/v1/packages/opamp-fleet-client?version=1.2.3&os=linux&arch=amd64"
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "opamp-fleet-client"}' \
       http://<server>:4320/api/v1/packages/opamp-fleet-client/type
```

The second call is what arms the package (ADR-0034): until a type is set it is offered to nobody, so
an artifact uploaded and left untyped reaches no Client at all. For this one the type is the
Client's own, `opamp-fleet-client`.

The staged binary's `self-check` compares that against what it reports, ignoring the commit the
build came from — `1.2.3` and `1.2.3+a1b2c3d` are the same release, and the content hash is what
pins *which* bytes arrived. What is **not** ignored is the pre-release: a `1.2.3-dev` build offered
as `1.2.3` is refused, because a build heading for a release is not that release.

Two things follow. Passing the full string still works, but if you do, remember that a `+` in a URL
query is decoded as a *space* — it has to be written `%2B`, which is the reason the release number
is the better thing to type. And `opamp-fleet-client --version` prints the full string on any host,
which is what to quote when asking which build a host runs.

## Connecting to the Server

**Choosing a transport.** `ws://`/`wss://` gets configuration changes pushed within a second.
`http://`/`https://` polls every `poll_interval_secs`, which is the option for a host that cannot
hold a long-lived connection. Nothing else differs: both carry the same messages, and the Server
accepts both at once.

**Reconnecting.** A dropped connection is retried with capped exponential backoff, and the Client
honours the Server's `UNAVAILABLE` retry hints.

**Server-offered connection settings** (ADR-0014). When the Server offers a new credential,
heartbeat interval, or endpoint, the Client **verifies the offer by actually connecting with it**,
persists it, and only then switches — across transports if the offered endpoint demands it. A
failed verification leaves the current settings in force and is reported as such, so a bad offer
cannot strand the fleet. Three fields of an offer are **ignored** — `certificate`, `tls`, and
`proxy` — and the offer is still acknowledged as applied, which is the missing mutual-TLS support
showing through; see [`docs/CONFORMANCE.md`](../CONFORMANCE.md).

## Troubleshooting

**The Client will not start.** Configuration errors are deliberately fatal and name the key (ADR-0008).
The ones you are most likely to meet:

| Message about | What to do |
|---|---|
| `accepts_packages` / `package` in a `[[supervisor]]` block | Both keys were removed. Delete them; the program's path decides package consent now, and the Server's Selector decides which artifact. See [`CHANGELOG.md`](../../CHANGELOG.md) for the per-host migration. |
| a program that is "neither" | The path is neither a bare file name nor absolute. Pick one — see [Which programs take updates](#which-programs-take-updates). |
| a program relative to the current drive (Windows) | Write the drive: `C:\…`. |
| an unknown key | Every key is checked; a typo is refused rather than ignored. |
| `[auth]` | Exactly one scheme — a bearer token, or a username *and* a password. |

**A Foreign Agent runs but never gets new configuration.** Its command line is pointing somewhere
the Client does not write. Use `${config_dir}` (see [Path placeholders](#path-placeholders)) and
check that the file name matches the *Configuration's* name on the Server.

**An Agent shows as connected but out of sync.** Look at `remote_config_status` and
`remote_config_error` on its row in `GET /api/v1/agents` — the Client reports why it rejected a
configuration.

**A Supervisor accepts no packages although you expected it to.** Its program is named by an
absolute path. The startup log states, per Supervisor, what it resolved and what it decided.

**An Agent that accepts packages is offered none.** Two equally specific Selectors are reaching it;
`package_conflict` on its fleet row names the problem (ADR-0017).

**Everything re-registered as new Agents after a move.** `supervisor_dir` changed, and the
`instance-uid` files stayed behind in the old tree. That is expected; nothing migrates
automatically.
