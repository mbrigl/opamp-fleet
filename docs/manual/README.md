# OpAMP Fleet — User Manual

This manual is for the people who **run** OpAMP Fleet: it says what each end can do, how to start
it, and what every configuration key means. It is split the way a deployment is split — the Server
is one machine, the Clients are all the others — so each half can be read on its own:

| Part | Read it to |
|---|---|
| **[Server](server.md)** | run the control plane: the listener, Configurations and Selectors, packages and deployments, the REST API, authentication, TLS |
| **[Client](client.md)** | run a managed host, end to end: how it is built, the OS service, the on-disk layout, Supervisors for Collectors and Foreign Agents, package updates, self-update, and Gateway Mode |
| **[Rollout walkthrough](rollout.md)** | both ends at once, end to end: build an artifact, sign it, upload it, aim it, and watch a Foreign Agent be installed and configured entirely from the Server |
| **[GLPI Agent recipe](glpi-agent.md)** | deliver a third party's release and supervise it: the GLPI inventory agent as a foreground daemon, on Windows and Linux, configured from the Server |
| **[Icinga 2 recipe](icinga2.md)** | roll out a monitoring agent the fleet owns end to end: the program, its directories, its certificate from the Icinga master, and its configuration |
| **[Command-line tools](tools.md)** | get software into the fleet: fetch a known agent's release and hand it to the Server, or build, hash, and sign an artifact out of any program |
| **[Artifact documents](../artifacts/)** | for maintainers: what each wrapped agent's artifact *is* — source, assets, integrity, repack, the delivered tree, and what the Client derives from it. One per wrapped agent: [Icinga 2](../artifacts/icinga2.md), [GLPI Agent](../artifacts/glpi-agent.md), [Telegraf](../artifacts/telegraf.md) |

The two halves interlock in three places, and each is described on both sides: **authentication**
(the Client presents a credential the Server accepts), **connection settings** (the Server can move
the fleet to a new endpoint or credential), and **packages** (the Server decides *which* artifact an
Agent gets, the Client decides *whether* it takes one at all).

## What this manual is not

- **[`docs/SPECIFICATION.md`](../SPECIFICATION.md)** — the problem, the goals, and the vocabulary.
  Every capitalized term here (Agent, Supervisor, Configuration, Selector, Package, Foreign Agent,
  Managed Process) is defined there.
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
$ cargo run -p client -- --config config/supervisor.toml
```

An installed deployment runs the same two programs under the names `server` and `client`; the
`cargo run -p … --` prefix is only how you invoke them from a source checkout.

## Quick start: a closed loop on one machine

This is the smallest complete deployment — one Server, one Client, one Configuration — and it needs
no configuration file at all, because every setting has a default.

1. **Start the Server.** It serves two planes on two ports: the **Agent plane** on `4320` (the
   OpAMP endpoint at `/v1/opamp` and the package downloads), and the **Operator plane** on
   `127.0.0.1:4321` (the REST API under `/api/v1/`, the API docs at `/api/v1/docs`, and the bundled
   UI at `/`). The operator half is on loopback because nothing authenticates it yet.

   ```console
   $ cargo run -p server -- --config config/server.toml
   ```

2. **Start a Client.** With no `[[supervisor]]` block it presents exactly one Agent: itself.

   ```console
   $ cargo run -p client -- --config config/supervisor.toml
   ```

3. **Open the UI** at <http://127.0.0.1:4321/>. The Agent is listed as *Connected*, with the
   attributes it reported.

4. **Create and roll out a Configuration.** In the UI, press **Configurations**, give it a name,
   leave the Selector empty (which targets every Agent), enter the configuration text, save — and
   then press **Roll out to all matching**, because saving only stores; the rollout act is what
   reaches the fleet. The same two steps over the API:

   ```console
   $ curl -X PUT -H 'Content-Type: application/json' \
          -d '{"selector": {}, "body": "receivers: {}"}' \
          http://127.0.0.1:4321/api/v1/configurations/base
   $ curl -X POST http://127.0.0.1:4321/api/v1/configurations/base/rollout
   ```

5. **Watch the loop close.** A WebSocket Client receives it within a second, an HTTP Client on its
   next poll. It stores the configuration, reports it **Applied** with the matching hash, and its
   effective configuration appears in the fleet table. Rolling the same Configuration out again
   sends nothing — every push is gated on a content hash. An Agent that connects *later* is not
   changed by the earlier act: its row on the Agents tab shows the Configuration waiting, with a
   **roll out** control of its own.

From here, [Server](server.md) covers targeting a subset of the fleet and distributing software, and
[Client](client.md) covers putting a real Collector or a Foreign Agent under management.

## Concepts both halves use

**Agent and `instance_uid`.** The unit the Server manages is an *Agent*, identified by an
`instance_uid` and nothing else — not by the connection that carried it. One Client presents several
Agents: itself, always, plus one per configured Supervisor. All of them share the Client's single
connection, so the Server's fleet view has more rows than there are hosts.

**Attributes.** Every Agent reports attributes — `service.name`, `service.instance.name`,
`service.version`, `service.instance.id`, `os.type`, `os.name`, `os.version`, `os.description`,
`host.name`, `host.arch`, `host.id` — and an operator can add more in `supervisor.toml`, plus
`service.namespace` where a deployment uses one. These are what Selectors match on. An attribute the
host cannot answer is absent rather than empty.

Two of them are easy to confuse, and telling them apart is what aims everything else:
`service.name` is the Agent **type** — `otelcol-contrib`, `promtail`, `supervisor` for the Client's
own Agent — the
same value on every host running that kind of agent, while `service.instance.name` is **your** name
for one Agent, the `[[supervisor]]` block's `name`. Aim at the type to reach every Agent of a kind,
at the instance name to reach exactly one.

**Configuration and Selector.** A *Configuration* is a named body of text held by the
Server. Its *Selector* is a set of `key=value` pairs that an Agent's reported attributes must equal
for it to receive that Configuration; an empty Selector targets every Agent. An Agent matching
several Configurations receives all of them, as named entries, and merges them itself; an Agent
matching none is left running what it already runs.

**Role.** A Configuration may carry the role `supplementary`, which means *content the
Managed Process reads by path* — a rule file, a lookup table — rather than configuration it is
started with. The Client writes it beside the configuration under its own name, and leaves it out of
what the process is configured with.

**Package.** What an Agent type runs at a version — its identity is those two things and nothing
else, and it holds one artifact per platform. It reaches no Agent of another type, and it aims at
nobody by itself.

**Deployment.** Where a Package goes: a name, the **channel** a Selector aims at, at most one Package
per Agent type, and each artifact's signature. It is the only thing that is rolled out, and an
Agent belongs to **at most one** — two claiming the same Agent is a conflict the Server reports
rather than resolves. A Selector is equality and cannot say "not", so channels are a partition over an
attribute every Agent carries; there is no fleet-wide default. The Server decides which artifact an
Agent is offered; the Client decides whether it accepts packages at all. Artifacts are verified by
content hash always, and by Ed25519 signature when a verification key is configured.

**Transports.** The URL scheme in the Client's `endpoint` selects the transport:
`ws://`/`wss://` for WebSocket, where the Server pushes changes within seconds, and
`http://`/`https://` for plain-HTTP polling. The Server accepts both on the same path, at the same
time.

**Configuration files.** Both ends read one hand-edited TOML file, named with `--config`.
Every key is optional, an unknown key is refused at startup rather than ignored, and there are no
environment-variable fallbacks. The annotated examples in [`config/`](../../config/) are the
reference copies: [`config/server.toml`](../../config/server.toml) and
[`config/supervisor.toml`](../../config/supervisor.toml).

## Not built yet

The manual documents what runs today. These are designed, or partly built, and named here so you do
not go looking for a setting that does not exist. [`docs/CONFORMANCE.md`](../CONFORMANCE.md) is the
authority on all of it.

- **`tls` and `proxy` in connection settings** — a Server offering either is told, in
  its status report, that the Client dropped them. Mutual TLS itself *is* built: see
  [the Server](server.md#mutual-tls-proving-who-is-on-the-connection).
- **Certificate revocation** — there is no CRL and no OCSP. A short `validity_days` plus
  renewal is what bounds an issued certificate.
- **Custom messages** (`CustomCapabilities` / `CustomMessage`) — planned, not implemented.
- **Other connection settings** (`AcceptsOtherConnectionSettings`) — deliberately not implemented:
  the protocol leaves their meaning entirely to the Agent, so honouring the capability would mean
  inventing semantics.
