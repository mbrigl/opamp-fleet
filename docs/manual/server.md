- [Mutual TLS: proving who is on the connection](#mutual-tls-proving-who-is-on-the-connection)
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





The download route sits on the **Agent plane**, unauthenticated, deliberately: the content hash and
the signature are what protect an installed binary, not who was allowed to fetch it — and a Client
downloading one presents no credential, which is exactly why guarding the Operator plane cannot
break a rollout.
show. All of it is served on the Operator plane (`127.0.0.1:4321` by default) and, when
[`[rest.auth]`](#the-operator-plane-restauth) is configured, needs Basic credentials.
       http://127.0.0.1:4321/api/v1/agents/<instance-uid>/labels
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
- **It authenticates the REST API and the UI only if you ask it to** — `[rest.auth]`, Basic, off by
  default, with the plane on loopback until you publish it.
