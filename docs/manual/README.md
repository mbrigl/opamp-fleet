# OpAMP Fleet — User Manual

This manual is for the people who **run** OpAMP Fleet: it says what each end can do, how to start
it, and what every configuration key means. It is split the way a deployment is split — the Server
is one machine, the Clients are all the others — so each half can be read on its own:

| Part | Read it to |
|---|---|
| **[Server](server.md)** | run the control plane: the listener, Configurations and Selectors, packages, the REST API, authentication, TLS |
| **[Client](client.md)** | run a managed host: the OS service, Supervisors for Collectors and Foreign Agents, package updates, self-update, and Gateway Mode |
| **[Rollout walkthrough](rollout.md)** | both ends at once, end to end: build an artifact, sign it, upload it, aim it, and watch a Foreign Agent be installed and configured entirely from the Server |

The two halves interlock in three places, and each is described on both sides: **authentication**
(the Client presents a credential the Server accepts), **connection settings** (the Server can move
the fleet to a new endpoint or credential), and **packages** (the Server decides *which* artifact an
Agent gets, the Client decides *whether* it takes one at all).

## What this manual is not

- **[`docs/SPECIFICATION.md`](../SPECIFICATION.md)** — the problem, the goals, and the vocabulary.
  Every capitalized term here (Agent, Supervisor, Configuration, Selector, Package, Foreign Agent,
  Managed Process) is defined there.
- **[`docs/adr/`](../adr/)** — *why* each thing is built the way it is. This manual states the rule
  and names the ADR; the reasoning stays in the ADR.
- **[`docs/CONFORMANCE.md`](../CONFORMANCE.md)** — how much of the OpAMP protocol each end
  implements, capability by capability, including what is deliberately missing.
- **[`CHANGELOG.md`](../../CHANGELOG.md)** — what an upgrade needs edited or moved before it will
  start.

## Before you start

Both binaries are built from one Cargo workspace; the build, test, and run commands live in the
[root README](../../README.md), under *Build, Test & Run*, and are not repeated here. It assumes
you can run:

```console
$ cargo run -p server -- --config config/server.toml
$ cargo run -p client -- --config config/client.toml
```

An installed deployment runs the same two programs under the names `server` and `client`; the
`cargo run -p … --` prefix is only how you invoke them from a source checkout.

## Quick start: a closed loop on one machine

This is the smallest complete deployment — one Server, one Client, one Configuration — and it needs
no configuration file at all, because every setting has a default.

1. **Start the Server.** It serves everything on one port (`4320` by default): the OpAMP endpoint at
   `/v1/opamp`, the REST API under `/api/v1/`, the API docs at `/api/v1/docs`, and the bundled UI at
   `/`.

   ```console
   $ cargo run -p server -- --config config/server.toml
   ```

2. **Start a Client.** With no `[[supervisor]]` block it presents exactly one Agent: itself.

   ```console
   $ cargo run -p client -- --config config/client.toml
   ```

3. **Open the UI** at <http://127.0.0.1:4320/>. The Agent is listed as *Connected*, with the
   attributes it reported.

4. **Create and publish a Configuration.** In the UI, press **Configurations**, give it a name,
   leave the Selector empty (which targets every Agent), enter the configuration text, save — and
   then press **Publish**, because saving only stores a draft; publishing is what reaches the
   fleet (ADR-0055). The same two steps over the API:

   ```console
   $ curl -X PUT -H 'Content-Type: application/json' \
          -d '{"selector": {}, "body": "receivers: {}"}' \
          http://127.0.0.1:4320/api/v1/configurations/base
   $ curl -X PUT -H 'Content-Type: application/json' \
          -d '{"published": true}' \
          http://127.0.0.1:4320/api/v1/configurations/base/publication
   ```

5. **Watch the loop close.** A WebSocket Client receives it within a second, an HTTP Client on its
   next poll. It stores the configuration, reports it **Applied** with the matching hash, and its
   effective configuration appears in the fleet table. Re-publishing the same Configuration sends
   nothing — every push is gated on a content hash.

From here, [Server](server.md) covers targeting a subset of the fleet and distributing software, and
[Client](client.md) covers putting a real Collector or a Foreign Agent under management.

## Concepts both halves use

**Agent and `instance_uid`.** The unit the Server manages is an *Agent*, identified by an
`instance_uid` and nothing else — not by the connection that carried it. One Client presents several
Agents: itself, always, plus one per configured Supervisor. All of them share the Client's single
connection, so the Server's fleet view has more rows than there are hosts.

**Attributes.** Every Agent reports attributes — `service.name`, `service.instance.name`,
`service.version`, `service.instance.id`, `os.type`, `os.name`, `os.version`, `os.description`,
`host.name`, `host.arch`, `host.id` — and an operator can add more in `client.toml`, plus
`service.namespace` where a deployment uses one. These are what Selectors match on. An attribute the
host cannot answer is absent rather than empty.

Two of them are easy to confuse, and telling them apart is what aims everything else (ADR-0033):
`service.name` is the Agent **type** — `otelcol-contrib`, `promtail`, `opamp-fleet-client` — the
same value on every host running that kind of agent, while `service.instance.name` is **your** name
for one Agent, the `[[supervisor]]` block's `name`. Aim at the type to reach every Agent of a kind,
at the instance name to reach exactly one.

**Configuration and Selector** (ADR-0012). A *Configuration* is a named body of text held by the
Server. Its *Selector* is a set of `key=value` pairs that an Agent's reported attributes must equal
for it to receive that Configuration; an empty Selector targets every Agent. An Agent matching
several Configurations receives all of them, as named entries, and merges them itself; an Agent
matching none is left running what it already runs.

**Role** (ADR-0016). A Configuration may carry the role `supplementary`, which means *content the
Managed Process reads by path* — a rule file, a lookup table — rather than configuration it is
started with. The Client writes it beside the configuration under its own name, and leaves it out of
what the process is configured with.

**Package** (ADR-0015, ADR-0017, ADR-0018). A named artifact the Server distributes. It states the
Agent **type** it is built for and reaches no Agent of another, whatever its Selector says
(ADR-0034) — a package with no type set reaches nobody at all — and within that type its Selector
picks which Agents, and its platform which bytes each of them gets. The Server decides which
artifact an Agent is offered; the Client decides whether it accepts packages at all. Artifacts are
verified by content hash always, and by Ed25519 signature when a verification key is configured.

**Transports** (ADR-0007). The URL scheme in the Client's `endpoint` selects the transport:
`ws://`/`wss://` for WebSocket, where the Server pushes changes within seconds, and
`http://`/`https://` for plain-HTTP polling. The Server accepts both on the same path, at the same
time.

**Configuration files** (ADR-0008). Both ends read one hand-edited TOML file, named with `--config`.
Every key is optional, an unknown key is refused at startup rather than ignored, and there are no
environment-variable fallbacks. The annotated examples in [`config/`](../../config/) are the
reference copies: [`config/server.toml`](../../config/server.toml) and
[`config/client.toml`](../../config/client.toml).

## Not built yet

The manual documents what runs today. These are designed, or partly built, and named here so you do
not go looking for a setting that does not exist. [`docs/CONFORMANCE.md`](../CONFORMANCE.md) is the
authority on all of it.

- **`tls` and `proxy` in connection settings** (ADR-0035) — a Server offering either is told, in
  its status report, that the Client dropped them. Mutual TLS itself *is* built: see
  [the Server](server.md#mutual-tls-proving-who-is-on-the-connection).
- **Certificate revocation** (ADR-0035) — there is no CRL and no OCSP. A short `validity_days` plus
  renewal is what bounds an issued certificate.
- **Custom messages** (`CustomCapabilities` / `CustomMessage`) — planned, not implemented.
- **Other connection settings** (`AcceptsOtherConnectionSettings`) — deliberately not implemented:
  the protocol leaves their meaning entirely to the Agent, so honouring the capability would mean
  inventing semantics.
