- [Gateway Mode: carrying other Clients](#gateway-mode-carrying-other-clients)
- [Agents that are more than one file](#agents-that-are-more-than-one-file)
  Supervisor is an additional Agent. All of them share one connection. The Client's own Agent
  reports `supervisor.toml` itself as its effective configuration — with credential values (`[auth]`'s
  `bearer_token` and `password`, `[packages]`'s `archive_key`) masked as `***`, since the Server
  persists what it receives.
  OpenTelemetry conventions define them, and nothing beyond. Its traces are the one place with no
  convention to follow — the standard names none for an agent's own lifecycle — so the spans are
  named after the operations this project already has a vocabulary for, and their status is the
  standard's, not one of ours.
$ supervisor --config /etc/opamp/supervisor.toml     # foreground; `run` is implied
$ supervisor run --config /etc/opamp/supervisor.toml # the same thing, said explicitly

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

A release artifact is the bare binary, so a freshly downloaded Client has no `supervisor.toml` to edit.
Without one it still installs and starts — on the development defaults, dialling `127.0.0.1` and
managing nothing. `--interactive` is the way past that:

```console
Server OpAMP endpoint [ws://127.0.0.1:4320/v1/opamp]: wss://fleet.example.com/v1/opamp
This Agent's name (service.instance.name) [Supervisor Agent]: host-01
Authentication toward the Server: bearer token
Bearer token: ********
Does the Server present a certificate from a private CA? [y/N]: n
Allow the Server to update this Client's own binary? [y/N]: n
installed supervisor
```

What it asks about is only what has no useful default here: the endpoint, the Agent's name, the
credential ([`[auth]`](#auth)), a private CA when the endpoint is `wss://` or `https://`
([`[tls]`](#tls)), and last — defaulting to **yes** since ADR-0020 — consent for the Server to
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
installed supervisor
```

It writes only the endpoint; everything else keeps its default, and the credential goes into the
file afterwards. All four rules above hold unchanged — in particular, an existing file is kept.

### Installing from a native package

themselves — the layout, the unit and the SCM entry are the same ones this page describes, because
they are made by the same command. No package ships a unit file of its own. What lands on `PATH` —
the command you type is always the binary the service runs.

```console
$ sudo apt install ./supervisor_1.2.3_linux_amd64.deb
$ sudo dnf install ./supervisor_1.2.3_linux_amd64.rpm
```

**The service is registered and left stopped.** That is deliberate: a Client with no configuration
would dial the development default and manage nothing, and a package must not manufacture that state
on every host it touches. Two steps remain, and the post-install prints them:

```console
```

On Windows the `.msi` asks for the installation folder and the endpoint. The folder is the install
root — the `.exe`, `supervisor.toml`, `versions/`, `current` and `state/` all live under it. The same
file installs unattended with the same two answers, which is how Intune, Group Policy and SCCM
deploy it:

```console
C:\> msiexec /i supervisor_1.2.3_windows_amd64.msi /qn ^
       ENDPOINT="wss://fleet.example.com/v1/opamp"
```

Two things to know about living with a packaged install:

- **`dpkg -l` reports the version it *delivered*, not the one that is running.** After a fleet
  self-update ([Updating the Client itself](#updating-the-client-itself)) the service runs the binary
  under `<root>/current/`, which no package manager owns — that separation is what keeps the next
  are the truth.
- **Removing the package stops and unregisters the service and uninstalls every staged version.**
  `supervisor.toml` stay, for the same reason an install never overwrites a configuration: it may hold
  a credential you typed. `apt purge` deletes those too — the instance directory whole. A reinstall
  after a plain remove keeps the host's identity and configuration and stages its own binary fresh.

macOS has no native installer; there, unpack the `.tar.gz` and run `service install` yourself.

### What the service is called





### Where the service's logs are

A Client started by the service manager writes its own log to **`<state_dir>/logs/`** on every

```
<state_dir>/logs/supervisor.2026-08-09.log
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
is exactly what a bad `supervisor.toml`, an unusable certificate, or a refused endpoint does not give
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

The full annotated example is [`config/supervisor.toml`](../../config/supervisor.toml). Every key is
| `name` | `"Supervisor Agent"` | The Agent's `service.instance.name` — your name for *this* Client, shown in the fleet view and matchable by a Selector. Its `service.name` is the constant type `supervisor`, the same on every host. |
These describe the **host**, and every Agent this Client presents carries them — including a fresh
one, in its very first message, before the Server has ever seen it.

To tag **one** Agent among several on a host, use a Server **label** instead
(`PUT /api/v1/agents/<uid>/labels`): it is keyed by that Agent's `instance_uid`, matched by the
same Selectors, and takes effect at once without editing a file on the host. A block's own
`[supervisor.attributes]` table used to do this and no longer exists; a block carrying one fails at
startup with that sentence.
Every Agent additionally reports, without configuration, everything the protocol names to describe
an Agent and where it runs: `service.name`, `service.instance.name`, `service.instance.id`,
`os.type`, `os.name`, `os.version`, `os.description`, `host.name`, `host.arch`, and `host.id` — plus
`service.version`, which for the Client's own Agent is the Client's baked-in version and for a
Supervisor-backed Agent is whatever the Managed Process reports about itself.

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
ca_file = "ca.pem"             # trust: replaces the built-in roots
cert_file = "client.pem"       # identity: what this Client presents
key_file = "client-key.pem"
Every key is optional on its own, so this section may carry a trust override, a client identity, or
both.

`ca_file` is the trust override for `wss://`/`https://` endpoints whose certificate comes from a

`cert_file` and `key_file` are this Client's own certificate for a Server that requires mutual TLS
the **bootstrap certificate** a fresh host enrols with. A certificate the Server issued outranks it:
the Client stores that pair in its state directory as `client-cert.pem` and `client-key.pem` and
prefers it, the same precedence persisted connection settings have over `supervisor.toml`. Deleting the
stored pair reverts to what is written here.

**Enrolment needs nothing in this file.** When the Server declares that it signs certificates, a
Client without one generates a key — which never leaves the host — sends a signing request, and
receives a certificate through the ordinary offer flow, renewing the same way once it is two thirds
through its validity. The private key is written `0600`; on Windows the state directory's ACL is
what protects it.
enabled = true                     # the default; false withdraws the consent
package = "supervisor"             # the default: this Client's own Agent type
See [Updating the Client itself](#updating-the-client-itself). **An absent section is the consent**
(ADR-0020): a Client the fleet cannot update is the one program on the host left to patch by hand.
What bounds it is the name — an offer under any other is refused and reported, never applied — and
carrying this Client is named. Not the Agent type: since ADR-0029 the two are different strings, and
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
  **Connected + Stale**. It needs a heartbeat configured on the Agent to work, since staleness only
  applies to Agents that promised to report periodically.
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
  whole directory `<supervisor_dir>/<name>/` is deleted — identity, written configuration,
  staged packages, and the Client-owned program. A changed Supervisor restarts under its name
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

A Client whose Server never rolls such a Configuration out runs its locally written blocks
exactly as before. Note that once one applied, the Server's set is authoritative: a later local
edit to the blocks stands only until the next rollout act overwrites it.

| `name` | — | This Agent's `service.instance.name` — your name for it — and the directory name it owns. Required; 1–32 lowercase letters, digits, and `-`. Must be unique in the file. A Managed Process can never overwrite it. |
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

[updates]
retain_previous_secs = 86400
```

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
| `version_args` | Arguments that make the program print its version, e.g. `["--version"]`. The program is invoked once with exactly these, and the first SemVer 2.0.0 version in its output becomes the Agent's `service.version`. Opt-in, because a Foreign Agent's version flag is its own convention. **They are also the preflight**: a package's staged program is run with them before what runs is stopped, and a non-zero exit refuses the package with the program's own message — so a build this host cannot run costs a refusal instead of a stop, a swap, a failed start and a rollback. |

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
binary, whatever anyone forgot to aim. What then happens on the host:
[The rollout walkthrough](rollout.md) runs this end to end, from packing the artifact to watching it
land.

One limit worth knowing before you plan a rollout: only a **top-level** package is installed. An
addon is something a Supervisor has no way to apply, so it is refused with `InstallFailed` rather
than written over the binary it was meant to extend.

### Package updates: rollback and retention


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
  `retain_previous_secs`: global in `[updates]`, **one day** by default, overridable in a block of
  an unwrapped kind and by a wrapper that has a reason to. `0` deletes on success. Each Supervisor
  keeps at most the immediately previous version.

```toml
# Global default for every Supervisor (one day shown; the built-in default):
[updates]
retain_previous_secs = 86400
```

## Agents that are more than one file

An executable plus the shared objects it loads — Fluent Bit is the usual example — is delivered by
command = "fluent-bit"            # bare: consent, exactly as everywhere else
program_path = "bin/fluent-bit"   # where the program sits inside the package
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
package = "supervisor"
An offer under any other name is refused and reported, never applied. That is one of two independent
guards, and it is the one on this side of the wire: the Server will not offer a package built for
another Agent type either, and this Client's type is the constant `supervisor` — the same string,
Neither guard replaces the other — an operator who types a Collector artifact as `supervisor` gets
past the Server, and this name is what is left.
running one in the install layout, and proved by running `supervisor self-check` on it before the
**What the Client says it has is the version it runs**, whether a package put it there or a `.deb`,
an `.rpm`, an MSI or a hand did. It reports that under the name `[self_update]` consents to from its
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
uninstalls nothing.

**Where the artifact comes from.** Every release publishes one archive per platform, named
`supervisor_<version>_<os>_<arch>.tar.gz`
([ADR-0029](../adr/0029-releases-installers-and-the-name-supervisor-secure-by-default.md)) — and that file *is* a
package artifact: it holds the Client under the name the install layout gives it, so it is uploaded
exactly as downloaded, and the SHA-256 the release published is the one the Agent verifies. Nothing
the member is `supervisor`, which is what a Client looks for, while the file says which
package it is. The fields are separated by `_` because two of them carry `-` — a package name and a
prerelease version (`1.2.3-dev`) — so the last two fields are the platform and can be read off the
name.


```console
$ curl -X PUT --data-binary @supervisor_1.2.3_linux_amd64.tar.gz \
       "http://<server>:4321/api/v1/packages/supervisor?version=1.2.3&os=linux&arch=amd64"
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "supervisor"}' \
       http://<server>:4321/api/v1/packages/supervisor/type
```

an artifact uploaded and left untyped reaches no Client at all. For this one the type is the

The staged binary's `self-check` compares that against what it reports, ignoring the commit the
build came from — `1.2.3` and `1.2.3+a1b2c3d` are the same release, and the content hash is what
pins *which* bytes arrived. What is **not** ignored is the pre-release: a `1.2.3-dev` build offered
as `1.2.3` is refused, because a build heading for a release is not that release.

Two things follow. Passing the full string still works, but if you do, remember that a `+` in a URL
query is decoded as a *space* — it has to be written `%2B`, which is the reason the release number
which is what to quote when asking which build a host runs.

