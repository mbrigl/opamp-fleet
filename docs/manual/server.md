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
- [Enrolment: a new host, approved by an operator](#enrolment-a-new-host-approved-by-an-operator)
- [Configurations: what the fleet runs](#configurations-what-the-fleet-runs)
- [Packages and Deployments: distributing software](#packages-and-deployments-distributing-software)
- [The REST API](#the-rest-api)
- [Authentication](#authentication)
- [Upgrading to admission by certificate alone](#upgrading-to-admission-by-certificate-alone)
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
  and only ever forwards: a Package reaches an Agent when it is an upgrade over the version that
  Agent reports installed, never when it would move it back or leave it where it is.
- **Restarts a Managed Process on request** — an Agent backed by a Supervisor accepts a restart
  command.
- **Offers new connection settings**: a heartbeat interval or an entirely different endpoint,
  which each Agent verifies by connecting before it switches.
- **Admits an Agent on one proof**: a client certificate in the TLS handshake. It signs Agent
  certificates as a local CA, and enrols a new host only on an operator's approval.
- **Serves two listeners over TLS 1.3**: OpAMP over both transports and the package download on
  one, the REST API, the OpenAPI document and its docs page, and a rudimentary UI on the other.

## Running it

```console
$ server --config /etc/opamp/server.toml
$ server --version
```

| Flag | Meaning |
|---|---|
| `--config <path>` | The TOML configuration file. Defaults to `server.toml` in the working directory. A missing file means the defaults, and the defaults hold no `[tls]`, so the Server refuses to start and names `[tls]`. |
| `--version` | Print the version and exit — the full string, `1.2.3+<commit>` for a release and `1.2.3-dev+<commit>` for a build on the way to one. |

Any other argument prints usage and exits with status 2. Logging goes to stderr and is controlled by
the `RUST_LOG` environment variable (default `info`); everything else is in the configuration file.

Stopping the Server is `SIGTERM`/`Ctrl-C`. Configurations and packages are persisted to disk, so a
restart resumes with the same fleet state; Agents reconnect on their own.

The Server refuses to start without `[tls]` and `[tls] client_ca_file`, and the refusal names what
is missing. It also refuses an `[auth]` section, and a credential key in `[connection_offer]`: the
Agent plane admits by client certificate alone (see
[Upgrading to admission by certificate alone](#upgrading-to-admission-by-certificate-alone)). For
a first run on one machine, [`scripts/dev-pki.sh`](../../scripts/dev-pki.sh) makes a development
set of certificates and a `server.toml` that uses it; the [quick start](README.md#quick-start-a-closed-loop-on-one-machine)
walks through it.

There are **two listeners, split by audience**
([ADR-0023](../adr/0023-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)): the one
the fleet talks to, and the one you talk to. Both serve TLS 1.3 and nothing older, with the same
certificate.

**The Agent plane** — `listen`, `127.0.0.1:4320` by default. Serving the fleet is one deliberate
line, `listen = "0.0.0.0:4320"`.

| Path | What it is |
|---|---|
| `/v1/opamp` | The OpAMP endpoint. `GET` upgrades to WebSocket, `POST` is the plain-HTTP exchange — the same path serves both. |
| `/api/v1/packages/{agent_type}/{version}/file` | An artifact's bytes: the one `/api/v1` route that belongs to the Agents, because the `download_url` in a package offer is a path the Client resolves against *its own* endpoint. It sits outside the Operator plane's `[rest.auth]` and behind the same TLS handshake as `/v1/opamp`, so a downloading Client presents its client certificate. A certificate from the client CA is required; a bootstrap certificate is answered `401`. A host is served only the artifact offered to one of its own Agents, and every other request is answered `404` ([A host fetches only what its Agents are offered](#a-host-fetches-only-what-its-agents-are-offered)). |

The Agent plane asks every peer for a client certificate in the TLS handshake. A peer without one
fails the handshake and reaches no route (see
[Mutual TLS](#mutual-tls-proving-who-is-on-the-connection)).

**The Operator plane** — `[rest] listen`, `127.0.0.1:4321` by default:

| Path | What it is |
|---|---|
| `/api/v1/…` | The REST API. |
| `/api/v1/openapi.json` | The OpenAPI document — the contract to generate a client from. It describes this plane, so the artifact download above is not in it. |
| `/api/v1/docs` | Interactive API documentation (Redoc, vendored and served from this origin, so it works offline). |
| `/` | The bundled UI: one embedded page, no frontend toolchain. It is deliberately rudimentary — the API is the product. |

The Agent plane has no credential; the Operator plane has one of its own,
[`[rest.auth]`](#the-operator-plane-restauth). This port carries the authority to reconfigure and
re-package the whole fleet, so its default address is the **loopback**. There `[rest.auth]` is
optional, and the address is what protects the plane. Reach it from another host through an SSH
tunnel (`ssh -L 4321:127.0.0.1:4321 <server-host>`), or publish it deliberately with
`[rest] listen = "0.0.0.0:4321"`. Off the loopback `[rest.auth]` is required, and a Server without
it is refused at startup with a message naming both keys.

Each plane holds a bounded number of connections: `max_connections` for the Agent plane and
`[rest] max_connections` for the Operator plane. A connection past the cap is closed on accept,
before the TLS handshake, and the connections already held keep working. See
[Bounds on every listener](#bounds-on-every-listener).

## Configuration reference

The full annotated example is [`config/server.toml`](../../config/server.toml). Every key is shown
below with its default. `[tls]` with its `client_ca_file` is required; every other key is
optional. An unknown key fails startup rather than being ignored.

### Top level

| Key | Default | Meaning |
|---|---|---|
| `listen` | `"127.0.0.1:4320"` | The **Agent plane**, as `address:port`: the OpAMP endpoint and the package downloads. `4320` is the protocol's default port. Serve the fleet with `"0.0.0.0:4320"`. |
| `max_connections` | `10000` | The connections the Agent plane holds at once. A connection past it is closed on accept. Raise it together with the process's file-descriptor limit for a larger fleet. `0` is refused at startup. |
| `config_dir` | `"fleet-configs"` | Where Configurations are persisted — one JSON file per Configuration, named after it. Written atomically; read back at startup. |
| `packages_dir` | `"fleet-packages"` | Where packages are persisted — one artifact plus metadata each. |
| `max_message_size_bytes` | `67108864` (64 MiB) | The largest OpAMP message accepted or sent, in either direction and on either transport. The protocol requires a limit and recommends this value; a fleet of status reports needs far less. An oversized HTTP request is answered `413`, an oversized WebSocket message closes the connection with `1009`. A request or message that has begun and delivers less than 64 KiB in a minute is answered `408`, or closes its connection with `1008`. |
| `max_package_size_bytes` | `1073741824` (1 GiB) | The largest artifact the package-upload route accepts. A package is a program, not a message — an `otelcol-contrib` binary is a few hundred megabytes — so this bound is far larger, and it applies to that one route. The upload has no deadline, but like every request body it must deliver 64 KiB in each minute once it has begun, or it is answered `408`. |
| `max_total_package_bytes` | `17179869184` (16 GiB) | The total size of all stored artifacts before a new upload is refused `507`. Where `max_package_size_bytes` bounds one artifact, this bounds the whole store, so no caller fills the disk by uploading many artifacts under distinct names. `0` is refused at startup. |
| `max_agents` | `100000` | The most Agent records the fleet holds at once. A report bearing a **new** `instance_uid` past this ceiling is answered `Unavailable` rather than admitted, so an admitted peer minting fresh self-asserted UIDs cannot exhaust memory and disk; Agents already known keep reporting. The defence against an anonymous flood is [admission](#authentication) by client certificate; this is the backstop behind it. `0` is refused at startup. |
| `stale_after_secs` | `90` | How long an Agent that declares `ReportsHeartbeat` may be silent before the fleet view marks it **stale**. Ignored when `[connection_offer]` names a heartbeat interval — then the budget is three of those. Only heartbeating Agents can go stale: one that promised no periodic report is never late. |
| `advertised_url` | unset | The absolute base URL advertised for package downloads. Leave it unset in the ordinary case: the Client then resolves the offered path against its own OpAMP endpoint, which is exactly where the download is served. Set it only when downloads must go through a different host, such as a mirror. |

### `[rest]`

The Operator plane's listener. Absent means the default.

```toml
[rest]
listen = "127.0.0.1:4321"   # "0.0.0.0:4321" publishes the plane, and then needs [rest.auth]
max_connections = 256
```

| Key | Default | Meaning |
|---|---|---|
| `listen` | `"127.0.0.1:4321"` | Where the REST API, the API docs, and the UI are served, over the TLS of `[tls]`. It must differ from `listen` above — two equal addresses are refused at startup by name, rather than surfacing later as *address already in use*. An address other than `127.0.0.1` or `::1` requires `[rest.auth]`. |
| `max_connections` | `256` | The connections the Operator plane holds at once. `0` is refused at startup. |

#### `[rest.auth]`

Basic authentication over that whole plane — see
[Authentication](#the-operator-plane-restauth). Optional on a loopback `listen`, required on any
other.

```toml
[rest.auth.basic_users]
fleet-admin = "$argon2id$v=19$m=19456,t=2,p=1$…"
```

| Key | Default | Meaning |
|---|---|---|
| `basic_users` | *(empty)* | Accepted Basic credentials, `user = "<Argon2id hash>"` — see [Credentials are kept as hashes](#credentials-are-kept-as-hashes). A section without one, or an entry that is not such a hash, fails startup. |

### `[tls]`

Required. **Both listeners** serve HTTPS and WSS in TLS 1.3, with this certificate and key. A
Server without the section is refused at startup with a message naming it. `client_ca_file` is
required too. It belongs to the Agent plane alone, which asks every peer for a client certificate
in the handshake (see [Mutual TLS](#mutual-tls-proving-who-is-on-the-connection)).

```toml
[tls]
cert_file = "cert.pem"
key_file = "key.pem"
client_ca_file = "client-ca.pem"   # the CA every Agent's certificate chains to
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
validity_days = 30
```

Without it, every host holds a certificate an operator provisioned, and such a certificate is
served an uploaded artifact only when it names its host as a URI SAN,
`urn:opamp-fleet:host:<id>`. A certificate that names no host fetches nothing from the download
route; referenced artifacts are unaffected. A Server started without `[client_ca]` says so once at
startup.

### `[enrolment]`

Optional. Present lets a new host enrol with a bootstrap certificate from this CA — see
[Enrolment](#enrolment-a-new-host-approved-by-an-operator). It needs `[client_ca]`, which signs
the approved request.

```toml
[enrolment]
bootstrap_ca_file = "bootstrap-ca.pem"
```

### `[admission_throttle]`

Optional; absent means the defaults shown. See
[Repeated failures are throttled](#repeated-failures-are-throttled).

```toml
[admission_throttle]
max_failures = 10
window_secs = 60
backoff_secs = 300
```

### `[agent_rate_limit]`

Optional; absent means the defaults shown. See
[How often a member may be heard](#how-often-a-member-may-be-heard).

```toml
[agent_rate_limit]
messages_per_sec = 10
burst = 300
gateway_messages_per_sec = 500
gateway_burst = 10000
```

### `[connection_offer]`

See [Moving the fleet](#moving-the-fleet-connection-settings). Either key or both is valid, but
not an empty section. The `endpoint` is `wss://` or `https://`; `ws://` or `http://` only to
`127.0.0.1` or `::1`. The section carries no credential: `bearer_token_file`, `username`,
`password_file`, `bearer_token` and `password` are each refused at startup by name.

```toml
[connection_offer]
heartbeat_interval_secs = 30
endpoint = "wss://fleet.example:4320/v1/opamp"
```

## Mutual TLS: proving who is on the connection

`[tls] client_ca_file` is required, and the Agent plane asks every peer for a client certificate
**in the TLS handshake**
([ADR-0026](../adr/0026-admission-by-a-client-certificate-alone.md)):

```toml
[tls]
cert_file = "cert.pem"
key_file = "key.pem"
client_ca_file = "client-ca.pem"
```

The handshake accepts a certificate that chains to `client_ca_file`, and, while `[enrolment]` is
configured, one that chains to the bootstrap CA. A peer that presents no certificate, or one that
chains to neither, fails the handshake and reaches no route. That covers the package download as
much as `/v1/opamp`. The Operator plane asks for no client certificate; a browser reaches it with
the password of [`[rest.auth]`](#the-operator-plane-restauth).

**The certificate is the whole of admission.** A connection is a member's when its certificate was
issued directly by the client CA, is valid, and is not
[revoked](#revocation-withdrawing-a-certificate); any other certificate that passes the handshake
makes an enrolment connection. No configuration admits a peer without a certificate. `/v1/opamp`
and the download route read no `Authorization` header: one a Client sends is ignored, never
refused, and no refusal carries a `WWW-Authenticate` challenge.

A certificate proves **fleet membership, not identity**. The Server does not match its subject
against an Agent's `instance_uid`: the Server itself may re-key an Agent at any time
(`AgentIdentification`), and a certificate that a re-key invalidates is an outage of your own making.

### Issuing certificates: the CSR flow

Add a `[client_ca]` section and the Server becomes a local CA:

```toml
[client_ca]
cert_file = "client-ca.pem"
key_file = "client-ca-key.pem"
validity_days = 30
```

Use a **separate** CA, not the listener's certificate and key: a CA private key stored where the
server certificate lives means compromising the Server mints fleet members at will. Then point
`[tls] client_ca_file` at that CA's certificate, so the certificates it issues are the ones the
listener accepts.

With the section present the Server declares `AcceptsConnectionSettingsRequest`. A Client whose
certificate is two thirds through its validity generates a key **that never leaves its host**,
sends a signing request, and receives the certificate as an ordinary connection-settings offer. It
proves the new certificate by connecting with it before it replaces the one in force. A request
from a peer whose certificate chains to the client CA is signed at once: renewal is automatic. A
request from a host that holds only a bootstrap certificate waits for an operator, as the next
section describes. The Server signs a client-authentication certificate only, and drops any
alternative names the request asks for.

A request that does not parse, or one arriving at a Server with no `[client_ca]`, is answered with
the protocol's `BadRequest` error response.

### The first certificate on a host

A host needs a certificate before it can connect at all, and it gets one in one of two ways:

1. **An operator provisions it.** Sign a client certificate with the client CA and write it, with
   its key, into the Client's `[tls] cert_file` and `key_file`. The Client connects with it at
   once and renews it through the CSR flow.
2. **The host enrols.** It is given a bootstrap certificate, and an operator approves its request —
   see [Enrolment](#enrolment-a-new-host-approved-by-an-operator).

**Behind a Gateway, the Gateway's handshake admits the Agents it carries.** Mutual TLS is per hop:
the Gateway checks the Agents connecting to it against the client CA and the Server's revocation
list, and presents its own certificate to the Server. It forwards no `Authorization` header. A
Gateway trusts the client CA and never the bootstrap CA, so a host behind one enrols by connecting to the Server once, or is
provisioned a certificate by an operator.

**A certificate names its host.** The Server puts `urn:opamp-fleet:host:<id>` into every
certificate it signs: a host is minted when an operator approves an enrolment, a certificate an
operator provisioned is given one on its first renewal, and every renewal keeps it — the Client
proves with its current key which certificate it renews, through a Gateway too. An Agent belongs
to the host that first reported it: a connection with another host's certificate that reports
for it is given an `instance_uid` of its own instead. A host holds at most three valid
certificates. A Gateway carries other hosts' Agents, so mark it once its certificate names a
host. **A Gateway admits nobody until its host is marked:** only a marked host is handed the
revocation list a Gateway refuses by (see
[Revocation](#revocation-withdrawing-a-certificate)):

```console
$ curl --cacert ca.pem https://127.0.0.1:4321/api/v1/hosts
[{"host":"0192…","gateway":false,"instance_uids":["0192…","0193…"],"certificates":1}]
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' -d '{"gateway": true}' \
       https://127.0.0.1:4321/api/v1/hosts/0192…/gateway
```

A Gateway's certificate speaks for any Agent; behind it, fleet membership is all the Server
proves, and the Gateway's certificate may fetch the artifact offered to any Agent. A third-party
client that sends no renewal proof renews the certificate it presents.

**A certificate lives 30 days by default** and is renewed at two thirds of that; a host is ejected
sooner by revoking its certificate ([Revocation](#revocation-withdrawing-a-certificate)).
An expired certificate locks a host out: a Client switched off longer than its validity enrols again
with a bootstrap certificate, or is given a new certificate by an operator.

## Enrolment: a new host, approved by an operator

Enrolment is how a fresh host obtains its first certificate without an operator signing one by
hand. It is armed by `[enrolment]`, and it needs `[client_ca]`, which signs the approved request:

```toml
[enrolment]
bootstrap_ca_file = "bootstrap-ca.pem"
```

The **bootstrap CA** is a CA of its own, never the client CA; a bootstrap CA that shares a
certificate with `client_ca_file` is refused at startup. A bootstrap certificate opens nothing by
itself. It needs an open enrolment window and an operator's approval, so one bootstrap certificate may serve the whole fleet. Keep its validity short.
Without `[enrolment]` no bootstrap certificate passes the handshake.

**The window is closed by default.** Open it for as long as you are enrolling hosts, from 1 to
86400 seconds:

```console
$ curl --cacert ca.pem -X POST -H 'Content-Type: application/json' \
       -d '{"open_for_secs": 3600}' https://127.0.0.1:4321/api/v1/enrolment/window
{"open":true,"until_ms":1790000000000}
$ curl --cacert ca.pem https://127.0.0.1:4321/api/v1/enrolment/window
$ curl --cacert ca.pem -X DELETE https://127.0.0.1:4321/api/v1/enrolment/window
```

`POST` opens the window, or moves its end to `open_for_secs` from now. `GET` says whether it is
open and until when. `DELETE` closes it early. The window and its pending requests live in memory
only, so a Server restart closes it. When it closes, every enrolling connection is closed and every
pending request expires.

**While it is open, an enrolling host may send a certificate request and nothing else.** The
Server creates no Agent record for it and offers it nothing but the issued certificate. The request
waits in a queue of at most 1024. The Client keeps its pending key and re-sends the same request
until it is answered.

**Match the request to the host by its fingerprint.** The enrolling Client logs the SHA-256
fingerprint of the key it generated:

```text
INFO certificate request generated; an enrolling host waits for an operator's approval key_fingerprint=3f9a…
```

The Server lists each pending request with that fingerprint as its `id`, beside its arrival time,
the peer address, the subject it asks for, and the bootstrap certificate's subject and fingerprint.
The Client sends its request as soon as the Server's first answer says it signs certificates,
so the request is listed moments after the host connects:

```console
$ curl --cacert ca.pem https://127.0.0.1:4321/api/v1/enrolments
[{"id":"3f9a…","arrived_ms":1789996400000,"peer":"10.0.4.17","subject":"CN=host-01",
  "key_fingerprint":"3f9a…","bootstrap_subject":"CN=fleet bootstrap","bootstrap_fingerprint":"…"}]
$ curl --cacert ca.pem -X POST https://127.0.0.1:4321/api/v1/enrolments/3f9a…/approve
$ curl --cacert ca.pem -X POST https://127.0.0.1:4321/api/v1/enrolments/3f9a…/reject
```

Approving signs the request with `[client_ca]` and offers the certificate on that host's
connection. The host stores it as `client-cert.pem` in its state directory and reconnects with it
as a member; from then on it renews by itself. Rejecting answers the request `BadRequest` and
closes the connection. An unknown or expired id is answered `404`, and every enrolment route
answers `404` while `[enrolment]` is not configured. A bootstrap certificate that arrives while
the window is closed is answered `503`, and does not count toward the throttle.

A request whose subject or SANs name an `instance_uid` other than the host's own is answered
`BadRequest` and never enters the queue. The same holds for a renewal.

The bootstrap certificate stays in the host's `supervisor.toml`, unused once the issued pair is
stored. Approve only what you can match to a host you are setting up.

## Revocation: withdrawing a certificate

A certificate stays valid until it expires. To shut a host out sooner, revoke its certificate. The Server refuses it from then on, on both
transports and on the package download, and closes at once every WebSocket session it admitted,
with close code `1008`. No other session is touched, and no restart is needed. The list lives
under `config_dir` and survives a restart.

Every certificate the client CA signs is in a register, with the certificate the host presented
when it renewed. Its `key_fingerprint` is the one the request was listed and approved by:

```console
$ curl --cacert ca.pem https://127.0.0.1:4321/api/v1/certificates
[{"authority":"client","issuer":"CN=fleet client CA","serial":"5c0f…","subject":"CN=host-01",
  "key_fingerprint":"…","not_after_ms":1797772400000,"instance_uid":"0192…",
  "issued_ms":1789996400000,"predecessor":{"authority":"client","serial":"41ab…"},
  "host":"0192…"}]
```

Revoke a certificate by the CA that issued it — `client` for the client CA, `bootstrap` for the
bootstrap CA of `[enrolment]` — and its serial. A revocation reaches every renewal of that
certificate too, so revoking the one a host enrolled with is enough even after it renewed, and it
stays in force until the last of those renewals has expired. `openssl x509 -noout -serial` prints
the serial of a certificate you hold; colons, case and leading zeros do not matter. A renewal
descends from the certificate its renewal proof names, through a Gateway too; a client that sends
no proof renews from the certificate its connection presented — behind a Gateway the Gateway's, so
revoking the Gateway revokes those renewals as well, and those Agents enrol again. A revoked
certificate's proof renews nothing.

```console
$ curl --cacert ca.pem -X POST -H 'Content-Type: application/json' \
       -d '{"certificate": {"authority": "client", "serial": "5c0f…"}}' \
       https://127.0.0.1:4321/api/v1/revocations
{"id":"9d2e…","kind":"certificate","revoked_ms":1790000000000,
 "certificate":{"authority":"client","serial":"5c0f…"}}
```

There is no credential to revoke. A request naming `"credential"` is answered `400`, naming the
field and saying that the Agent plane admits by client certificate alone. A credential entry
`revocations.json` holds is dropped when the list is loaded, with one log line giving the number
dropped, and the list is written back without it.

`GET /api/v1/revocations` lists every entry, and `DELETE /api/v1/revocations/<id>` lifts one; the
next connection is admitted again. The list holds at most 100 000 entries; an entry for a
certificate the register held is dropped once neither that certificate nor any renewal of it is
valid. The register holds at most 100 000 certificates and at most 10 000 in one renewal chain,
and keeps the last 1 000 places for enrolments.

Every WebSocket session also ends when the certificate that admitted it expires, with `1008` and
the reason `certificate expired`. A Client renews at two thirds of the life and reconnects with
the new certificate, so a healthy fleet never sees this.

Behind a Gateway the Server sees the Gateway's certificate, not the Agent's, so the Gateway refuses
for it. Every host marked as a Gateway fetches the revoked certificates of the client CA from
`GET /v1/gateway/revocations` on the Agent plane every 30 seconds, renewals already resolved, and
refuses a downstream peer whose certificate is on it with `401`, closing its sessions with `1008`
and the reason `revoked`. A revocation therefore reaches a gatewayed Agent within about 30
seconds. A Gateway that has held no list younger than 300 seconds — its Server unreachable, or its
host not marked — answers every downstream peer `503` and closes their sessions with the reason
`revocation list stale`. Revoking the Gateway's own certificate ends every Agent it carries.

## The audit record

Every security decision leaves one line in `config_dir/audit/`: each admission and refusal on the
Agent plane, each refused operator sign-in, each enrolment request, approval and rejection, each
certificate issued, each revocation and the session it ended, each request for the Gateways'
revocation list from a host not marked as a Gateway, each download a host may not fetch, each
operator act with the operator's name, and each package an Agent reports installed or failed. A
plain-HTTP Agent's admission is recorded once an hour per address and certificate, a WebSocket
session every time. No line holds a secret in any form — an `Authorization` value is never
written, not even as a hash, whether an operator presented it or an Agent sent it unasked — nor a
key or a CSR body.

```console
$ tail -n1 /var/lib/opamp-fleet-server/audit/audit-1.jsonl
{"event":"revocation.revoked","id":"9d2e…","kind":"certificate","authority":"client",
 "serial":"5c0f…","outcome":"revoked","prev":"41b7…","seq":5113,"time":"2026-10-04T09:12:44Z"}
$ server audit-verify /var/lib/opamp-fleet-server/audit
5113 entries in 1 files, the chain holds
```

Each line carries the SHA-256 of the line before it, so a line edited or removed afterwards breaks
the chain where it happened; `audit-verify` names the first entry that does not follow. Against an
intruder on the Server host that only helps with a copy taken before, so ship the directory off the
host. The record also goes to the Server's log under the target `audit`.

The Server never takes a decision it cannot record: when the record cannot be written — a full
disk, a failed device — it admits no Agent and runs no operator act, answering `503`, until it
can. A certificate request in that time is answered `Unavailable` with a time to retry, and the
Agent asks again. Refusals past ten a second from one address are counted into one line, not dropped.
`[audit] max_file_bytes` (64 MiB) and `keep_files` (16) bound the space it takes.

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

The examples on this page call the Operator plane on the loopback, over TLS. `--cacert ca.pem`
names the CA that signed the Server's certificate. Off the loopback, add the
[`[rest.auth]`](#the-operator-plane-restauth) credential with `-u`.

```console
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' \
       -d '{"service_name": "otelcol-contrib", "selector": {"os.type": "linux", "env": "prod"}, "body": "receivers: {}"}' \
       https://127.0.0.1:4321/api/v1/configurations/linux-prod
$ curl --cacert ca.pem -X POST https://127.0.0.1:4321/api/v1/configurations/linux-prod/rollout
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
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' \
       -d '{"body": "rules: []", "role": "supplementary"}' \
       https://127.0.0.1:4321/api/v1/configurations/ruleset
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

## Packages and Deployments: distributing software

Package delivery is armed by `packages_dir`; the Server declares the capability only while the
store holds something. Two objects share that directory, and the split is the whole of the model:

- A **Package** is *what* an Agent type runs at a version. Its identity is the **Agent type and
  the version**, and it holds one entry per platform — the Linux build, the macOS builds, the
  Windows build. Nothing else: no name of its own, no Selector, no signature. It aims at nobody
  and is never rolled out.
- A **Deployment** is *where that goes*. A name, the **channel** it aims at, one Package per Agent
  type, and the **signature** of each artifact. It is the only thing that is rolled out.

An Agent belongs to **at most one Deployment**. Two matching the same Agent is a conflict: that
Agent is offered nothing new, and the fleet view says so on its row in `package_conflict`. There
is no most-specific-wins and no newest-wins — a rule that decides which artifact a host gets by
comparing every stored object against every other is a rule nobody can evaluate by looking at
anything.

### Channels are a partition, not a default

**A Selector is equality and cannot say "not".** "Everyone except the canary hosts" is not a
writable Selector, so disjoint channels come from *membership*: an attribute every Agent carries.

**The Server prescribes no key.** There is no reserved word and no special handling anywhere — a
Selector is equality over whatever the Agent reports, and the key is one you invent. What follows
are examples, not a schema.

```console
# at provisioning, in the host's supervisor.toml
[attributes]
channel = "stable"

# or from here, without touching the host
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' -d '{"labels": {"channel": "beta"}}' \
       https://127.0.0.1:4321/api/v1/agents/<uid>/labels
```

Which key you pick is a decision about *what the partition means*, and three shapes cover most
fleets:

| key | what it says about the host | how a release moves |
|---|---|---|
| `channel` | which stream of versions it follows — `stable`, `beta`, `nightly` | you change **what the channel carries**; the hosts stay where they are |
| `region` | where it runs — `eu-central`, `us-east` | a release follows the sun, one region at a time; or a version stays pinned where a jurisdiction requires it |
| `tenant` | whose it is — `acme`, `globex` | one customer's software moves on that customer's schedule, independently of everyone else's |

`channel` is the one to reach for when the partition is about *release risk*, which is the ordinary
case: a host subscribes to `stable` and stays there for years, while the Deployment named `stable`
is given one version after another. `region` and `tenant` describe the host instead of its
appetite, and both compose — a fleet can carry all three and aim a Deployment at
`{"tenant": "acme", "channel": "stable"}`, which is two equality pairs and needs nothing new.

What they have in common is the reason they work: each names a **property of the host**. A key that
named the Deployment instead — `deployment = "contrib"` — would put the same fact in two places, so
re-aiming a channel would mean editing every host in it. The Selector exists to avoid exactly that.

**A Deployment's Selector may not be empty** — an empty one matches every Agent, so it would
collide with every other channel the moment a second one exists, and it is what a forgotten field
looks like. The price is stated plainly: **there is no "roll out to everyone" any more.** A
fleet-wide delivery needs every Agent to carry the same value, and a freshly enrolled host
belongs to no Deployment until it is labelled. That host shows on the fleet view with no
deployment, which is the ordinary state after an enrolment and not a fault.

### Create the Package, then upload its entries

The identity is the path. The Agent type is compared **raw** against the `service.name` the Agents
report — there is no canonical set of Agent types, so spell it exactly as they do. The create body
has no writable field; `{}` is the whole of it.

```console
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' -d '{}' \
       https://127.0.0.1:4321/api/v1/packages/otelcol-contrib/0.109.0
$ curl --cacert ca.pem -X PUT --data-binary @otelcol-contrib_0.109.0_linux_amd64.tar.gz \
       "https://127.0.0.1:4321/api/v1/packages/otelcol-contrib/0.109.0/entries/linux/amd64"
```

Platform spellings are accepted and stored canonically, so the tokens off an upstream release's
file name work as they are: `macos` and `osx` mean `darwin`, `x86_64` and `x64` mean `amd64`,
`aarch64` means `arm64`. An `os`/`arch` this Server has never heard of is stored as given rather
than refused — the fleet may run a system nobody here anticipated.

**Or reference an artifact hosted elsewhere**. The Server stores the address and your SHA-256,
offers them verbatim, and never downloads the artifact — so the hash and the signature are the whole
of the protection. The `url` is `https://`; `http://` is accepted only when its host is
`127.0.0.1` or `[::1]`, and any other is refused `400` naming the rule
([ADR-0019](../adr/0019-the-package-store-references-artifacts-only-over-tls-beyond-the-loopback.md)).
So neither the artifact nor the headers an Agent sends for it cross a network in plaintext:

```console
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' \
       -d '{"url": "https://mirror.example/otelcol.tar.gz", "sha256": "…"}' \
       https://127.0.0.1:4321/api/v1/packages/otelcol-contrib/0.109.0/entries/linux/amd64/source
```

The URL is probed once, with one `HEAD` over TLS 1.3 that follows no redirect, to catch a typo
while you are still looking at the screen. A definitive refusal from the source (a `4xx`) fails the
request; a source this Server cannot reach does not, because the Server is not in the download path
and its reachability says nothing about the Agents'. A private source can be given headers to send.
A Client fetches from such a source only when its `[packages] allowed_sources` lists it (see
[the Client](client.md#where-a-download-may-come-from)).

The store keeps its Deployments in `<packages_dir>/deployments/`; every *other* entry there is a
Package directory, and one this Server does not recognise **fails the start naming the path**
rather than being skipped — a store left over from an older layout would otherwise open
successfully and empty, which reads as "nothing uploaded yet".

A Package that no Deployment holds reaches nobody, and the package list says so — `in no
deployment` in the UI, an empty `deployments` array in the API. That is the state a Selector
matching no one used to be.

### Put it in a channel, sign it there, roll it out

```console
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' -d '{"selector": {"channel": "beta"}}' \
       https://127.0.0.1:4321/api/v1/deployments/beta
$ curl --cacert ca.pem -X PUT https://127.0.0.1:4321/api/v1/deployments/canary/packages/otelcol-contrib/0.109.0
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' -d "{\"signature\": \"$sig\"}" \
       https://127.0.0.1:4321/api/v1/deployments/canary/signatures/otelcol-contrib/0.109.0/linux/amd64
$ curl --cacert ca.pem -X POST https://127.0.0.1:4321/api/v1/deployments/canary/rollout
```

**Nothing is offered before that last line.** Everything above stores; the **rollout act** is the
only thing that distributes, the same act Configurations have. Five platforms' artifacts can be
uploaded, put in a channel, signed and then released together, and the window in which a
half-described release is already reaching the fleet does not exist. An Agent that enrols — or is
labelled into the channel — later waits, marked pending on its fleet row, for an act of its own.

**A channel holds at most one Package per Agent type**, because an Agent has one binary to replace. A
second is refused `409` naming the one in the way; `?replace=true` is how you say you mean to swap
the version the channel runs. Adding the same one again is the same request arriving twice and
succeeds.

**The signature belongs to the Deployment**, not to the artifact: what an operator signs off on is
a release to a set of machines, so the same Package in two channels is signed in each. **The Server
never offers an unsigned entry**
([ADR-0021](../adr/0021-packages-and-deployments-that-sign-every-package.md)). An entry its
Deployment holds no signature for is no candidate, and the fleet view says the signature is
missing. A rollout of a Deployment that lacks a signature for any entry is refused `409`, naming
each Package and the platforms it is unsigned for, and nothing is released. The deployment view
reports which platforms are covered (`signed_platforms`), and the UI marks an unsigned package
`⚠`. Supplying a signature on the artifact upload instead answers `400` naming the route that
takes it.

**While a Package is rolled out to at least one Agent its entries are frozen** (the fleet is
installing those bytes; uploads answer `409`); ship a change as the next version, which is a new
Package. On the channel, the same rule covers exactly what a standing offer travels with: the
**signature** of a Package it released, and its hold on that Package — removing it would take the
signature with it and turn a signed rollout unsigned for anyone still downloading. **Swapping the
version a channel holds is not frozen**, because that is how a rollout proceeds: the hosts already
released keep what they have, and the new version shows as waiting until the next press. Deleting a Package withdraws it from every Agent it was rolled out to, and **nothing is
uninstalled** — an Agent keeps running what it installed. Deleting a Deployment does the same.

### The canary shape

Two channels, disjoint by label, each holding its own version:

```console
$ curl --cacert ca.pem -X PUT -d '{"selector": {"channel": "stable"}}' … /api/v1/deployments/stable
$ curl --cacert ca.pem -X PUT -d '{"selector": {"channel": "beta"}}'   … /api/v1/deployments/beta
```

There are two ways to move, and with a `channel` key the second is the ordinary one:

- **Move a host** — change its label from `stable` to `beta`, and it takes what the beta channel holds.
  This is how a single machine is tried first.
- **Move the release** — give the `stable` channel the version `beta` already carries and press again.
  The hosts never move; the channel they subscribe to is handed the next version. This is how a
  rollout *finishes*.

**Widening the beta channel instead is not how it ends** — it would make both channels claim the same
hosts, which is a conflict, not a rollout.

### Fit runs before aim, and it is mandatory

A Package built for another **Agent type** is not a candidate at all: its type is matched against
the `service.name` the Agent reports, so a Promtail artifact never reaches a Collector even from a
channel that claims it. Then an entry must exist for the **platform** the Agent reports. Only what
survives both is what its channel can release. **An Agent that reports no `service.name`, `os.type`,
or `host.arch` is offered nothing** — there is no artifact that can be known to be meant for it or
to run on it, and guessing is how a fleet-wide outage starts.

### Reading the counts

A Deployment carries three, because zero has three meanings and only the first is a mistake to go
hunting for:

| count | zero means |
|---|---|
| `claiming_agents` | the channel aims at nobody — a Selector naming an attribute nobody reports, or a value nobody carries |
| `targeted_agents` | everyone it claims already runs what it holds; nothing to do |
| `conflicting_agents` | *non*-zero is the one to act on: another channel claims those Agents too |

On the other side, each Agent's row says which channel claims it (`deployment`) as well as which one
released what it runs (`assigned_deployment`). The two differ on purpose, and reading them together
tells apart four states that would otherwise be the same empty cell — because the next move differs
in each:

| the row shows | what it means | what to do |
|---|---|---|
| no `deployment`, no conflict | no channel's Selector matches this host | label it, or give it a `channel` attribute |
| a `deployment`, nothing assigned, nothing pending | the channel holds nothing this Agent can take — no Package for its type, none for its platform, or an entry without a signature | upload the entry, put the right Package in the channel, or sign the entry |
| a `deployment` and something pending | it is waiting for a rollout act | press it |
| `package_conflict` | two channels claim it | narrow one Selector |

**A rollout act never moves an Agent backwards.** Since
[ADR-0014](../adr/0014-rollout-and-what-reaches-an-agent.md) the version an Agent reports
installed is part of matching: a Package reaches it only if the Package's version is **greater**,
compared as SemVer (major, minor, patch, then the pre-release rules). Equal is not greater — a
Package an Agent already runs reaches it with nothing — and a reported version nothing can order
is refused rather than guessed at. The bulk act skips such Agents; the per-Agent act answers `409`
and says which version the Agent reports.

**An Agent that reports no version for the package is held against the version it reports
*running*** — its `service.version`
([ADR-0014](../adr/0014-rollout-and-what-reaches-an-agent.md)).
That is what makes the rule reach a Client installed from a `.deb`, an `.rpm` or an MSI, which has
installed no package and has none to report: no Client is offered the version it already runs, and
none is moved backwards. A `service.version` nothing can order (`1.19`, `24.04.1`) simply says
nothing, so an Agent whose program numbers itself its own way stays reachable.

**Where an Agent reports both, what it *runs* decides**
([ADR-0014](../adr/0014-rollout-and-what-reaches-an-agent.md) points 2 and 3). The Package must be greater
than the `service.version` the Agent reports, and the package status is not read beside it —
neither to admit a Package the running version refuses, nor to refuse one it admits. A statement
about the present outranks a record of an install, which outlives the binary it describes. So a
claim the Agent's own program denies never holds the package back, in either direction: a Client
reporting `supervisor 0.4.2` installed while reporting that it runs 0.4.0 — a state directory that
outlived its binary, a self-update that staged and did not take effect — is offered 0.4.1, where
the claim used to refuse it as a downgrade and strand the host for good. The `409` names the
version that decided and says that the claim was not consulted.

**What this costs, and whom it touches.** Where a program reports a version *above* the Package that
carries it, no Package below that number reaches it any more, and where it reports one below, a
Package between the two can move its package backwards — a Collector calling itself `0.98.0` under
an `otelcol` Package at `2.0.0` can be assigned a `1.5.0`. Nothing moves on its own: a rollout is
an explicit act and you see the version you press. This reaches only Agents that report a
`service.version` of their own — the Client itself, and an OpAMP-aware Managed Process such as a
Collector carrying `opampextension`. An Icinga 2 or a GLPI Agent reports none, so its Packages
keep being matched on the package status alone. **Number a Package the way the program it carries
numbers itself**, and neither case arises.

Versions are still kept side by side — a new version is a new Package, and the older artifact
stays in the store — but **taking a bad version back is not a rollout**:

- on the host, the version a package superseded is retained for `retain_previous_secs` and put
  back when the new one fails its health gate (see the Client manual, *Package updates: rollback
  and retention*);
- a Client that will not stay up after its own self-update goes back by itself;
- fleet-wide, what is left is publishing the older content **as a new, greater version** — which
  is honest about the fact that the fleet moves forward, and is the only thing the matching rule
  will carry.

**Deleting.** `DELETE /api/v1/packages/otelcol-contrib/0.109.0` removes the Package — its entries,
artifacts, metadata, and every per-Agent assignment that referenced it; the entry route with a
platform removes just that one entry. `DELETE` on a Deployment is refused (`409`) while an Agent's
assignment still names it, since the offer travels with that channel's signatures: roll those
Agents out through another Deployment, or delete the Package, first. Nothing is uninstalled by any
of them.

**Building and signing**. The helper that ships with the Client writes the
artifact, hashes it, and signs it:

```console
$ opamp-package-sign pack --out promtail-3.0.0.tar.gz ./promtail   # prints the sha256
$ opamp-package-sign keygen --out fleet-signing.pk8                # prints the public key
$ sig=$(opamp-package-sign sign --key fleet-signing.pk8 --agent-type promtail --version 3.0.0 \
      promtail-3.0.0.tar.gz)
```

`pack` writes `.tar.gz` or an AES-256-encrypted `.7z`, and names the member the way the receiving
Supervisor will look for it. A Client opens those two and `.zip`; an artifact that is none of the
three is taken to *be* the program. [The rollout walkthrough](rollout.md) puts the whole sequence
together.

The download route sits on the **Agent plane**, outside `[rest.auth]` and behind the same
TLS handshake as `/v1/opamp`. A downloading Client presents its client certificate, and it presents
it to this Server's own origin and to no other host. Guarding the Operator plane therefore cannot
break a rollout. The content hash and the signature are what protect an installed binary.

`keygen` prints the public key as hex — that value is the Client's `[packages] verification_key`.
A Client without it takes no package at all, its own update included, and says so at startup
([ADR-0018](../adr/0018-signed-package-delivery-from-allowed-sources.md)). Give every Client the
key.

### A host fetches only what its Agents are offered

The download route serves an uploaded artifact only to a host whose certificate speaks for an
Agent that is offered that artifact — its Agent type, version and Platform — by a rollout. A host
speaks for the Agents that reported with its certificate, and a host marked as a Gateway for any
Agent. An artifact stays offered until the Agent's assignment changes, so an Agent that echoed its
offer and then failed to install can fetch it again. A version saved in a Deployment but not yet
released by a rollout is offered to no one and cannot be fetched by anyone.

Every other request for an artifact is answered `404` with the same status, body and headers as
a request for an artifact the store does not hold: an artifact released to another host's Agents,
one released to nobody yet, an entry that is only a reference, and any request over a certificate
that names no host. A malformed type, version or Platform is still answered `400`, and a
certificate that is not a member's still `401`. Each refused fetch leaves a `download.refused`
entry in [the audit record](#the-audit-record) with `check` `not offered`, the host, the
certificate's serial and the type, version and Platform asked for. The certificate is admitted
before the route decides what it may fetch, so a refused fetch leaves both a `download.admitted`
and a `download.refused` entry: a `download.admitted` says the certificate was let in, not that
an artifact was served. Such a refusal is no failed admission, so it does not count toward
[the throttle](#repeated-failures-are-throttled). Each download, served or refused, takes a token
from the host's bucket ([How often a member may be heard](#how-often-a-member-may-be-heard)); for
a Gateway, from its aggregate bucket.

A `404` for an Agent that was offered an artifact therefore means one of three things: its host's
certificate names no host (provision it with `urn:opamp-fleet:host:<id>`, see
[`[client_ca]`](#client_ca)), the Agent now reports another Agent type than the Package was built
for, or a later rollout changed what it is offered.

A Client behind a Gateway receives uploaded artifacts through the Gateway
([ADR-0033](../adr/0033-a-host-fetches-only-what-its-agents-are-offered-and-a-gateway-caches-it-for-the-hosts-behind-it.md)).
Its offer names the path on this route, which it resolves against its own endpoint, the Gateway.
The Gateway fetches each such artifact from this Server once, with its own certificate, as soon
as it relays an offer of it, and serves it on the same path only to the hosts whose Agents it
relayed that offer to. So the Gateway's host must be marked as a Gateway, and each artifact costs
this Server one download, counted against the Gateway's aggregate bucket and recorded under the
Gateway's host, however many Agents behind it install it. A Client that asks while the Gateway is
still fetching is answered `503` with `Retry-After: 30` and asks again. **With `advertised_url` set, uploaded
artifacts are not delivered to Clients behind a Gateway:** the offered URL is absolute and names
this Server, which is not the Client's own origin, so the Client refuses it as a source not
allowed, or presents no certificate to it and this Server's handshake refuses it. Leave
`advertised_url` unset in a fleet with Gateways. Referenced artifacts are always fetched from
their source directly. How the Gateway holds what it fetches is in
[the Client's manual](client.md#the-package-cache).

## The REST API

The OpenAPI document at `/api/v1/openapi.json` is the contract; `/api/v1/docs` renders it. Every
error response carries a JSON body with an `error` field, so a generated client has something to
show. All of it is served on the Operator plane (`127.0.0.1:4321` by default), over TLS 1.3, and,
when [`[rest.auth]`](#the-operator-plane-restauth) is configured, needs Basic credentials.
`[rest.auth]` is required whenever the plane listens off the loopback.

| Method & path | What it does |
|---|---|
| `GET /api/v1/agents` | The whole fleet: every Agent, its attributes, capabilities, matching Configurations, package installations, health, and sync state. |
| `POST /api/v1/agents/{instance_uid}/restart` | Queue a restart of that Agent's Managed Process. Delivered on the next exchange — pushed over WebSocket, on the next poll over plain HTTP. Only Supervisor-backed Agents accept it; a Client's own Agent has no process to restart. |
| `PUT /api/v1/agents/{instance_uid}/labels` | Set this Agent's labels — see [Labels: rollout channels without touching the host](#labels-rollout-channels-without-touching-the-host). Body: `{"labels": {…}}`; an empty map clears them. |
| `DELETE /api/v1/agents/{instance_uid}` | Forget this Agent — see [Forgetting an Agent](#forgetting-an-agent) below. Reaches no host. `409` while it is still reporting. |
| `POST /api/v1/agents/{instance_uid}/rollout` | The per-Agent rollout act. Empty body: everything the fleet view shows as waiting for this Agent. `{"configuration": "…"}` or `{"package": {"name": "…", "agent_type": "…", "version": "…"}}`: that one resource — any version that fits, aims at, and would upgrade this Agent; `409` with the reason when it would not. |
| `GET /api/v1/configurations` | Every Configuration — the saved revision each. |
| `GET /api/v1/configurations/{name}` | One Configuration. |
| `PUT /api/v1/configurations/{name}` | Create it, or replace its saved revision. Body: `{"selector": {…}, "body": "…", "role": "…", "service_name": "…"}` — everything but `body` may be omitted. **Distributes nothing**. |
| `POST /api/v1/configurations/{name}/rollout` | Roll the saved revision out to every Agent it currently fits and aims at — the moment a change starts travelling. Answers how many Agents were assigned. |
| `DELETE /api/v1/configurations/{name}` | Remove it — from every Agent it was rolled out to as well, which those Agents apply. |
| `GET /api/v1/packages` | Every stored Package (never the artifact bytes), each with the Deployments that hold it. |
| `PUT /api/v1/packages/{agent_type}/{version}` | Create a Package. The body has no writable field — `{}`. **Distributes nothing**. |
| `GET /api/v1/packages/{agent_type}/{version}` | One Package. |
| `PUT /api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}` | Upload one platform's artifact (the raw body). `409` while the Package is rolled out to an Agent. A `?signature=` is refused `400` — it belongs to the Deployment. |
| `PUT /api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}/source` | Point that entry at an artifact hosted elsewhere. Body: `{"url": "…", "sha256": "…", "headers": {…}}`. |
| `DELETE /api/v1/packages/{agent_type}/{version}/entries/{os}/{arch}` | Remove one entry. `409` while the Package is rolled out to an Agent. |
| `DELETE /api/v1/packages/{agent_type}/{version}` | Remove the Package — and every per-Agent assignment that referenced it. Uninstalls nothing. |
| `GET /api/v1/packages/{agent_type}/{version}/file?os=…&arch=…` | The artifact bytes — where an offered `download_url` points. **The one route on the Agent plane** (`:4320`), and never guarded by `[rest.auth]`: it is not in the OpenAPI document for the same reason. It requires a client certificate from the client CA in the handshake; a bootstrap certificate is answered `401`. It serves only an artifact offered to an Agent the host speaks for, and answers every other request `404`, as for an artifact that does not exist. |
| `GET /api/v1/deployments` | Every Deployment, with its channel, its Packages, and the three reach counts. |
| `PUT /api/v1/deployments/{name}` | Create one or re-aim it. Body: `{"selector": {…}}` — **never empty** (`400`). **Distributes nothing**. |
| `GET` / `DELETE /api/v1/deployments/{name}` | One Deployment; `DELETE` is `409` while an Agent's assignment names it, and uninstalls nothing. |
| `PUT /api/v1/deployments/{name}/selector` | Re-aim it. Never distributes. |
| `PUT /api/v1/deployments/{name}/packages/{agent_type}/{version}` | Put a Package in the channel. `409` on a second of an Agent type it already holds; `?replace=true` swaps it. `404` for a Package nobody uploaded. |
| `DELETE /api/v1/deployments/{name}/packages/{agent_type}/{version}` | Take it out, and its signatures with it. |
| `PUT` / `DELETE /api/v1/deployments/{name}/signatures/{agent_type}/{version}/{os}/{arch}` | Record or remove one artifact's Ed25519 signature. Body: `{"signature": "<hex>"}`. |
| `POST /api/v1/deployments/{name}/rollout` | Roll it out to every Agent it claims and would move — the moment a rollout starts. Agents another Deployment also claims are skipped and reported as conflicts. `409` while the channel holds no Packages, and `409` naming each Package and platform while any entry lacks a signature. |
| `GET` / `POST` / `DELETE /api/v1/enrolment/window` | Read, open or close the enrolment window — see [Enrolment](#enrolment-a-new-host-approved-by-an-operator). `POST` body: `{"open_for_secs": n}`, 1 to 86400. `404` while `[enrolment]` is not configured. |
| `GET /api/v1/enrolments` | The pending enrolment requests, oldest first, each with its `id` — the fingerprint the enrolling Client logs. |
| `POST /api/v1/enrolments/{id}/approve` | Sign that request and hand the host its certificate. `404` for an unknown or expired id. |
| `POST /api/v1/enrolments/{id}/reject` | Refuse that request and close its connection. `404` for an unknown or expired id. |

The package routes answer `404` while package delivery is not configured on this Server.

### Whom a rollout actually reaches

The counts live on the **Deployment**, because that is what aims (see *Reading the counts* above).
A Package carries no count of its own — it aims at nobody — and answers the question by naming the
channels that hold it. A Package in no channel is stored and unreachable, which is the state a Selector
matching nobody used to be.

`targeted_agents` is what a rollout act would change; `claiming_agents` is who is in the channel
whatever they run; `conflicting_agents` is who this channel cannot reach because a second one claims
them too. It answers for the fleet **as reported so far**: a channel aimed at hosts that have not
connected yet legitimately reaches nobody, which is why these are counts to be read rather than
errors to be raised.

### Labels: rollout channels without touching the host

A Selector aims a Configuration or a Deployment at the Agents whose attributes match it — and the
attribute a staged rollout actually wants, `channel = "beta"`, is one you invent. Until now it could
only be invented in `[attributes]` in `supervisor.toml`, so moving a host between channels meant
editing a file **on that host** and restarting it.

**Labels are that attribute, set from here**:

```console
$ curl --cacert ca.pem -X PUT -H 'Content-Type: application/json' \
       -d '{"labels": {"channel": "beta"}}' \
       https://127.0.0.1:4321/api/v1/agents/<instance-uid>/labels
```

A label is matched exactly like a reported attribute, by **both** halves of the targeting: the
Configuration an Agent is sent and the Deployment it belongs to. So trying a new collector binary on
a few hosts is a Deployment aimed at `channel = beta` plus this call on the hosts that should get it
first — and moving a host back is the same call with the value it had.

The same call carries any other partition: `{"labels": {"region": "eu-central"}}` or
`{"labels": {"tenant": "acme"}}`. The Server treats all of them identically, because it knows none
of them — see [Channels are a partition](#channels-are-a-partition-not-a-default).

It takes effect at once: a connected Agent is pushed whatever its new channel gets, rather than waiting
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
learned, while a label is something you decided, so a host that comes back is in the channel you put it
in. Clearing them is its own call.

One caveat worth knowing: labels are keyed by Instance UID. If the Server re-keys an Agent — which
it does when two Agents report the same identity — the new identity starts with no labels.

### Forgetting an Agent

A host that was decommissioned leaves a row behind, and nothing ages it out. `DELETE
/api/v1/agents/{instance_uid}` — the `✕ forget` action on a fleet row — drops what this Server knows
about that Agent.

**It does nothing on the machine.** No process is stopped, nothing is uninstalled, and nothing is
revoked: a certificate proves *fleet membership* and its host, never which Agent is speaking, so
forgetting one Agent has nothing of it to take away. To shut a host out, [revoke its
certificate](#revocation-withdrawing-a-certificate). A Client that is still running and still pointed at this
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

The Agent plane admits by **client certificate alone**
([ADR-0026](../adr/0026-admission-by-a-client-certificate-alone.md)); see
[Mutual TLS](#mutual-tls-proving-who-is-on-the-connection). It has no credential of its own, and
`server.toml` has no `[auth]` section: one that is present is refused at startup, naming `[auth]`.
`/v1/opamp` and the download route read no `Authorization` header and send no
`WWW-Authenticate` challenge. A refusal behind the handshake — a revoked certificate, or a
bootstrap certificate on the download route — is answered `401`.

The Operator plane is the one with a credential, [`[rest.auth]`](#the-operator-plane-restauth)
below. It never reaches an Agent's host.

### Credentials are kept as hashes

No credential in `server.toml` authenticates on its own, so a copy of the file — in a backup, a
diff, configuration management — admits no host and signs in no operator. An operator's Basic
password is listed by its Argon2id hash, with at least `m=19456`, `t=2` and `p=1`. A value in clear
or a weaker hash is refused at startup, naming the section and the user without repeating the
value. Make each entry with the Server itself:

```console
$ server hash-credential --basic
secret:
$argon2id$v=19$m=19456,t=2,p=1$…
```

`--basic` is the one scheme; it reads the password from standard input, without showing it on a
terminal, and prints the hash to paste.

The package download route sits outside `[rest.auth]`: it is guarded by the client certificate
the handshake requires, and its content hash and signature protect what it serves.

### Repeated failures are throttled

`[admission_throttle]` limits how often one peer address may fail admission:

```toml
[admission_throttle]
max_failures = 10      # failures within the window that start a back-off
window_secs = 60
backoff_secs = 300     # how long the address is answered 429
```

Every `401` on `/v1/opamp` and on the download route counts as a failure of the peer's IP address.
An address that reaches `max_failures` within `window_secs` is in back-off for `backoff_secs`.
During the back-off each request is answered `429` with `Retry-After` set to the seconds remaining,
before anything else is checked. A WebSocket upgrade is refused the same way before it completes.
A successful admission clears nothing — behind a shared address a member's success would wipe a
guesser's count — so failures only age out of the window. On the Agent plane a `401` follows a
handshake that succeeded, so the throttle bounds what a refused host that keeps retrying costs in
handshakes, revocation lookups and audit entries. The Operator plane counts its own `401`s the same
way, in a table of its own. `0` for any of the three keys is refused at startup.

### How often a member may be heard

A certificate admits a member; `[agent_rate_limit]` bounds how often it is heard after that
([ADR-0023](../adr/0023-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)):

```toml
[agent_rate_limit]
messages_per_sec = 10            # tokens a host's or an Agent's bucket gains per second
burst = 300                      # that bucket's capacity; a new bucket starts full
gateway_messages_per_sec = 500   # tokens a marked Gateway's aggregate gains per second
gateway_burst = 10000            # the aggregate's capacity
```

Each message on `/v1/opamp`, over either transport, takes one token before anything else is done
with it, and so does each request on the package download route once the certificate is admitted.
A message that cannot be decoded is counted too. The bucket is:

- the host the certificate names;
- the certificate's issuer and serial, for a certificate an operator provisioned that names no
  host yet;
- the peer address, an IPv6 one by its /64, on an enrolment connection: one bootstrap certificate
  may serve the whole fleet;
- for a host marked as a Gateway, two buckets, and a message takes a token from both: the bucket
  of the Agent it names, sized by `messages_per_sec` and `burst`, and the Gateway's aggregate,
  sized by the `gateway_` keys. A message naming no valid `instance_uid`, an undecodable one and
  a download count in the aggregate alone. Marking or unmarking a Gateway takes effect at its next
  message.

A message past the limit is not processed: no record changes and no certificate is signed. It is
answered with the message's own `instance_uid` and `error_response` `Unavailable` with
`retry_info` of 30 seconds, on plain HTTP in the body of a `200`. The connection stays open. A
Client waits the 30 seconds, and the next message the Server processes from that Agent is asked
for a full report, so nothing is lost. A download past the limit is answered `429` with
`Retry-After: 30`; it is no failure toward `[admission_throttle]`, and the Client waits it out
before it asks again, for up to 30 minutes per download. Beyond this limit the Server
honours the protocol's error and retry semantics as well, and answers malformed input with
`BAD_REQUEST`.

Every refusal is recorded as `agent_rate.throttled`, naming the host (or the serial and the CA's
role), the `instance_uid`, the `bucket` that was empty (`agent`, `host` or `gateway`) and the
`route` (`opamp` with its `transport`, or `download`). Past ten a second from one address they are
counted in `agent_rate.throttled.aggregated`.

The default `burst` lets one full report from each of the 256 Agents a host may speak for through
after a reconnect. `messages_per_sec` times the offered `[connection_offer]
heartbeat_interval_secs`, or 30 seconds without one, is how many Agents one host can report for at
that interval; the Server logs a warning at startup naming both keys when that is below 256. The
buckets live in memory: the subjects and aggregates up to the certificate register's 100 000, the
Agents behind Gateways up to `max_agents`, and a full table forgets the bucket used least
recently. `0` for any key is refused at startup; no value switches the limit off.

### The Operator plane: `[rest.auth]`

`[rest.auth]` guards **the whole Operator plane** — `/api/v1/…`, the OpenAPI document, the API docs,
and the UI at `/`. With it, every request needs Basic credentials and anything else is answered
`401` with a `WWW-Authenticate: Basic` challenge. On a loopback `[rest] listen` the section is
optional, and the plane's address is what protects it. On any other address it is required: a
Server without it is refused at startup with a message naming both keys.

```toml
[rest]
listen = "0.0.0.0:4321"          # publishing it requires the section below

[rest.auth.basic_users]
fleet-admin = "$argon2id$v=19$m=19456,t=2,p=1$…"
```

Basic, and only Basic, because the audience is a browser and `curl`: the browser answers the
challenge by itself, so the bundled UI needs no login page, no session, and no cookie. Several users
are how a credential is rotated — add the new one, hand it out, remove the old — or how one
operator's is withdrawn without touching anyone else's.

**The operator tools carry it in the URL** they are given, which needs no new flag:

```console
$ curl --cacert ca.pem -u fleet-admin:secret https://127.0.0.1:4321/api/v1/agents
$ opamp-package-fetch … --server https://fleet-admin:secret@127.0.0.1:4321
```

Two limits worth stating plainly. It is **authentication, not authorization**: everyone listed can
do everything the plane offers — there are no roles, and one Server still manages one fleet. And
Basic sends a reusable password on **every** request. The plane always serves TLS 1.3, so the
password never crosses the network in cleartext, and `server.toml` holds only its Argon2id hash.

## Upgrading to admission by certificate alone

The Agent plane admits a Client by its client certificate alone and reads no `Authorization`
header: a Client or Gateway that sends one is admitted on its certificate, and the header is
ignored. A Client sends no credential, so a Server that still demands one answers it `401`.
**Upgrade the Server first, then its Clients and Gateways.**

1. **Delete the credential from `server.toml`.** Remove the `[auth]` section, and the credential
   keys `bearer_token_file`, `username`, `password_file`, `bearer_token` and `password` from
   `[connection_offer]`; a `[connection_offer]` left with neither `heartbeat_interval_secs` nor
   `endpoint` goes whole. The Server refuses to start while either remains, and says which:

   ```text
   server.toml: [auth] is refused — the Agent plane admits by client certificate alone; remove the section
   server.toml: [connection_offer] bearer_token_file is refused — the Agent plane admits by client certificate alone, so no credential is offered; remove the key
   ```

   A file the offered credential was read from can be deleted. `[rest.auth]` stays as it is.
2. **Start the Server.** Every Client that holds a valid certificate keeps connecting. A credential
   revocation in the persisted list is dropped when it is loaded, with one log line, and the list
   is written back without it.
3. **Upgrade the Clients**, through the self-update or by package. A Client sends no credential,
   ignores a leftover `[auth]` in `supervisor.toml` with a notice at startup, and drops an
   `Authorization` header a credential rotation left in its persisted connection settings,
   rewriting the file without it (see [the Client](client.md#a-leftover-auth)). A Gateway forwards
   no `Authorization` and shares its upstream connections among every Agent it carries.

## TLS

`[tls]` is required, and it turns **both listeners** into HTTPS/WSS listeners with one certificate
and key ([ADR-0023](../adr/0023-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)).
There is no plaintext port beside either of them. Every connection speaks TLS 1.3 alone, with the
three TLS 1.3 suites of the `ring` provider; a peer that speaks only TLS 1.2 cannot connect. Clients
use `wss://` or `https://` endpoints, and Clients trusting a private CA set `ca_file` in their own
`[tls]` section.

The same section names the CA a **client certificate** must chain to:
see [Mutual TLS](#mutual-tls-proving-who-is-on-the-connection).

### Bounds on every listener

Each plane is bounded before a request is parsed:

- **A connection cap per plane**: `max_connections` (default 10 000) for the Agent plane and
  `[rest] max_connections` (default 256) for the Operator plane. A connection past the cap is
  closed on accept, before the TLS handshake, while the ones already held keep working.
- **The TLS handshake** must complete within 10 seconds, and a peer has 30 seconds to send its
  HTTP/1 request headers.
- **HTTP/2** allows 100 concurrent streams per connection. A keep-alive ping goes out every 30
  seconds, and a peer that leaves one unanswered for 20 seconds is dropped.

Only the caps are keys, because they depend on the size of the fleet. The other bounds are the
same in every deployment.

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

What arrives: process metrics every 10 seconds (CPU, memory, uptime) for each Client's own process
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

- **A cleartext destination is refused anywhere but the loopback**
  ([ADR-0022](../adr/0022-own-telemetry-over-tls-1-3-and-plaintext-only-to-the-loopback.md)).
  `http://` is accepted only to the IP literals `127.0.0.1` and `::1`. The rest of `127.0.0.0/8`,
  the private ranges `10/8`, `172.16/12`, `192.168/16` and `fc00::/7`, link-local and carrier-grade
  NAT addresses are refused. The judgement is made on the parsed **address**: a host name over
  `http://` is refused, `localhost` included, and no name is resolved. The Agent refuses such a
  destination and reports the refusal back on the offer; it never warns and sends anyway. Every
  other destination is `https://` in TLS 1.3, so a Collector on the LAN, or one reached by name,
  needs TLS 1.3 in front of it. A destination that speaks only TLS 1.2 fails the handshake.
- **A Collector's internal telemetry does not come this way.** The Client must not touch a Managed
  Process's configuration, so what it reports about a Collector is what it can see from
  outside. Configure the Collector for its own internals as you would without OpAMP.

## Moving the fleet: connection settings

`[connection_offer]` is how the fleet is moved to a new heartbeat interval or a new endpoint
without touching every host. The Server compiles the section into one hash-gated offer, and every
Agent that accepts connection settings gets it, **verifies it by actually connecting**, and
switches only on success. An Agent that cannot connect with the offered settings keeps the ones it
has and reports the failure. An offer goes only to an Agent admitted with a certificate from the
client CA; an enrolling host receives none but the certificate it asked for.

The offer carries **no credential and no `headers`**
([ADR-0027](../adr/0027-connection-settings-offered-without-a-credential-and-server-capabilities.md)):
the Agent plane reads no `Authorization`, so an offered one would be a value nothing reads. A
credential key in the section — `bearer_token_file`, `username`, `password_file`, `bearer_token`,
`password` — is refused at startup, naming the key. A Client that is offered `headers` by another
Server applies the rest of the offer and reports it `FAILED`, naming the header keys.

**The offered `endpoint` is never plaintext off the host**
([ADR-0027](../adr/0027-connection-settings-offered-without-a-credential-and-server-capabilities.md)). It is
`wss://` or `https://`; `ws://` or `http://` only when its host is `127.0.0.1` or `::1`. Any other
`endpoint` is refused at startup with a message naming `[connection_offer] endpoint`.

The standing offer carries no `tls` and no `proxy`: there is no configuration surface for them. A
`certificate` travels in an offer only as the answer to an Agent's signing request.

## What the Server does not do

- **It does not require a password on a loopback Operator plane.** `[rest.auth]` is optional
  there, because the address is the guard; it is required the moment the plane listens anywhere
  else.
- **It does not weigh a message by its cost.** Every message and download takes one token of
  `[agent_rate_limit]`, whatever it carries. The Operator plane is not rate-limited; it is
  guarded by `[rest.auth]` and `[admission_throttle]`.
- **It does not download referenced package artifacts.** A referenced package is a URL plus a hash;
  the Agents fetch it.
- **It does not install itself as a service.** Run it under whatever supervises services on the
  host — the Client is the end that ships its own service integration.
- **It has no user model and no multi-tenancy.** One Server manages one fleet.
