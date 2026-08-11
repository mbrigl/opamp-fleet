- [Mutual TLS: proving who is on the connection](#mutual-tls-proving-who-is-on-the-connection)
- [The fleet's own telemetry](#the-fleets-own-telemetry)
- **Distributes Configurations to the Agents a Selector names — when you say so**: saving stores,
  an explicit rollout act releases, and every push is gated on a content hash so nothing the
  Agent already runs is sent again.
- **Distributes packages**: versioned Sets of uploaded artifacts, or references to ones hosted
  elsewhere, aimed at part of the fleet by Selector — released only by an explicit rollout act,
  Agent reports installed, never when it would move it back or leave it where it is.
There are **two listeners, split by audience** (ADR-0012): the one the fleet talks to, and the one
you talk to.

**The Agent plane** — `listen`, `0.0.0.0:4320` by default:

**The Operator plane** — `[rest] listen`, `127.0.0.1:4321` by default:

| Path | What it is |
|---|---|
| `/api/v1/openapi.json` | The OpenAPI document — the contract to generate a client from. It describes this plane, so the artifact download above is not in it. |
`[auth]` guards the OpAMP endpoint and nothing else; the Operator plane has its own credential,
[`[rest.auth]`](#the-operator-plane-restauth), and without it that plane is open to whoever reaches
it. That is why its default address is **loopback**: this port carries the authority to reconfigure
and re-package the whole fleet. Reach it from another host through an SSH tunnel
(`ssh -L 4321:127.0.0.1:4321 <server-host>`), or publish it deliberately with
`[rest] listen = "0.0.0.0:4321"` — and then guard it.

| `listen` | `"0.0.0.0:4320"` | The **Agent plane**, as `address:port`: the OpAMP endpoint and the package downloads. `4320` is the protocol's default port. |
| `max_total_package_bytes` | `17179869184` (16 GiB) | The total size of all stored artifacts before a new upload is refused `507`. Where `max_package_size_bytes` bounds one artifact, this bounds the whole store, so no caller fills the disk by uploading many artifacts under distinct names. `0` is refused at startup. |
| `max_agents` | `100000` | The most Agent records the fleet holds at once. A report bearing a **new** `instance_uid` past this ceiling is answered `Unavailable` rather than admitted, so a peer minting fresh self-asserted UIDs cannot exhaust memory and disk; Agents already known keep reporting. The real defence against an anonymous flood is [`[auth]`](#authentication) — this is the backstop while it is off. `0` is refused at startup. |
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

Present means **both listeners** serve HTTPS and WSS instead of plain HTTP and WS, with the same
certificate and key. `cert_file` and `key_file` are required together; `client_ca_file` is optional,
belongs to the Agent plane alone, and turns on mutual TLS (see
[Mutual TLS](#mutual-tls-proving-who-is-on-the-connection)).
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
## Mutual TLS: proving who is on the connection

must arrive over a connection carrying a client certificate that bundle verifies:
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

A **Configuration** is a name, a body of text, an optional **Agent type**, an optional
**Selector**, and an optional **role**.
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

the `service.name` the Agent reports — compared raw, before the Selector. Unset means every type,
which for a Collector body is rarely what you want: every Agent a Client presents accepts remote
configuration, so an untyped fleet-wide body reaches Foreign Agents and the Client's own Agent
too. (A Selector pair `service.name=…` still works; the field is the visible, first-class way to
say the same thing.)

reported — identifying or non-identifying, both are matched. An empty Selector targets every
Agent of the type (or every Agent, if no type is set either).
       -d '{"service_name": "otelcol-contrib", "selector": {"os.type": "linux", "env": "prod"}, "body": "receivers: {}"}' \
       http://127.0.0.1:4321/api/v1/configurations/linux-prod
$ curl -X POST http://127.0.0.1:4321/api/v1/configurations/linux-prod/rollout
$ curl -X PUT -H 'Content-Type: application/json' \
       -d '{"body": "rules: []", "role": "supplementary"}' \
       http://127.0.0.1:4321/api/v1/configurations/ruleset
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

```console
```

$ curl -X PUT -H 'Content-Type: application/json' -d '{}' \

       -d '{"url": "https://mirror.example/otelcol.tar.gz", "sha256": "…"}' \


**A rollout act never moves an Agent backwards.** Since
[ADR-0027](../adr/0027-rollout-and-what-reaches-an-agent.md) the version an Agent reports

**An Agent that reports no version for the package is held against the version it reports
*running*** — its `service.version`
([ADR-0027](../adr/0027-rollout-and-what-reaches-an-agent.md)).
That is what makes the rule reach a Client installed from a `.deb`, an `.rpm` or an MSI, which has
installed no package and has none to report: no Client is offered the version it already runs, and
none is moved backwards. A `service.version` nothing can order (`1.19`, `24.04.1`) simply says
nothing, so an Agent whose program numbers itself its own way stays reachable.

**Where an Agent reports both, what it *runs* decides**
`service.version` of their own — the Client itself, and an OpAMP-aware Managed Process such as a


- on the host, the version a package superseded is retained for `retain_previous_secs` and put
  back when the new one fails its health gate (see the Client manual, *Package updates: rollback
  and retention*);
- a Client that will not stay up after its own self-update goes back by itself;
- fleet-wide, what is left is publishing the older content **as a new, greater version** — which
  is honest about the fact that the fleet moves forward, and is the only thing the matching rule
  will carry.


artifact, hashes it, and signs it:
$ opamp-package-sign pack --out promtail-3.0.0.tar.gz ./promtail   # prints the sha256
$ opamp-package-sign keygen --out fleet-signing.pk8                # prints the public key
$ sig=$(opamp-package-sign sign --key fleet-signing.pk8 promtail-3.0.0.tar.gz)
`pack` writes `.tar.gz` or an AES-256-encrypted `.7z` — the only two containers a Client can open —
and names the member the way the receiving Supervisor will look for it. There is no ZIP support and
no way to add one: an artifact that is neither gzip nor 7z is taken to *be* the program.
[The rollout walkthrough](rollout.md) puts the whole sequence together.

The download route sits on the **Agent plane**, unauthenticated, deliberately: the content hash and
the signature are what protect an installed binary, not who was allowed to fetch it — and a Client
downloading one presents no credential, which is exactly why guarding the Operator plane cannot
break a rollout.

show. All of it is served on the Operator plane (`127.0.0.1:4321` by default) and, when
[`[rest.auth]`](#the-operator-plane-restauth) is configured, needs Basic credentials.
| `DELETE /api/v1/agents/{instance_uid}` | Forget this Agent — see [Forgetting an Agent](#forgetting-an-agent) below. Reaches no host. `409` while it is still reporting. |
| `POST /api/v1/agents/{instance_uid}/rollout` | The per-Agent rollout act. Empty body: everything the fleet view shows as waiting for this Agent. `{"configuration": "…"}` or `{"package": {"name": "…", "agent_type": "…", "version": "…"}}`: that one resource — any version that fits, aims at, and would upgrade this Agent; `409` with the reason when it would not. |
| `GET /api/v1/configurations` | Every Configuration — the saved revision each. |
| `PUT /api/v1/configurations/{name}` | Create it, or replace its saved revision. Body: `{"selector": {…}, "body": "…", "role": "…", "service_name": "…"}` — everything but `body` may be omitted. **Distributes nothing**. |
| `POST /api/v1/configurations/{name}/rollout` | Roll the saved revision out to every Agent it currently fits and aims at — the moment a change starts travelling. Answers how many Agents were assigned. |
| `DELETE /api/v1/configurations/{name}` | Remove it — from every Agent it was rolled out to as well, which those Agents apply. |




editing a file **on that host** and restarting it.

       http://127.0.0.1:4321/api/v1/agents/<instance-uid>/labels
```

A label is matched exactly like a reported attribute, by **both** halves of the targeting: the

for its next poll.

**A label may not restate an attribute the Agent reports** — that is refused with `409`, naming the
key. Reported attributes are not annotations: `os.type` and `host.arch` decide which artifact fits
label could outrank them, a slip here would offer a host a binary built for another one. Where an
Agent reports something wrong, the fix belongs in that host's `supervisor.toml`, where the wrong value
comes from.

If an Agent *starts* reporting a key that was labelled earlier, the reported value wins and the
fleet row marks the label as shadowed — set, and matching nothing.

**Labels are yours, not the Agent's.** They never travel to it; the Agent only ever experiences the
effect, which is the configuration and the software it is offered. They are stored on the Server and
survive a restart, and **forgetting an Agent does not clear them**: forgetting drops what the Server
in. Clearing them is its own call.

One caveat worth knowing: labels are keyed by Instance UID. If the Server re-keys an Agent — which
it does when two Agents report the same identity — the new identity starts with no labels.

### Forgetting an Agent

A host that was decommissioned leaves a row behind, and nothing ages it out. `DELETE
/api/v1/agents/{instance_uid}` — the `✕ forget` action on a fleet row — drops what this Server knows

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

**The REST API and the UI are not guarded by this** — they are a different plane with a credential
of their own, `[rest.auth]` below. Neither is the package download route, deliberately: an Agent
fetches an artifact without presenting anything, and its content hash and signature are what protect
it.
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

`[tls]` turns **both listeners** into HTTPS/WSS listeners, with one certificate and key — there is
no plaintext port left open beside either of them. Clients then use `wss://` or `https://` endpoints, and
The Server can also **verify a client certificate**, which is the other half of the same section:
see [Mutual TLS](#mutual-tls-proving-who-is-on-the-connection).
## The fleet's own telemetry

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
*and* for every process it supervises; each Client's own log output as OTLP records; and a trace per
fleet operation — `package.install`, `config.apply` (a Managed Process's configuration, and a
Client's Supervisor set), `connection.settings.apply`, and `self.update`. Each carries its phases as
child spans and ends with the outcome the Client reported to this Server, so *which phase* a rollout
failed in is a question the trace answers. Nothing else is traced: the Clients' own message handling
would be a continuous stream with no outcome in it, and would bury the operations that have one.

A log record written during one of those operations carries that trace's id, so a backend holding
both signals can show the lines that explain a failure beside the span that failed. Each Agent's
Resource carries its identifying attributes, so one host's several Agents stay apart at the
receiving end.

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
  outside. Configure the Collector for its own internals as you would without OpAMP.

- **It authenticates the REST API and the UI only if you ask it to** — `[rest.auth]`, Basic, off by
  default, with the plane on loopback until you publish it.
