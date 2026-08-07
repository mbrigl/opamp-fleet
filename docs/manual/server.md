# The Server

[← User Manual](README.md) · [The Client →](client.md)

The Server is the control plane: it holds the configuration the fleet should run, tracks what every
Agent reports back, distributes software, and exposes all of it as an OpenAPI-described REST API. It
runs on Linux, as an ordinary foreground process — unlike the Client, it does not install itself as
a service.

- [What the Server does](#what-the-server-does)
- [Running it](#running-it)
- [Configuration reference](#configuration-reference)
- [Configurations: what the fleet runs](#configurations-what-the-fleet-runs)
- [Packages: distributing software](#packages-distributing-software)
- [The REST API](#the-rest-api)
- [Authentication](#authentication)
- [TLS](#tls)
- [Moving the fleet: connection settings](#moving-the-fleet-connection-settings)
- [What the Server does not do](#what-the-server-does-not-do)

## What the Server does

- **Distributes Configurations to the Agents a Selector names** (ADR-0012), and only when what the
  Agent reports differs from what it should run — every push is gated on a content hash.
- **Tracks the fleet**: which Agents exist, what they report, whether they are connected, healthy,
  and in sync, which Configurations match them, and which packages they have installed.
- **Distributes packages** (ADR-0015): an uploaded artifact, or a reference to one hosted elsewhere
  (ADR-0018), aimed at part of the fleet by Selector (ADR-0017), with one step of rollback history
  (ADR-0019).
- **Restarts a Managed Process on request** — an Agent backed by a Supervisor accepts a restart
  command.
- **Offers new connection settings** (ADR-0014): a credential, a heartbeat interval, or an entirely
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
| `--version` | Print the version and exit. |

Any other argument prints usage and exits with status 2. Logging goes to stderr and is controlled by
the `RUST_LOG` environment variable (default `info`); everything else is in the configuration file.

Stopping the Server is `SIGTERM`/`Ctrl-C`. Configurations and packages are persisted to disk, so a
restart resumes with the same fleet state; Agents reconnect on their own.

Everything is served on the single configured listener (ADR-0005):

| Path | What it is |
|---|---|
| `/v1/opamp` | The OpAMP endpoint. `GET` upgrades to WebSocket, `POST` is the plain-HTTP exchange — the same path serves both (ADR-0007). |
| `/api/v1/…` | The REST API. |
| `/api/v1/openapi.json` | The OpenAPI document — the contract to generate a client from. |
| `/api/v1/docs` | Interactive API documentation (Redoc, vendored and served from this origin, so it works offline). |
| `/` | The bundled UI: one embedded page, no frontend toolchain. It is deliberately rudimentary — the API is the product. |

## Configuration reference

The full annotated example is [`config/server.toml`](../../config/server.toml). Every key is
optional and shown below with its default; an unknown key fails startup rather than being ignored.

### Top level

| Key | Default | Meaning |
|---|---|---|
| `listen` | `"0.0.0.0:4320"` | The single listener, as `address:port`. `4320` is the protocol's default port. |
| `config_dir` | `"fleet-configs"` | Where Configurations are persisted — one JSON file per Configuration, named after it. Written atomically; read back at startup. |
| `packages_dir` | `"fleet-packages"` | Where packages are persisted — one artifact plus metadata each. |
| `max_message_size_bytes` | `67108864` (64 MiB) | The largest OpAMP message accepted or sent, in either direction and on either transport. The protocol requires a limit and recommends this value; a fleet of status reports needs far less. An oversized HTTP request is answered `413`, an oversized WebSocket message closes the connection with `1009`. |
| `max_package_size_bytes` | `1073741824` (1 GiB) | The largest artifact the package-upload route accepts. A package is a program, not a message — an `otelcol-contrib` binary is a few hundred megabytes — so this bound is far larger, and it applies to that one route. |
| `advertised_url` | unset | The absolute base URL advertised for package downloads. Leave it unset for the ordinary single-listener case: the Client resolves the offered path against its own OpAMP endpoint. Set it only when downloads must go through a different host. |

### `[tls]`

Present means the listener serves HTTPS and WSS instead of plain HTTP and WS (ADR-0007). Both keys
are required together.

```toml
[tls]
cert_file = "cert.pem"
key_file = "key.pem"
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

## Configurations: what the fleet runs

A **Configuration** is a name, a body of text, an optional **Selector**, and an optional **role**.

**Names** become file names here, config-map keys on the wire, and entry files on every Client
— including Windows ones. The grammar is therefore narrow: 1–32 characters, lowercase letters,
digits, and `-`, not starting or ending with `-`, and not a Windows reserved device name (`con`,
`nul`, `com1`, …).

**The Selector decides who gets it.** Each `key=value` pair must equal an attribute the Agent
reported — identifying or non-identifying, both are matched. An empty Selector targets every Agent.

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"selector": {"os.type": "linux", "env": "prod"}, "body": "receivers: {}"}' \
       http://127.0.0.1:4320/api/v1/configurations/linux-prod
```

**Several Configurations may match one Agent.** It receives all of them, as named entries in one
config map, and merges them itself. An Agent matching none is left running what it already runs —
the Server never blanks an Agent by omission.

**The role marks content that is not configuration** (ADR-0016). `role: "supplementary"` means the
Managed Process reads this content *by path* — a rule file, a lookup table — so the Client writes it
into the configuration directory under its own name but never passes it to the process as
configuration. An unset role means top-level configuration, which is what every Configuration was
before the option existed. Any other non-empty value travels to the Agent verbatim and is treated
like `supplementary`.

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"body": "rules: []", "role": "supplementary"}' \
       http://127.0.0.1:4320/api/v1/configurations/ruleset
```

**Nothing is sent twice.** The Server composes the entries an Agent should run, hashes them, and
compares that hash to what the Agent reports. Equal hashes mean nothing crosses the wire. This is
why saving an unchanged Configuration is free, and why a role change *does* reach the fleet: the
role is part of the hash.

**How a change travels.** Over WebSocket the Server pushes it within a second; over plain HTTP it
rides the Agent's next poll. The Agent then reports the configuration back as applied or failed,
with the hash it applied — visible on the Agent's row as `remote_config_status`, `remote_config_error`,
and `in_sync`.

## Packages: distributing software

Package delivery is armed by `packages_dir`; the Server declares the capability only while the store
holds something. A package is offered to an Agent when the package's Selector matches it **and** the
Agent accepts packages at all — which is the Client's decision, made by how it names its program
(see [the Client](client.md#which-programs-take-updates)).

**Upload an artifact.** The artifact is the raw request body, its metadata rides the query:

```console
$ curl -X PUT --data-binary @otelcol-contrib_0.109.0_linux_amd64.tar.gz \
       "http://127.0.0.1:4320/api/v1/packages/otelcol?version=0.109.0&signature=$sig"
```

| Query parameter | Meaning |
|---|---|
| `version` | Free-form version string, e.g. a SemVer the Agent can compare. Required. |
| `addon` | `true` marks an addon package. The default is a top-level package — a Managed Process's binary. A Supervisor has no way to apply an addon, so an Agent refuses one with `InstallFailed`. |
| `signature` | Hex-encoded Ed25519 signature over the artifact, verified by the Agent before it installs. |

**Or reference an artifact hosted elsewhere** (ADR-0018). The Server stores the address and your
SHA-256, offers them verbatim, and never downloads the artifact — so the hash, and the signature
when one is configured, is the whole of the protection:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"url": "https://mirror.example/otelcol.tar.gz", "sha256": "…", "version": "0.109.0"}' \
       http://127.0.0.1:4320/api/v1/packages/otelcol/source
```

The URL is probed once, to catch a typo while you are still looking at the screen. A definitive
refusal from the source (a `4xx`) fails the request; a source this Server cannot reach does not,
because the Server is not in the download path and its reachability says nothing about the Agents'.
A private source can be given headers to send.

**Aim it** (ADR-0017). Without a Selector a package reaches every Agent that accepts packages:

```console
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"selector": {"env": "canary"}}' \
       http://127.0.0.1:4320/api/v1/packages/otelcol/selector
```

Where several packages match one Agent, **the most specific Selector wins** — a fleet-wide package
plus a narrower one is how a rollout starts on part of the fleet. Two equally specific Selectors
reaching the same Agent leave it with no offer at all, and the fleet view says so on that Agent, in
`package_conflict`.

**One step back** (ADR-0019). Replacing a package's artifact keeps its predecessor, and a rollback
re-offers it — an ordinary offer naming an older artifact. The Selector is untouched. A package that
has replaced nothing answers `409`:

```console
$ curl -X POST http://127.0.0.1:4320/api/v1/packages/otelcol/rollback
```

**Building and signing** (ADR-0015, ADR-0018). The helper that ships with the Client writes the
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

The download route sits on the unauthenticated REST plane deliberately: the content hash and the
signature are what protect an installed binary, not who was allowed to fetch it.

`keygen` prints the public key as hex — that value is the Client's `[packages] verification_key`.
Once a Client has a key configured, an unsigned package is refused; without one, a *signed* package
is refused too. Decide fleet-wide, not per host.

## The REST API

The OpenAPI document at `/api/v1/openapi.json` is the contract; `/api/v1/docs` renders it. Every
error response carries a JSON body with an `error` field, so a generated client has something to
show.

| Method & path | What it does |
|---|---|
| `GET /api/v1/agents` | The whole fleet: every Agent, its attributes, capabilities, matching Configurations, package installations, health, and sync state. |
| `POST /api/v1/agents/{instance_uid}/restart` | Queue a restart of that Agent's Managed Process. Delivered on the next exchange — pushed over WebSocket, on the next poll over plain HTTP. Only Supervisor-backed Agents accept it; a Client's own Agent has no process to restart. |
| `GET /api/v1/configurations` | Every Configuration. |
| `GET /api/v1/configurations/{name}` | One Configuration. |
| `PUT /api/v1/configurations/{name}` | Create or replace it. Body: `{"selector": {…}, "body": "…", "role": "…"}` — `selector` and `role` may be omitted. |
| `DELETE /api/v1/configurations/{name}` | Remove it. Agents that matched it stop matching; they keep running what they last applied. |
| `GET /api/v1/packages` | Every stored package (never the artifact bytes), including the version a rollback would restore. |
| `PUT /api/v1/packages/{name}` | Upload an artifact. See above for the query parameters. |
| `PUT /api/v1/packages/{name}/source` | Point the package at an artifact hosted elsewhere. |
| `PUT /api/v1/packages/{name}/selector` | Set which Agents it is offered to. |
| `POST /api/v1/packages/{name}/rollback` | Re-offer the version this package replaced. `409` when there is none. |
| `DELETE /api/v1/packages/{name}` | Remove it from the store. |
| `GET /api/v1/packages/{name}/file` | The artifact bytes — where an offered `download_url` points. |

The package routes answer `404` while package delivery is not configured on this Server.

**What a fleet row tells you.** `GET /api/v1/agents` is what the UI renders, and the fields worth
knowing by name:

| Field | Meaning |
|---|---|
| `instance_uid`, `service_name`, `service_version`, `os` | Identity, as the Agent reports it. |
| `identifying_attributes`, `non_identifying_attributes` | Everything a Selector can match on. |
| `capabilities` | The capability set this Agent declared — which tells you, for instance, whether it accepts packages. |
| `matched_configurations`, `desired_hash` | What it should be running. |
| `remote_config_status`, `remote_config_error`, `in_sync` | What it reports about the last configuration it was sent. |
| `effective_config` | What it says it is actually running. |
| `healthy`, `health_status`, `connected`, `transport`, `last_seen_ms` | Liveness. |
| `packages`, `package_conflict` | Package installations, and why an Agent that accepts packages is being offered none. |
| `available_components` | Reported by a Collector carrying the `opampextension`. |

## Authentication

`[auth]` guards **the OpAMP endpoint only** (ADR-0013). Without the section the endpoint is open;
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

**The REST API and the UI are not guarded by this.** Neither is the package download route. Put the
API behind whatever fronts it (a reverse proxy, an existing portal's authentication) if it must not
be public, and rely on signatures rather than access control for artifacts.

Without TLS the credentials travel in cleartext — a Client warns when it sends one beyond the
loopback interface, but it still sends it. Pair `[auth]` with `[tls]` for anything real.

## TLS

`[tls]` turns the single listener into an HTTPS/WSS listener (ADR-0007) — there is no second port,
and no plaintext one left open beside it. Clients then use `wss://` or `https://` endpoints, and
Clients trusting a private CA additionally set `ca_file` in their own `[tls]` section.

The Server presents a server certificate and **verifies no client certificate**. Mutual TLS is not
built; see [`docs/CONFORMANCE.md`](../CONFORMANCE.md).

## Moving the fleet: connection settings

`[connection_offer]` (ADR-0014) is how the fleet is moved to a new credential, a new heartbeat
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

- **It does not authenticate the REST API or the UI**, by design (ADR-0013) — see above.
- **It does not throttle.** It honours the protocol's error and retry semantics and answers
  malformed input with `BAD_REQUEST`, but it never tells an Agent to slow down.
- **It does not download referenced package artifacts.** A referenced package is a URL plus a hash;
  the Agents fetch it.
- **It does not install itself as a service.** Run it under whatever supervises services on the
  host — the Client is the end that ships its own service integration.
- **It has no user model and no multi-tenancy.** One Server manages one fleet.
