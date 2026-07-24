- [Gateway Mode: carrying other Clients](#gateway-mode-carrying-other-clients)

### Running it under its own account

By default the system service runs as root (systemd, launchd) or `LocalSystem` (Windows).
`--run-as` drops that (ADR-0028): the service — and every Managed Process its Supervisors spawn —
configuration file, the state directory, **and the executable layout**. The layout too because
the self-update runs *inside* the service — a layout the account cannot write would silently end
[server-driven updates](#self_update) for that host.

On Linux and macOS the account must already exist; the install refuses early if it does not:

```console
# useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin opamp-fleet
```

On Windows only **passwordless** account forms are accepted — there is deliberately no password
flag, for the same reason `--endpoint` takes no credential. The recommended form is the service's
own virtual account, which Windows provisions and password-manages by itself:

```console
```

A group-managed service account (`DOMAIN\name$`) and the built-ins `NT AUTHORITY\LocalService` /
policy grants it to `NT SERVICE\ALL SERVICES` (covering the virtual account), the built-ins carry
it inherently, and a gMSA gets it from its domain's group policy. On a host hardened to remove
that default grant, restore the right for the account or the service will not start.

Two consequences to weigh before using it:

- **The account is a trust boundary.** Whoever holds it can replace the binary in the layout, and
  pointer — an administrator invoking the CLI executes account-owned code.
- **The account's limits are the fleet's.** Anything under this Client that needs a port below
  1024 or root-only telemetry sources will fail — that trade-off is the point of the flag.

Re-running `install` with a different `--run-as` re-registers and re-owns the same directories;
without the flag it registers exactly as before — root/`LocalSystem`, no handover.

### The first configuration, on a host that has none

Without one it still installs and starts — on the development defaults, dialling `127.0.0.1` and
managing nothing. `--interactive` is the way past that:

```console
Server OpAMP endpoint [ws://127.0.0.1:4320/v1/opamp]: wss://fleet.example.com/v1/opamp
Authentication toward the Server: bearer token
Bearer token: ********
Does the Server present a certificate from a private CA? [y/N]: n
Allow the Server to update this Client's own binary? [y/N]: n
```

What it asks about is only what has no useful default here: the endpoint, the Agent's name, the
credential ([`[auth]`](#auth)), a private CA when the endpoint is `wss://` or `https://`
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
  versions and the state directory already use. The file is validated by the ordinary loader before
  the service is registered; a file that does not parse fails the install and stays on disk for you
  to correct.
### What the service is called





### Where the service's logs are

A Client started by the service manager writes its own log to **`<state_dir>/logs/`** on every

```
```

**On Windows this is the only copy there is** — the SCM discards a service's stderr, so `sc query`
telling you the service will not start is all the platform itself offers. On Linux and macOS the
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

log records to a destination the **Server** offers. That needs a Server it can already reach — which
it. The file on disk is what explains those.

The Windows services list has a **Description** column beside that name, and it is a separate field
that nothing fills on its own — a service can carry a display name and still show an empty

Both are set right after the registration, with `sc.exe config` and `sc.exe description`.


### Windows needs an elevated shell, and says so before it writes

Registering a machine-wide service needs Administrator, and a running process cannot raise its own
rights — there is no UAC prompt to be had from inside a command that has already started. So
`service install` asks the service control manager up front whether this process may register a
service at all, and stops with a message naming the fix if it may not:

```console
the Windows service control manager denied access: registering a machine-wide service needs
Administrator, and a running process cannot raise its own rights. Open a shell with "Run as
administrator" — from PowerShell, `Start-Process powershell -Verb RunAs` — and run this command
again. Nothing has been installed or written.
```

That the check comes *before* the first write is the point of it: `%ProgramData%` lets an ordinary
user create folders, so an install refused only at `sc create` had already staged a version directory
and pointed `current` at it, leaving half an install behind. `uninstall`, `start`, and `stop` write
nothing beforehand and simply report the manager's own refusal.

These describe the **host**, and every Agent this Client presents carries them — including a fresh
one, in its very first message, before the Server has ever seen it.

To tag **one** Agent among several on a host, use a Server **label** instead
(`PUT /api/v1/agents/<uid>/labels`): it is keyed by that Agent's `instance_uid`, matched by the
same Selectors, and takes effect at once without editing a file on the host. A block's own
`[supervisor.attributes]` table used to do this and no longer exists; a block carrying one fails at
startup with that sentence.
## Gateway Mode: carrying other Clients

A Client can stand at a network boundary and carry other Clients' Agents upstream over a small pool
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

| `stop_timeout_secs` | global `[supervisors]` value | Graceful-stop budget before the process is killed. **Unwrapped kinds only.** |
| `apply_grace_secs` | global `[supervisors]` value | How long a restarted process must survive before a received configuration is acknowledged `APPLIED`. `0` acknowledges on start. **Unwrapped kinds only.** |
| `retain_previous_secs` | global `[updates]` value | How long the version a successful update supersedes is kept before deletion. `0` deletes it on success. **Unwrapped kinds only.** See [Package updates: rollback and retention](#package-updates-rollback-and-retention). |
| `program_path` | unset | Where the program sits *inside* a package that is a whole directory tree, e.g. `bin/fluent-bit`. Unset means the package is a single file. **Unwrapped kinds only** — a wrapper knows its own tree. See [Agents that are more than one file](#agents-that-are-more-than-one-file). |

#### Wrapped and unwrapped kinds


A key a wrapper supplies is **refused by name** rather than silently overridden, and the message
says what supplies the value now. The same check runs on a Supervisor set the Server offers, so a
block that would not work is refused *before* any running process is touched.

This Client reports the kinds it was compiled with as attributes of its own Agent — one key per
Clients that can actually run it.
#### Timing is the fleet's, then the agent's

Three keys describe *time*, and all three are the deployment's policy first:

```toml
[supervisors]
stop_timeout_secs = 10   # graceful-stop budget before the process is killed
apply_grace_secs = 3     # how long a restart must hold before an apply is acknowledged

A **wrapped kind corrects them** where its agent's own behaviour demands it — Icinga 2 needs sixty
seconds to drain its checks and close its cluster connections, which is a property of Icinga and
not of any host. Only a block of an **unwrapped** kind may state its own, because there no kind
exists to hold the value.

#### Keys that were removed

Each fails at startup with a message saying what to do instead, and each is refused the same way in
a set the Server offers:

| Key | What answers it now |
|---|---|
| `package` | the Server aims packages by Selector |
| `accepts_packages` | every Agent's program path decides |
| `working_dir` | the process starts in the directory its program lives in |
| `reload_signal` | a kind that knows the agent holds it; an unwrapped agent applies by restarting |
| `[supervisor.attributes]` | a Server label, keyed by the Agent's `instance_uid` |
| `args` | Extra arguments, appended **after** the `--config` flags the Supervisor builds — with [placeholder expansion](#path-placeholders). |
| `[supervisor.env]` | Additional environment for the Collector process — the natural home for a value the config reads as `${env:VAR}`, e.g. a per-host endpoint. Expanded through the same placeholders. |

The process starts in **the directory its program lives in** — this Supervisor's own `program/`, or
the tree root for a tree package. There is no `working_dir` key: the old default was whatever
directory the service manager left this Client in, usually `/`, which nobody chose.

There is no `reload_signal` key either. Whether a program re-reads its configuration on a signal is
the program's own convention, and a wrong value here is the one that stays invisible — a signal the
process ignores looks exactly like an apply that worked. An agent under this kind therefore applies
a configuration **by restarting**; an agent whose in-place reload is worth having is an agent worth
re-reads it:
close that, in a Supervisor's operator-written strings — a `command`'s `args` and
`[supervisor.env]`, and a `collector`'s `args` and `[supervisor.env]`. A wrapped kind builds its own
paths and needs no placeholder, except in the four Icinga keys, which take them too:
  `retain_previous_secs`: global in `[updates]`, **one day** by default, overridable in a block of
  an unwrapped kind and by a wrapper that has a reason to. `0` deletes on success. Each Supervisor
  keeps at most the immediately previous version.
The staged binary's `self-check` compares that against what it reports, ignoring the commit the
build came from — `1.2.3` and `1.2.3+a1b2c3d` are the same release, and the content hash is what
pins *which* bytes arrived. What is **not** ignored is the pre-release: a `1.2.3-dev` build offered
as `1.2.3` is refused, because a build heading for a release is not that release.

Two things follow. Passing the full string still works, but if you do, remember that a `+` in a URL
query is decoded as a *space* — it has to be written `%2B`, which is the reason the release number
which is what to quote when asking which build a host runs.
