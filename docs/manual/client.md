# The Client

[← User Manual](README.md) · [← The Server](server.md)

The Client is what runs on a managed host: one process, installed as a native operating-system
service on Linux, macOS, and Windows, that supervises local processes, applies the configuration the
Server sends them, reports back what they are doing, and can replace their binaries — and its own.

- [What the Client does](#what-the-client-does)
- [Running it](#running-it)
- [Running it as an OS service](#running-it-as-an-os-service)
- [Configuration reference](#configuration-reference)
- [Supervisors: putting a process under management](#supervisors-putting-a-process-under-management)
- [Which programs take updates](#which-programs-take-updates)
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
$ client --config /etc/opamp/client.toml     # foreground; `run` is implied
$ client run --config /etc/opamp/client.toml # the same thing, said explicitly
$ client --version
```

| Global flag | Meaning |
|---|---|
| `--config <path>` | The TOML configuration file. Defaults to `client.toml`; defaults apply if it does not exist. |
| `--instance <name>` | Selects the service identity (`io.opamp-fleet.client.<instance>`) and the default install root, so several differently-configured Clients coexist on one host. Defaults to `default`. Same name grammar as everything else: 1–32 lowercase letters, digits, and `-`. |
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
$ client service install --config /etc/opamp/client.toml   # root / Administrator
$ client service start
$ client service status
$ client service stop
$ client service uninstall      # deregisters; never deletes the install layout or state
```

| Flag | Applies to | Meaning |
|---|---|---|
| `--user` | every `service` action | Target the current user's service manager instead of the system one. Useful in development; the default is a system service that starts at boot. |
| `--root <dir>` | `service install` | The install root. Defaults to the platform's data directory for the scope and instance — Linux `/var/lib/opamp-fleet/client/<instance>`, macOS `/Library/Application Support/opamp-fleet/client/<instance>`, Windows `%ProgramData%\opamp-fleet\client\<instance>`. No path is ever fixed. |

The root holds versioned installs side by side, a `current` pointer the service is registered
against, and the default state directory:

```
<root>/versions/opamp-client-<version>-<commit>/client   # every installed version
<root>/current -> versions/opamp-client-…/               # symlink (Unix), junction (Windows)
<root>/state/                                            # the default state_dir
```

Because the service runs `<root>/current/client`, switching versions never re-registers the service.

After a crash the service manager restarts the service; after an explicit stop it stays down. Known
platform gaps, tracked in ADR-0010: on macOS `service status` is advisory and `install` does not
auto-start, and on Windows the SCM discards stderr, so service logs are lost until logging to a file
lands.

## Configuration reference

The full annotated example is [`config/client.toml`](../../config/client.toml). Every key is
optional and shown below with its default; an unknown key fails startup rather than being ignored.

### Top level

| Key | Default | Meaning |
|---|---|---|
| `endpoint` | `"ws://127.0.0.1:4320/v1/opamp"` | The Server's OpAMP endpoint. The scheme selects the transport (ADR-0007): `ws://`/`wss://` for WebSocket, `http://`/`https://` for polling. |
| `name` | `"opamp-fleet-client"` | The Agent's `service.name` — its human identity in the fleet view. |
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

Every Agent additionally reports, without configuration: `service.name`, `service.instance.id`,
`os.type`, `host.arch`, and `os.description` — plus `service.version`, which for the Client's own
Agent is the Client's baked-in version and for a Supervisor-backed Agent is whatever the Managed
Process reports about itself.

### `[tls]`

```toml
[tls]
ca_file = "ca.pem"
```

A trust override for `wss://`/`https://` endpoints whose certificate comes from a private CA
(ADR-0007). Without it the platform's trust store applies. The Client presents **no** client
certificate — mutual TLS is not built.

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
that package's Selector (ADR-0017), never a key in this file.

### `[self_update]`

```toml
[self_update]
package = "opamp-client"
```

See [Updating the Client itself](#updating-the-client-itself). Absent — the default — the Client's
own Agent declares no package capability at all and no offer can reach it.

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
| `name` | — | This Agent's `service.name`, and the directory name it owns. Required; 1–32 lowercase letters, digits, and `-`. Must be unique in the file. |
| `endpoint_port` | `0` (ephemeral) | The port of the Supervisor Endpoint on `127.0.0.1`. The endpoint always comes up; pin the port when something is meant to connect to it. |
| `stop_timeout_secs` | `10` | Graceful-stop budget before the process is killed. |
| `apply_grace_secs` | `3` | How long a restarted process must survive before a received configuration is acknowledged `APPLIED`. `0` acknowledges on start. |
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
<state_dir>/packages/                 # staging for a self-update artifact
```

Each Supervisor owns everything under its own directory (ADR-0021):

```
<supervisor_dir>/<name>/instance-uid      # this Agent's identity
<supervisor_dir>/<name>/remote-config.pb  # the last configuration it received
<supervisor_dir>/<name>/config/           # one entry file per matching Configuration
<supervisor_dir>/<name>/program/          # the program, when it is named by a bare file name
<supervisor_dir>/<name>/packages/         # staging its package downloads go through
```

**Persisted connection settings override the file.** `<state_dir>/connection-settings.pb` takes
precedence over `endpoint`, `[auth]`, and the intervals in `client.toml`. Delete it to revert to
what the file says.

## Package updates

A Supervisor whose program is its own (a bare file name) declares that it accepts packages, and the
Server offers it whichever artifact that package's Selector aims at it. What then happens on the
host:

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

Two limits worth knowing before you plan a rollout: a Managed Process that is **more than one file**
— an executable plus the shared objects it loads — cannot be delivered as a package, because exactly
one archive member is installed; name it by an absolute path and update it however it was installed.
And only a **top-level** package is installed: an addon is something a Supervisor has no way to
apply, so it is refused with `InstallFailed` rather than written over the binary it was meant to
extend.

## Updating the Client itself

The Client is always its own Agent, so the Server can always see which version a host runs. Letting
the Server *replace* that version is opt-in, and the opt-in **names the package**:

```toml
[self_update]
package = "opamp-client"
```

That name is the whole of the protection: a package with an empty Selector reaches every consenting
Agent, so without a name to match, the first fleet-wide Collector artifact someone uploads would be
installed over the Client and take the host out of reach. An offer under any other name is refused
and reported, never applied.

On an accepted offer the artifact is verified like any other, staged as a new version *beside* the
running one in the install layout, and proved by running `client self-check` on it before the
`current` pointer moves — which asks two things at once: does this binary run at all on this host,
and is it actually an OpAMP Fleet Client at the version offered. The process then exits and asks the
service manager to restart it; it does not restart itself. A marker in `state_dir` carries the
outcome across that restart: the new version commits itself once it reaches the Server, and one that
will not stay up is rolled back to its predecessor after three attempts. Either outcome is reported
to the Server by whichever version came up.

Self-update therefore requires an installed service — it is the service manager that performs the
restart.

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
