# The Server

[← User Manual](README.md) · [The Client →](client.md)

The Server is the control plane: it holds the configuration the fleet should run, tracks what every
Agent reports back, distributes software, and exposes all of it as an OpenAPI-described REST API. It
runs on Linux, as an ordinary foreground process — unlike the Client, it does not install itself as
a service.

- [What the Server does](#what-the-server-does)
- [Running it](#running-it)
- [Configuration reference](#configuration-reference)
- [Mutual TLS: proving who is on the connection](#mutual-tls-proving-who-is-on-the-connection)
- [Configurations: what the fleet runs](#configurations-what-the-fleet-runs)
- [Packages: distributing software](#packages-distributing-software)
- [The REST API](#the-rest-api)
- [Authentication](#authentication)
- [TLS](#tls)
- [The fleet's own telemetry](#the-fleets-own-telemetry)
- [Moving the fleet: connection settings](#moving-the-fleet-connection-settings)
- [What the Server does not do](#what-the-server-does-not-do)

## What the Server does

- **Distributes Configurations to the Agents a Selector names — when you say so**: saving stores,
  an explicit rollout act releases, and every push is gated on a content hash so nothing the
  Agent already runs is sent again.
- **Tracks the fleet**: which Agents exist, what they report, whether they are connected, healthy,
  and in sync, which Configurations match them, and which packages they have installed.
- **Distributes packages**: versioned Sets of uploaded artifacts, or references to ones hosted
  elsewhere, aimed at part of the fleet by Selector — released only by an explicit rollout act,
  and only ever forwards: a Set reaches an Agent when it is an upgrade over the version that
  Agent reports installed, never when it would move it back or leave it where it is.
- **Restarts a Managed Process on request** — an Agent backed by a Supervisor accepts a restart
  command.
- **Offers new connection settings**: a credential, a heartbeat interval, or an entirely
  different endpoint, which each Agent verifies by connecting before it switches.
- **Serves one listener for everything**: OpAMP over both transports, the REST API, the OpenAPI
  document and its docs page, and a rudimentary UI.

## Running it

```console
$ server --config /etc/opamp/server.toml
$ server --version
```

| Flag | Meaning |
|---|---|
| `--config <path>` | The TOML configuration file. Defaults to `server.toml` in the working directory; a missing file is not an error, since every setting has a default. |
| `--version` | Print the version and exit — the full string, `1.2.3+<commit>` for a release and `1.2.3-dev+<commit>` for a build on the way to one. |

Any other argument prints usage and exits with status 2. Logging goes to stderr and is controlled by
the `RUST_LOG` environment variable (default `info`); everything else is in the configuration file.

Stopping the Server is `SIGTERM`/`Ctrl-C`. Configurations and packages are persisted to disk, so a
restart resumes with the same fleet state; Agents reconnect on their own.

There are **two listeners, split by audience** (ADR-0066): the one the fleet talks to, and the one
you talk to.

**The Agent plane** — `listen`, `0.0.0.0:4320` by default:

| Path | What it is |
|---|---|
| `/v1/opamp` | The OpAMP endpoint. `GET` upgrades to WebSocket, `POST` is the plain-HTTP exchange — the same path serves both. |
| `/api/v1/packages/{name}/{type}/{version}/file` | An artifact's bytes: the one `/api/v1` route that belongs to the Agents, because the `download_url` in a package offer is a path the Client resolves against *its own* endpoint. Unauthenticated on purpose — a downloading Client presents no credential, and the content hash and signature are what protect the bytes. |

**The Operator plane** — `[rest] listen`, `127.0.0.1:4321` by default:

| Path | What it is |
|---|---|
| `/api/v1/…` | The REST API. |
| `/api/v1/openapi.json` | The OpenAPI document — the contract to generate a client from. It describes this plane, so the artifact download above is not in it. |
| `/api/v1/docs` | Interactive API documentation (Redoc, vendored and served from this origin, so it works offline). |
| `/` | The bundled UI: one embedded page, no frontend toolchain. It is deliberately rudimentary — the API is the product. |

`[auth]` guards the OpAMP endpoint and nothing else; the Operator plane has its own credential,
[`[rest.auth]`](#the-operator-plane-restauth), and without it that plane is open to whoever reaches
it. That is why its default address is **loopback**: this port carries the authority to reconfigure
and re-package the whole fleet. Reach it from another host through an SSH tunnel
(`ssh -L 4321:127.0.0.1:4321 <server-host>`), or publish it deliberately with
`[rest] listen = "0.0.0.0:4321"` — and then guard it.

## Configuration reference

The full annotated example is [`config/server.toml`](../../config/server.toml). Every key is
optional and shown below with its default; an unknown key fails startup rather than being ignored.

### Top level

| Key | Default | Meaning |
|---|---|---|
| `listen` | `"0.0.0.0:4320"` | The **Agent plane**, as `address:port`: the OpAMP endpoint and the package downloads. `4320` is the protocol's default port. |
| `config_dir` | `"fleet-configs"` | Where Configurations are persisted — one JSON file per Configuration, named after it. Written atomically; read back at startup. |
| `packages_dir` | `"fleet-packages"` | Where packages are persisted — one artifact plus metadata each. |
| `max_message_size_bytes` | `67108864` (64 MiB) | The largest OpAMP message accepted or sent, in either direction and on either transport. The protocol requires a limit and recommends this value; a fleet of status reports needs far less. An oversized HTTP request is answered `413`, an oversized WebSocket message closes the connection with `1009`. |
| `max_package_size_bytes` | `1073741824` (1 GiB) | The largest artifact the package-upload route accepts. A package is a program, not a message — an `otelcol-contrib` binary is a few hundred megabytes — so this bound is far larger, and it applies to that one route. |
| `max_total_package_bytes` | `17179869184` (16 GiB) | The total size of all stored artifacts before a new upload is refused `507`. Where `max_package_size_bytes` bounds one artifact, this bounds the whole store, so no caller fills the disk by uploading many artifacts under distinct names. `0` is refused at startup. |
| `max_agents` | `100000` | The most Agent records the fleet holds at once. A report bearing a **new** `instance_uid` past this ceiling is answered `Unavailable` rather than admitted, so a peer minting fresh self-asserted UIDs cannot exhaust memory and disk; Agents already known keep reporting. The real defence against an anonymous flood is [`[auth]`](#authentication) — this is the backstop while it is off. `0` is refused at startup. |
| `stale_after_secs` | `90` | How long an Agent that declares `ReportsHeartbeat` may be silent before the fleet view marks it **stale**. Ignored when `[connection_offer]` names a heartbeat interval — then the budget is three of those. Only heartbeating Agents can go stale: one that promised no periodic report is never late. |
| `advertised_url` | unset | The absolute base URL advertised for package downloads. Leave it unset in the ordinary case: the Client then resolves the offered path against its own OpAMP endpoint, which is exactly where the download is served. Set it only when downloads must go through a different host, such as a mirror. |

### `[rest]`

The Operator plane's listener. Absent means the default.

```toml
[rest]
listen = "127.0.0.1:4321"   # "0.0.0.0:4321" publishes the REST API and the UI to the network
```

| Key | Default | Meaning |
|---|---|---|
| `listen` | `"127.0.0.1:4321"` | Where the REST API, the API docs, and the UI are served. It must differ from `listen` above — two equal addresses are refused at startup by name, rather than surfacing later as *address already in use*. |

#### `[rest.auth]`

Optional Basic authentication over that whole plane — see
[Authentication](#the-operator-plane-restauth). Absent means open.

```toml
[rest.auth.basic_users]
fleet-admin = "a-strong-password"
```

| Key | Default | Meaning |
|---|---|---|
| `basic_users` | *(empty)* | Accepted Basic credentials, `user = "password"`. A section without one, or an entry with an empty name or password, fails startup. |

### `[tls]`

Present means **both listeners** serve HTTPS and WSS instead of plain HTTP and WS, with the same
certificate and key. `cert_file` and `key_file` are required together; `client_ca_file` is optional,
belongs to the Agent plane alone, and turns on mutual TLS (see
[Mutual TLS](#mutual-tls-proving-who-is-on-the-connection)).

```toml
[tls]
cert_file = "cert.pem"
key_file = "key.pem"
client_ca_file = "client-ca.pem"   # optional: require a client certificate on /v1/opamp
```

### `[telemetry_offer]`

Optional. Where Agents send their own telemetry — see
[The fleet's own telemetry](#the-fleets-own-telemetry). At least one endpoint is required if the
section is present.

```toml
[telemetry_offer]
metrics_endpoint = "https://collector.example:4318/v1/metrics"
[telemetry_offer.headers]
Authorization = "Bearer a-telemetry-token"
```

### `[client_ca]`

Optional. Present makes the Server a local CA that signs Agent certificate requests — see
[Issuing certificates](#issuing-certificates-the-csr-flow).

```toml
[client_ca]
cert_file = "client-ca.pem"
key_file = "client-ca-key.pem"
validity_days = 90
```

### `[auth]`

See [Authentication](#authentication).

```toml
[auth]
bearer_tokens = ["a-long-random-token", "the-previous-one-during-rotation"]
[auth.basic_users]
fleet = "a-strong-password"
```

### `[connection_offer]`

See [Moving the fleet](#moving-the-fleet-connection-settings). Any subset of the keys is valid, but
not an empty section.

```toml
[connection_offer]
bearer_token = "a-long-random-token"           # or username = "…" / password = "…"
heartbeat_interval_secs = 30
endpoint = "wss://fleet.example:4320/v1/opamp"
```

## Mutual TLS: proving who is on the connection

`[tls]` gains an optional `client_ca_file`. With it set, every request to `/v1/opamp`
must arrive over a connection carrying a client certificate that bundle verifies:

```toml
[tls]
cert_file = "cert.pem"
key_file = "key.pem"
client_ca_file = "client-ca.pem"
```

Client authentication stays **optional at the TLS layer** and required on the OpAMP route alone.
That is deliberate: the Agent plane also serves the package download, and a Client fetching an
artifact presents no certificate — the content hash and the signature are what protect those bytes. A certificate that *is* presented is always verified — rustls refuses one it cannot
chain before any route sees it.

**Every configured proof must succeed.** `[auth]` alone behaves as it always has. `client_ca_file`
alone makes the endpoint certificate-only. Both configured means **both** are required of every
request, not either one — so turning mutual TLS on can never widen admission. What it can do is shut
out a host that has no certificate yet, which is what the next section is for.

A certificate proves **fleet membership, not identity**. The Server does not match its subject
against an Agent's `instance_uid`: the Server itself may re-key an Agent at any time
(`AgentIdentification`), and a certificate that a re-key invalidates is an outage of your own making.

### Issuing certificates: the CSR flow

Add a `[client_ca]` section and the Server becomes a local CA:

```toml
[client_ca]
cert_file = "client-ca.pem"
key_file = "client-ca-key.pem"
validity_days = 90
```

Use a **separate** CA, not the listener's certificate and key: a CA private key stored where the
server certificate lives means compromising the Server mints fleet members at will. Then point
`[tls] client_ca_file` at that CA's certificate, so the certificates it issues are the ones the
listener accepts.

With the section present the Server declares `AcceptsConnectionSettingsRequest`. A Client that has
no certificate, or holds one two thirds through its validity, generates a key **that never leaves
its host**, sends a signing request, and receives the certificate as an ordinary connection-settings
offer — which it proves by connecting with before it replaces the one in force. Admission is the
approval: a request that got this far already satisfied every proof the endpoint requires. There is
no approval queue.

A request that does not parse, or one arriving at a Server with no `[client_ca]`, is answered with
the protocol's `BadRequest` error response.

### The order that does not lock anyone out

1. Configure `[client_ca]` and restart. Nothing is required of anyone yet; Clients begin enrolling
   on their next connection.
2. Watch them come back with certificates — each Client writes `client-cert.pem` into its state
   directory.
3. Set `[tls] client_ca_file` and restart. Now a certificate is required.
4. Once every host is on one, delete `[auth]` if you want the endpoint to be certificate-only.

Step 4 is not for every fleet. **Keep `[auth]` if you will run Gateways**: a Gateway terminates TLS,
so a client certificate cannot reach the Server through it, and the credential — forwarded unchanged
— is the only per-Agent proof that survives the hop.

**There is no revocation.** Short `validity_days` plus renewal is what bounds a certificate; ejecting
a host faster than its certificate expires means rotating the CA. And an expired certificate locks a
host out even with a valid credential: a Client switched off longer than its validity needs
`client_ca_file` unset for as long as it takes to re-enrol.

## Configurations: what the fleet runs

A **Configuration** is a name, a body of text, an optional **Agent type**, an optional
**Selector**, and an optional **role**.

**Names** become file names here, config-map keys on the wire, and entry files on every Client
— including Windows ones. The grammar is therefore narrow: 1–32 characters, lowercase letters,
digits, and `-`, not starting or ending with `-`, and not a Windows reserved device name (`con`,
`nul`, `com1`, …).

**Saving never distributes**. `PUT` stores the Configuration — complete, aimed, and reaching
nobody. Distribution is a **rollout act**, and there are two of the same meaning:
`POST …/rollout` releases the saved text to **every Agent it currently fits and aims at**, and
the per-Agent control on the fleet view releases it to one Agent. Either act pins a snapshot:
the Agent keeps exactly the revision it was rolled out, so a later edit changes nothing anywhere
— the fleet view shows the newer save *waiting* per Agent — until the next act. An Agent that
connects (or starts matching) after the act waits the same way: nothing is distributed by
enrolment, by a Selector edit, or by a label move.

**Deleting is not inert**: removing a Configuration removes it from every Agent it was rolled out
to; those Agents apply their config map without the entry and restart. Only an Agent left with
nothing assigned keeps running what it runs.

**The type decides whom it can reach at all**. `service_name`, when set, must equal
the `service.name` the Agent reports — compared raw, before the Selector. Unset means every type,
which for a Collector body is rarely what you want: every Agent a Client presents accepts remote
configuration, so an untyped fleet-wide body reaches Foreign Agents and the Client's own Agent
too. (A Selector pair `service.name=…` still works; the field is the visible, first-class way to
say the same thing.)

**The Selector decides who gets it.** Each `key=value` pair must equal an attribute the Agent
reported — identifying or non-identifying, both are matched. An empty Selector targets every
Agent of the type (or every Agent, if no type is set either).

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "otelcol-contrib", "selector": {"os.type": "linux", "env": "prod"}, "body": "receivers: {}"}' \
       http://127.0.0.1:4321/api/v1/configurations/linux-prod
$ curl -X POST http://127.0.0.1:4321/api/v1/configurations/linux-prod/rollout
```

**Several Configurations may match one Agent.** It receives all of them, as named entries in one
config map, and merges them itself. An Agent matching none is left running what it already runs —
the Server never blanks an Agent by omission.

**The role marks content that is not configuration**. `role: "supplementary"` means the
Managed Process reads this content *by path* — a rule file, a lookup table — so the Client writes it
into the configuration directory under its own name but never passes it to the process as
configuration. An unset role means top-level configuration, which is what every Configuration was
before the option existed. Any other non-empty value travels to the Agent verbatim and is treated
like `supplementary`.

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"body": "rules: []", "role": "supplementary"}' \
       http://127.0.0.1:4321/api/v1/configurations/ruleset
```

**Nothing is sent twice.** The Server composes the entries an Agent was **rolled out**, hashes
them, and compares that hash to what the Agent reports. Equal hashes mean nothing crosses the
wire. This is why repeating a rollout act with unchanged content is free, and why a role change
*does* reach the fleet once rolled out: the role is part of the hash.

**How a change travels.** The rollout act is the moment it starts — over WebSocket the Server
pushes it within a second; over plain HTTP it rides the Agent's next poll. The Agent then reports
the configuration back as applied or failed, with the hash it applied — visible on the Agent's
row as `remote_config_status`, `remote_config_error`, and `in_sync`.

**The fleet view shows what waits.** Per Agent, `GET /api/v1/agents` answers what is rolled out
to it (`assigned_configurations`, `assigned_packages`) and what could be
(`pending_configurations`, `pending_packages` — a candidate not yet rolled out, or a newer save
than the one in force). The Server never acts on the waiting list by itself; the per-Agent
rollout (`POST /api/v1/agents/{instance_uid}/rollout`, empty body for everything waiting, or
`{"configuration": "…"}` / `{"package": {…}}` for one resource) is the operator's press.

## Packages: distributing software

Package delivery is armed by `packages_dir`; the Server declares the capability only while the store
holds something. A package is offered to an Agent when the package **fits** it — built for its Agent
type *and* for the machine it reported — *and* the package's Selector matches it, *and* the Agent
accepts packages at all, which is the Client's decision, made by how it names its program
(see [the Client](client.md#how-a-block-names-its-program)).

**Nothing is offered before it is rolled out**. Uploading stores a Set — complete, typed, aimed,
and reaching no Agent however finished it is. Distribution is the **rollout act**, the same act
Configurations have: the Set's own act releases it to every Agent it fits and its Selector aims
at, and the per-Agent control on the fleet view releases it to one Agent:

```console
$ curl -X POST http://<server>:4321/api/v1/packages/otelcol/otelcol-contrib/1.2.3/rollout
```

So five platforms' artifacts can be uploaded, aimed, and then released together — and the window
in which a half-described package is already reaching the fleet does not exist. An Agent that
enrols later waits, marked pending on its fleet row, for an act of its own. While a Set is rolled
out to at least one Agent its entries are **frozen** (the fleet is installing those bytes;
uploads answer `409`); ship a change as the next version, which is a new Set. Deleting a Set
withdraws it from every Agent it was rolled out to, and **nothing is uninstalled** — an Agent
keeps running what it installed.

**Fit is mandatory, aim is optional** — and fit runs first, in two steps. A
Set built for another **Agent type** is not a candidate at all: its type is matched against the
`service.name` the Agent reports, so a Promtail artifact never reaches a Collector even with no
Selector on it. Then every entry built for another **platform** is dropped. Only what survives
both is aimed.

**A package is a Set**: identified by *name, Agent type, and version* — stated at creation,
never edited; a new version is a new Set — holding one entry per platform: the Linux build, the
macOS builds, the Windows build. The Server hands each Agent the entry built for its `os.type`
and `host.arch` and never another, so a mixed fleet is one rollout rather than one per platform.
**The Selector aims, the type and the platform fit** — which Agents of a kind a Set is for is
your choice, what kind and which bytes are not.

**Create the Set, then upload its entries.** The identity is the path; the type is compared
**raw** against the `service.name` the Agents report — there is no canonical set of Agent types,
so spell it exactly as they do; a typo here is a rollout act that reaches nobody rather than an
error, and **`targeted_agents` is how you catch it** (the package list in the UI shows
`⚠ 0`). The artifact is the raw request body of its entry route:

```console
$ curl -X PUT -H 'Content-Type: application/json' -d '{}' \
       http://127.0.0.1:4321/api/v1/packages/otelcol/otelcol-contrib/0.109.0
$ curl -X PUT --data-binary @otelcol-contrib_0.109.0_linux_amd64.tar.gz \
       "http://127.0.0.1:4321/api/v1/packages/otelcol/otelcol-contrib/0.109.0/entries/linux/amd64?signature=$sig"
```

The create body takes `{"selector": {…}, "addon": true|false}` — an addon marks content a
Supervisor applies beside the program; the default is a top-level package, a Managed Process's
binary. Platform spellings are accepted and stored canonically, so the tokens off an upstream
release's file name work as they are: `macos` and `osx` mean `darwin`, `x86_64` and `x64` mean
`amd64`, `aarch64` means `arm64`. An `os`/`arch` this Server has never heard of is stored as
given rather than refused — the fleet may run a system nobody here anticipated.

**Or reference an artifact hosted elsewhere**. The Server stores the address and your
SHA-256, offers them verbatim, and never downloads the artifact — so the hash, and the signature
when one is configured, is the whole of the protection:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"url": "https://mirror.example/otelcol.tar.gz", "sha256": "…"}' \
       http://127.0.0.1:4321/api/v1/packages/otelcol/otelcol-contrib/0.109.0/entries/linux/amd64/source
```

The URL is probed once, to catch a typo while you are still looking at the screen. A definitive
refusal from the source (a `4xx`) fails the request; a source this Server cannot reach does not,
because the Server is not in the download path and its reachability says nothing about the Agents'.
A private source can be given headers to send.

**Aim it**. Without a Selector a rollout act reaches every Agent **of the Set's type** that
accepts packages:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"selector": {"env": "canary"}}' \
       http://127.0.0.1:4321/api/v1/packages/otelcol/otelcol-contrib/0.109.0/selector
```

A Selector aims **every** platform of that Set at once, because the aim belongs to the Set — and
editing it distributes nothing: it decides whom the *next* rollout act reaches.

Where several Sets of one name would reach an Agent, **the most specific Selector wins the
proposal**, and among equally specific ones the greater version — a fleet-wide Set plus a
narrower canary one is how a rollout starts on part of the fleet. Two equally specific Selectors
of *different* names reaching the same Agent leave it with no proposal at all, and the fleet view
says so on that Agent, in `package_conflict`.

**An Agent that reports no `service.name`, `os.type`, or `host.arch` is offered nothing** — there is
no artifact that can be known to be meant for it or to run on it, and guessing is how a fleet-wide
outage starts. Every Client this project ships reports all three; a foreign OpAMP client that
reports no type says so on its fleet row rather than leaving you with a rollout that never starts.

**A rollout act never moves an Agent backwards.** Since
[ADR-0076](../adr/0076-a-set-reaches-an-agent-only-as-an-upgrade.md) the version an Agent reports
installed is part of matching: a Set reaches it only if the Set's version is **greater**, compared
as SemVer (major, minor, patch, then the pre-release rules). Equal is not greater — a Set an Agent
already runs reaches it with nothing — and a reported version nothing can order is refused rather
than guessed at. The bulk act skips such Agents; the per-Agent act answers `409` and says which
version the Agent reports.

**An Agent that reports no version for the package is held against the version it reports
*running*** — its `service.version`
([ADR-0079](../adr/0079-the-version-an-agent-runs-stands-in-for-an-unreported-package-version.md)).
That is what makes the rule reach a Client installed from a `.deb`, an `.rpm` or an MSI, which has
installed no package and has none to report: no Client is offered the version it already runs, and
none is moved backwards. A `service.version` nothing can order (`1.19`, `24.04.1`) simply says
nothing, so an Agent whose program numbers itself its own way stays reachable.

**Where an Agent reports both, what it *runs* decides**
([ADR-0083](../adr/0083-what-reaches-an-agent.md) points 2 and 3). The Set must be greater than the
`service.version` the Agent reports, and the package status is not read beside it — neither to admit
a Set the running version refuses, nor to refuse one it admits. A statement about the present
outranks a record of an install, which outlives the binary it describes. So a claim the Agent's own
program denies never holds the package back, in either direction: a Client reporting `supervisor
0.4.2` installed while reporting that it runs 0.4.0 — a state directory that outlived its binary, a
self-update that staged and did not take effect — is offered 0.4.1, where the claim used to refuse
it as a downgrade and strand the host for good. The `409` names the version that decided and says
that the claim was not consulted.

**What this costs, and whom it touches.** Where a program reports a version *above* the Set that
carries it, no Set below that number reaches it any more, and where it reports one below, a Set
between the two can move its package backwards — a Collector calling itself `0.98.0` under an
`otelcol` Set at `2.0.0` can be assigned a `1.5.0`. Nothing moves on its own: a rollout is an
explicit act and you see the version you press. This reaches only Agents that report a
`service.version` of their own — the Client itself, and an OpAMP-aware Managed Process such as a
Collector carrying `opampextension`. An Icinga 2 or a GLPI Agent reports none, so its Sets keep
being matched on the package status alone. **Number a Set the way the program it carries numbers
itself**, and neither case arises.

Versions are still kept side by side — a new version is a new Set, and the older artifact stays in
the store — but **taking a bad version back is not a rollout**:

- on the host, the version a package superseded is retained for `retain_previous_secs` and put
  back when the new one fails its health gate (see the Client manual, *Package updates: rollback
  and retention*);
- a Client that will not stay up after its own self-update goes back by itself;
- fleet-wide, what is left is publishing the older content **as a new, greater version** — which
  is honest about the fact that the fleet moves forward, and is the only thing the matching rule
  will carry.

**Deleting.** `DELETE /api/v1/packages/otelcol/otelcol-contrib/0.109.0` removes the Set — its
entries, artifacts, metadata, and every per-Agent assignment that referenced it; the entry route
with a platform removes just that one entry. Nothing is uninstalled either way.

**Building and signing**. The helper that ships with the Client writes the
artifact, hashes it, and signs it:

```console
$ opamp-package-sign pack --out promtail-3.0.0.tar.gz ./promtail   # prints the sha256
$ opamp-package-sign keygen --out fleet-signing.pk8                # prints the public key
$ sig=$(opamp-package-sign sign --key fleet-signing.pk8 promtail-3.0.0.tar.gz)
```

`pack` writes `.tar.gz` or an AES-256-encrypted `.7z` — the only two containers a Client can open —
and names the member the way the receiving Supervisor will look for it. There is no ZIP support and
no way to add one: an artifact that is neither gzip nor 7z is taken to *be* the program.
[The rollout walkthrough](rollout.md) puts the whole sequence together.

The download route sits on the **Agent plane**, unauthenticated, deliberately: the content hash and
the signature are what protect an installed binary, not who was allowed to fetch it — and a Client
downloading one presents no credential, which is exactly why guarding the Operator plane cannot
break a rollout.

`keygen` prints the public key as hex — that value is the Client's `[packages] verification_key`.
Once a Client has a key configured, an unsigned package is refused; without one, a *signed* package
is refused too. Decide fleet-wide, not per host.

## The REST API

The OpenAPI document at `/api/v1/openapi.json` is the contract; `/api/v1/docs` renders it. Every
error response carries a JSON body with an `error` field, so a generated client has something to
show. All of it is served on the Operator plane (`127.0.0.1:4321` by default) and, when
[`[rest.auth]`](#the-operator-plane-restauth) is configured, needs Basic credentials.

| Method & path | What it does |
|---|---|
| `GET /api/v1/agents` | The whole fleet: every Agent, its attributes, capabilities, matching Configurations, package installations, health, and sync state. |
| `POST /api/v1/agents/{instance_uid}/restart` | Queue a restart of that Agent's Managed Process. Delivered on the next exchange — pushed over WebSocket, on the next poll over plain HTTP. Only Supervisor-backed Agents accept it; a Client's own Agent has no process to restart. |
| `PUT /api/v1/agents/{instance_uid}/labels` | Set this Agent's labels — see [Labels: rollout rings without touching the host](#labels-rollout-rings-without-touching-the-host). Body: `{"labels": {…}}`; an empty map clears them. |
| `DELETE /api/v1/agents/{instance_uid}` | Forget this Agent — see [Forgetting an Agent](#forgetting-an-agent) below. Reaches no host. `409` while it is still reporting. |
| `POST /api/v1/agents/{instance_uid}/rollout` | The per-Agent rollout act. Empty body: everything the fleet view shows as waiting for this Agent. `{"configuration": "…"}` or `{"package": {"name": "…", "agent_type": "…", "version": "…"}}`: that one resource — any version that fits, aims at, and would upgrade this Agent; `409` with the reason when it would not. |
| `GET /api/v1/configurations` | Every Configuration — the saved revision each. |
| `GET /api/v1/configurations/{name}` | One Configuration. |
| `PUT /api/v1/configurations/{name}` | Create it, or replace its saved revision. Body: `{"selector": {…}, "body": "…", "role": "…", "service_name": "…"}` — everything but `body` may be omitted. **Distributes nothing**. |
| `POST /api/v1/configurations/{name}/rollout` | Roll the saved revision out to every Agent it currently fits and aims at — the moment a change starts travelling. Answers how many Agents were assigned. |
| `DELETE /api/v1/configurations/{name}` | Remove it — from every Agent it was rolled out to as well, which those Agents apply. |
| `GET /api/v1/packages` | Every stored Set (never the artifact bytes), with `matching_agents` and `targeted_agents` — see [Whom a package actually reaches](#whom-a-package-actually-reaches). |
| `PUT /api/v1/packages/{name}/{agent_type}/{version}` | Create a Set, or update its Selector and kind. Body: `{"selector": {…}, "addon": false}`. **Distributes nothing**. |
| `GET /api/v1/packages/{name}/{agent_type}/{version}` | One Set. |
| `PUT /api/v1/packages/{name}/{agent_type}/{version}/entries/{os}/{arch}` | Upload one platform's artifact (the raw body; optional `?signature=<hex>`). `409` while the Set is rolled out to an Agent. |
| `PUT /api/v1/packages/{name}/{agent_type}/{version}/entries/{os}/{arch}/source` | Point that entry at an artifact hosted elsewhere. Body: `{"url": "…", "sha256": "…", "signature": "…", "headers": {…}}`. |
| `DELETE /api/v1/packages/{name}/{agent_type}/{version}/entries/{os}/{arch}` | Remove one entry. `409` while the Set is rolled out to an Agent. |
| `PUT /api/v1/packages/{name}/{agent_type}/{version}/selector` | Set whom a rollout act would release it to. Never distributes. |
| `POST /api/v1/packages/{name}/{agent_type}/{version}/rollout` | Roll the Set out to every Agent it fits, its Selector aims at, and it would upgrade — the moment a rollout starts. Agents it would not move forward are skipped, and `assigned_agents` counts what it actually changed. `409` while the Set holds no entries. |
| `DELETE /api/v1/packages/{name}/{agent_type}/{version}` | Remove the Set — and every per-Agent assignment that referenced it. Uninstalls nothing. |
| `GET /api/v1/packages/{name}/{agent_type}/{version}/file?os=…&arch=…` | The artifact bytes — where an offered `download_url` points. **The one route on the Agent plane** (`:4320`), and never guarded by `[rest.auth]`: it is not in the OpenAPI document for the same reason. |

The package routes answer `404` while package delivery is not configured on this Server.

### Whom a package actually reaches

Every stored package carries **two** counts, and reading them together is the point:

- **`matching_agents`** — how many Agents this Set fits and its Selector aims at, whatever they
  currently run.
- **`targeted_agents`** — how many of those a rollout act would actually change: the ones this Set
  would **upgrade**. This is the number the UI puts on the *Roll out* button.

Neither is a separate calculation — the Server answers them exactly as it matches a Set to an
Agent, so a number cannot promise a reach the fleet does not get.

**Zero in `matching_agents` is the number to look for.** A package aims at nobody when

- its **Agent type** is unset or misspelled — compared raw, with nothing to catch a typo;
- no **artifact** matches a platform any Agent reports;
- its **Selector** matches no Agent; or
- two equally specific Selectors reach the same Agents, so the Server refuses to guess and offers
  nothing.

None of those is an upload error — the package stores fine, validates fine, and reaches no one — so
without this number a mistyped rollout looks exactly like a successful one until somebody notices
the version never moved. The UI shows this case as `⚠ 0`.

**Zero in `targeted_agents` with a non-zero `matching_agents` is not a fault.** It means every
Agent this Set aims at already runs this version or a newer one, so there is nothing for an act to
do; the UI states it plainly rather than warning about it.

It counts the fleet **as reported so far**, which means a package staged ahead of the hosts it is
for legitimately reads `0`. That is why it is a number to read rather than something the Server
refuses to store.

The number answers "whom would a rollout act reach *now*" — checking the aim before the act is
what it is for. Which Agents actually run the Set is a fact about the Agents, answered per row by
`GET /api/v1/agents` (`assigned_packages`, `pending_packages`).

**What a fleet row tells you.** `GET /api/v1/agents` is what the UI renders, and the fields worth
knowing by name:

| Field | Meaning |
|---|---|
| `connected`, `stale` | Two facts, not one. `connected` says a connection carrying this Agent is open — behind a Gateway, the *Gateway's*. `stale` says nothing has been heard from the Agent itself for longer than its budget. `connected: true, stale: true` is the gatewayed Agent whose Client went away. On plain HTTP there is no socket to close, so `connected` turns true on the first poll and is cleared only by the Agent's `agent_disconnect` on shutdown — a poller that dies without saying goodbye stays `connected` forever, and `stale` is the fact worth reading. |
| `instance_uid`, `service_name`, `service_instance_name`, `service_version`, `os` | Identity, as the Agent reports it. `service_name` is the Agent *type* — what it is, shared by every Agent of that kind — and `service_instance_name` is the operator's name for this one; it is empty for a foreign OpAMP client that reports none. |
| `identifying_attributes`, `non_identifying_attributes` | Everything the Agent reports, and part of what a Selector matches. |
| `labels`, `shadowed_labels` | The labels you set, matched by Selectors like a reported attribute. `shadowed_labels` names the ones the Agent's own reports override — set, and matching nothing. |
| `capabilities` | The capability set this Agent declared — which tells you, for instance, whether it accepts packages. |
| `matched_configurations`, `desired_hash` | What it should be running. |
| `remote_config_status`, `remote_config_error`, `in_sync` | What it reports about the last configuration it was sent. |
| `effective_config` | What it says it is actually running. |
| `healthy`, `health_status`, `health_error`, `connected`, `transport`, `last_seen_ms` | Liveness — and `connected` alone is not it. A Supervisor whose Managed Process will not start is still `connected` (the Supervisor lives); `healthy: false` with `health_status` (`no process installed`, `awaiting configuration`, …) and the reason in `health_error` is where that shows. The UI's *Health* column renders exactly these three: the badge carries `health_status`, and `health_error` — often a whole command line — opens in a dialog when you click it, so one broken Agent does not take twenty rows of screen. |
| `packages`, `package_conflict` | Package installations, and why an Agent that accepts packages is being offered none. |
| `package_error` | Why the Agent refused the *offer itself* — no package status carries this, and the Client's own Agent refusing a package `[self_update]` did not name is the case it exists for. |
| `available_components` | Reported by a Collector carrying the `opampextension`. |

### Labels: rollout rings without touching the host

A Selector aims a Configuration or a package at the Agents whose attributes match it — and the
attribute a staged rollout actually wants, `rollout = "canary"`, is one you invent. Until now it
could only be invented in `[attributes]` in `supervisor.toml`, so moving a host between rings meant
editing a file **on that host** and restarting it.

**Labels are that attribute, set from here**:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"labels": {"rollout": "canary"}}' \
       http://127.0.0.1:4321/api/v1/agents/<instance-uid>/labels
```

A label is matched exactly like a reported attribute, by **both** halves of the targeting: the
Configuration an Agent is sent and the package it is offered. So a canary
rollout of a new collector binary is a Selector of `rollout = canary` on the package plus this call
on the hosts that should get it first — and moving a host out again is the same call with an empty
map.

It takes effect at once: a connected Agent is pushed whatever its new ring gets, rather than waiting
for its next poll.

**A label may not restate an attribute the Agent reports** — that is refused with `409`, naming the
key. Reported attributes are not annotations: `os.type` and `host.arch` decide which artifact fits
the machine and `service.name` decides which packages fit it at all. If a
label could outrank them, a slip here would offer a host a binary built for another one. Where an
Agent reports something wrong, the fix belongs in that host's `supervisor.toml`, where the wrong value
comes from.

If an Agent *starts* reporting a key that was labelled earlier, the reported value wins and the
fleet row marks the label as shadowed — set, and matching nothing.

**Labels are yours, not the Agent's.** They never travel to it; the Agent only ever experiences the
effect, which is the configuration and the software it is offered. They are stored on the Server and
survive a restart, and **forgetting an Agent does not clear them**: forgetting drops what the Server
learned, while a label is something you decided, so a host that comes back is in the ring you put it
in. Clearing them is its own call.

One caveat worth knowing: labels are keyed by Instance UID. If the Server re-keys an Agent — which
it does when two Agents report the same identity — the new identity starts with no labels.

### Forgetting an Agent

A host that was decommissioned leaves a row behind, and nothing ages it out. `DELETE
/api/v1/agents/{instance_uid}` — the `✕ forget` action on a fleet row — drops what this Server knows
about that Agent.

**It does nothing on the machine.** No process is stopped, nothing is uninstalled, and no credential
is revoked: a credential here proves *fleet membership*, never which Agent is speaking, so there is
none belonging to one Agent to take away. A Client that is still running and still pointed at this
Server reports again within its polling or heartbeat interval and the row comes back. Forgetting
tidies the view; **to remove an agent for good, stop it on the host** (`supervisor service
uninstall`) and then forget it here.

It is refused with `409` while the Agent is still reporting — connected, and heard from within the
staleness budget. That is not caution for its own sake: the record holds the hashes that tell this
Server not to re-offer what an Agent already has, so forgetting a live Agent has its configuration
sent again, and a Managed Process restarts whenever a configuration arrives. Stop the agent first,
or wait for it to fall silent. An Agent that is already disconnected can be forgotten at once.

The same applies to one that comes back later: it is offered its configuration, its connection
settings, and its packages afresh. The packages cost nothing — the Client re-installs nothing whose
content hash it already has — but the configuration is applied again, which for a managed agent is
one restart. That is the price of forgetting something that was not really gone.

Nothing expires on its own: there is no retention sweep and no inactivity timeout, so a row stays
until someone forgets it.

## Authentication

`[auth]` guards **the OpAMP endpoint only**. Without the section the endpoint is open;
with it, every OpAMP request — plain-HTTP `POST` or WebSocket upgrade — needs an `Authorization`
header matching one of the listed credentials, and anything else is answered `401`.

```toml
[auth]
bearer_tokens = ["a-long-random-token"]
[auth.basic_users]
fleet = "a-strong-password"
```

Both schemes may be configured at once, and several valid credentials may be listed — which is what
makes an overlapping rotation possible.

**The REST API and the UI are not guarded by this** — they are a different plane with a credential
of their own, `[rest.auth]` below. Neither is the package download route, deliberately: an Agent
fetches an artifact without presenting anything, and its content hash and signature are what protect
it.

Without TLS the credentials travel in cleartext — a Client warns when it sends one beyond the
loopback interface, but it still sends it. Pair `[auth]` with `[tls]` for anything real.

### The Operator plane: `[rest.auth]`

`[rest.auth]` guards **the whole Operator plane** — `/api/v1/…`, the OpenAPI document, the API docs,
and the UI at `/`. Without the section that plane is open, which is why its default address is
loopback; with it, every request needs Basic credentials and anything else is answered `401` with a
`WWW-Authenticate: Basic` challenge.

```toml
[rest]
listen = "0.0.0.0:4321"          # publishing it is the reason to add the section below

[rest.auth.basic_users]
fleet-admin = "a-strong-password"
```

Basic, and only Basic, because the audience is a browser and `curl`: the browser answers the
challenge by itself, so the bundled UI needs no login page, no session, and no cookie. Several users
are how a credential is rotated — add the new one, hand it out, remove the old — or how one
operator's is withdrawn without touching anyone else's.

**The operator tools carry it in the URL** they are given, which needs no new flag:

```console
$ curl -u fleet-admin:secret http://127.0.0.1:4321/api/v1/agents
$ opamp-package-fetch … --server http://fleet-admin:secret@127.0.0.1:4321
```

Two limits worth stating plainly. It is **authentication, not authorization**: everyone listed can
do everything the plane offers — there are no roles, and one Server still manages one fleet. And
Basic sends a reusable password on **every** request, so it is only as private as the channel under
it: pair `[rest.auth]` with `[tls]`, or put a TLS-terminating proxy in front. The Server logs a
warning at startup when the plane is published in cleartext with a credential configured. Passwords
are stored in `server.toml` verbatim, exactly as `[auth]`'s are.

## TLS

`[tls]` turns **both listeners** into HTTPS/WSS listeners, with one certificate and key — there is
no plaintext port left open beside either of them. Clients then use `wss://` or `https://` endpoints, and
Clients trusting a private CA additionally set `ca_file` in their own `[tls]` section.

The Server can also **verify a client certificate**, which is the other half of the same section:
see [Mutual TLS](#mutual-tls-proving-who-is-on-the-connection).

## The fleet's own telemetry

`[telemetry_offer]` is where the fleet's Clients send their own metrics, logs, and
traces. Each signal is independent, and each is offered only to Agents that declare they can report
it:

```toml
[telemetry_offer]
metrics_endpoint = "https://collector.example:4318/v1/metrics"
traces_endpoint = "https://collector.example:4318/v1/traces"
logs_endpoint = "https://collector.example:4318/v1/logs"
[telemetry_offer.headers]
Authorization = "Bearer a-telemetry-token"
```

The endpoints are **full OTLP/HTTP URLs with path**. This Server appends no `/v1/metrics` for you:
guessing a receiver's routing is how telemetry disappears into a `404` nobody looks at.

**Nothing is configured on the Client.** The capability an Agent declares means "I can report to the
destination *you* name", so this section is the only place a destination comes from — and with no
section, no Agent sends anything.

What arrives: process metrics every 30 seconds (CPU, memory, uptime) for each Client's own process
*and* for every process it supervises; each Client's own log output as OTLP records; and one span
per operation that already has a lifecycle here — a configuration being applied, a package being
installed, a self-update. Each Agent's Resource carries its identifying attributes, so one host's
several Agents stay apart at the receiving end.

Two limits worth knowing before you point this somewhere:

- **A cleartext destination is refused outside the private address space.** `http://` is accepted to
  loopback and to the private ranges — `10/8`, `172.16/12`, `192.168/16`, `fc00::/7` — and rejected
  anywhere else by the Agent and reported back, because the stream carries identifying attributes and
  whatever the Client logs. The protocol permits exactly this refusal. The judgement is made on the
  **address**: a host name over `http://` is refused whatever it resolves to, since a name can be
  re-pointed after the offer was admitted. So a Collector one hop away on the LAN needs no
  certificate — `http://192.168.10.5:4318/v1/metrics` is accepted — and one reached by name, or
  across anything public, needs TLS in front of it.
- **A Collector's internal telemetry does not come this way.** The Client must not touch a Managed
  Process's configuration, so what it reports about a Collector is what it can see from
  outside. Configure the Collector for its own internals as you would without OpAMP.

## Moving the fleet: connection settings

`[connection_offer]` is how the fleet is moved to a new credential, a new heartbeat
interval, or a new endpoint without touching every host. The Server compiles the section into one
hash-gated offer, and every Agent that accepts connection settings gets it, **verifies it by
actually connecting**, and switches only on success. An Agent that cannot connect with the offered
settings keeps the ones it has and reports the failure.

Rotating a credential is therefore a sequence with no window in which the fleet is locked out:

1. Add the **new** token to `[auth]`, keeping the old one — both are accepted.
2. Point `[connection_offer]` at the new token.
3. Restart the Server. The offer goes out; each Agent verifies it and switches.
4. Once the fleet has migrated, drop the old token from `[auth]` and restart again.

Unless `endpoint` points at a *different* Server, the offered credential must be in this Server's
accepted set — a Server that offered a credential it would itself reject would lock out the fleet,
so it fails at startup instead.

The offer never carries `certificate`, `tls`, or `proxy`: there is no configuration surface for
them, which is the Server half of the missing mutual-TLS support.

## What the Server does not do

- **It authenticates the REST API and the UI only if you ask it to** — `[rest.auth]`, Basic, off by
  default, with the plane on loopback until you publish it.
- **It does not throttle.** It honours the protocol's error and retry semantics and answers
  malformed input with `BAD_REQUEST`, but it never tells an Agent to slow down.
- **It does not download referenced package artifacts.** A referenced package is a URL plus a hash;
  the Agents fetch it.
- **It does not install itself as a service.** Run it under whatever supervises services on the
  host — the Client is the end that ships its own service integration.
- **It has no user model and no multi-tenancy.** One Server manages one fleet.
