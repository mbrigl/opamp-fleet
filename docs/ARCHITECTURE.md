# Architecture — OpAMP Fleet

> The system **as it stands today**: the parts it is built from, what each is responsible for, and
> how they fit together. It holds no rule and no decision. It names the ADR behind each structural
> choice; where it disagrees with an accepted ADR, the ADR is right and this document is stale. It
> is updated in the same change as the structure it describes
> ([ADR-0001](adr/0001-agent-governance-model.md)).
>
> **Kept current by:** Markus Brigl. A document everyone may edit and nobody owns is
> the one that goes stale.
>
> **Last design revision:** none yet, due after 20 changes. The revision that ran moves the
> date; the number is this project's to set; a sensor counts the changes outside `docs/` since
> the date and says when the next is due ([ADR-0004](adr/0004-feature-layer.md)).

## Context

What sits outside the system and what crosses its boundary. A diagram earns its place here more
than anywhere else, and none beats one that has stopped being true.

```text
 operator ── REST / bundled UI ──▶ Operator plane :4321 ─┐
                                                         │  Server (Linux)
 Client ──── OpAMP, mTLS ──────────────▶ Agent plane :4320 ─┘   │
   │  ▲                                                          ├─▶ config_dir: fleet state,
   │  └── Gateway (a Client) ◀── OpAMP, mTLS ── other Clients    │   register, audit record
   │                                                             └─▶ packages_dir: artifacts
   ├── Supervisor Endpoint (loopback) ◀── a Collector's opampextension
   ├── Managed Processes: Collector, Telegraf, GLPI Agent, Icinga 2, any command
   ├── package sources: the Server's origin, or a mirror in allowed_sources
   ├── own-telemetry destinations: OTLP/HTTP, as the Server offers them
   └── service manager: systemd, launchd or the Windows SCM
```

- **Operators** drive the fleet through the REST API on the Operator plane, guarded by
  `[rest.auth]` beyond the loopback; the bundled UI uses the same API
  ([ADR-0059](adr/0059-admission-by-a-client-certificate-alone.md)).
- **Clients** reach the Agent plane over OpAMP — WebSocket or plain HTTP — admitted by the client
  certificate in the TLS 1.3 handshake alone
  ([ADR-0054](adr/0054-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md),
  ADR-0059). A Client in Gateway Mode carries other Clients' Agents over its own upstream
  connections and refuses what the Server revoked
  ([ADR-0064](adr/0064-client-modes-and-a-gateway-that-admits-by-certificate-and-refuses-what-the-server-revoked.md)).
- **Managed Processes** run on the Client's host under a Supervisor each; a Collector reports
  through its own `opampextension` to the Supervisor Endpoint, which admits only the process its
  Supervisor started ([ADR-0053](adr/0053-the-supervisor-endpoint-admits-only-its-own-process.md)).
- **Package sources** serve artifacts: the Server itself, or a mirror the Client's
  `allowed_sources` names. What arrives is checked against its hash and the operator's signature
  ([ADR-0042](adr/0042-signed-package-delivery-from-allowed-sources.md)).
- **The service manager** starts the Client and restarts it after a self-update
  ([ADR-0044](adr/0044-the-client-updates-itself-from-a-signed-package.md)).
- **Upstream**, the opamp-spec release named as the Protocol Baseline in
  [`CONFORMANCE.md`](CONFORMANCE.md) fixes the wire format.

## Building blocks

The parts the system is made of, each with one responsibility, named in the vocabulary of
[`GLOSSARY.md`](GLOSSARY.md). One level deep, deeper only where the size of a part earns it. The
golden path and the test pattern an agent copies from are named here.

Five crates in one workspace ([ADR-0037](adr/0037-five-crates-a-publishable-communication-layer-and-toml-configuration.md)):

- **`opamp`** — the OpAMP communication layer, publishable on its own
  ([ADR-0036](adr/0036-the-whole-opamp-communication-layer-in-the-opamp-crate.md)). Always the
  generated types, framing and the endpoint's body rules. Behind `client`, an Agent's protocol state
  machine, the two transports, and `client::connection`, which builds a connection, its TLS and its
  HTTP client from a `Connection` the application fills in. Behind `server`, the endpoint around a
  `Handler`, `server::listen`, the listener with its TLS and its bounds on connection setup, and
  `server::pace`, the floor every body and WebSocket message it serves is held to. The server
  side takes a WebSocket over through hyper's upgrade and reads its frames with
  `tokio-tungstenite`, the library the client side uses too.
  `tls` reads PEM and installs the ring provider. It reads no file and knows nothing of this
  project.
- **`fleet-core`** — what the Server and the Client implement identically beyond the protocol: the
  version, the platform aliases, the statement a package signature covers, and the renewal proof a
  CSR carries.
- **`fleet-server`** — the Server: the fleet, its Configurations, labels, packages and
  Deployments, the Agent plane and the Operator plane.
- **`fleet-agent`** — the Client in all its modes, the program `supervisor`.
- **`fleet-tools`** — the operator command-line tools `opamp-package-fetch` and
  `opamp-package-sign`.

Inside the Server and the Client the code is ports and adapters
([ADR-0006](adr/0006-architecture-style.md)): a **core** that holds the domain and owns its
**ports**, **adapters** that bind a port to a technology, and a **composition root** — `lib.rs`,
`main.rs`, and on the Client `supervisor` and `service::runtime` — that wires the two. Every
module's role is listed in
[`dependency_direction.rs`](../crates/fleet-core/tests/dependency_direction.rs), which fails when a
core module names an adapter or a technology, and when a module has no role.

- **Server stores** — each store is a port in the core module it serves (`AgentStore` in
  `agent_store`, `LabelStore` in `labels`, `ConfigBackend` under the `ConfigStore` in `configs`,
  `DeploymentBackend` under the `DeploymentStore` in `deployments`, `PackageBackend` under the
  `PackageStore` in `packages`), with its filesystem adapter in `fs`. The package port speaks in
  paths, because ADR-0020 makes an artifact a file that is staged, re-hashed and streamed. The
  core's constructors take the ports — `AppState::with_stores`, `PackageStore::with_backend`,
  `PackageOffering::with_deployments` — and `lib.rs` wires the filesystem adapters in
  `AppState::new`, `PackageStore::open` and `PackageOffering::new`.
- **Server listeners** — `tls` reads the `[tls]` and `[enrolment]` files into the material each
  plane serves with — the Agent plane requiring a client certificate in the handshake, the
  Operator plane asking for none — and tells a member's certificate from a bootstrap one by its
  issuer. `listen` serves both planes on one handle with one drain and a connection cap each
  ([ADR-0054](adr/0054-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)).
- **Server admission** — `transport::Admission` requires the client certificate on every request
  to `/v1/opamp` and reads no `Authorization` header, admits a bootstrap certificate only while the
  enrolment window is open, and guards the package download with the same certificate. Two core
  modules hold the state behind it: `enrolment`, the operator-opened window and the queue of
  requests an operator approves through `api`, and `throttle`, the per-address back-off after
  repeated failures, which both planes use
  ([ADR-0059](adr/0059-admission-by-a-client-certificate-alone.md)). `revocation`
  holds the register of issued certificates, the revocation list and the hosts, behind the
  `LedgerStore` port ([ADR-0065](adr/0065-certificate-revocation-that-follows-renewal-and-reaches-the-gateways.md)).
  `agent_rate` holds the token buckets that bound how often an admitted host, an Agent behind a
  marked Gateway, and the Gateway as a whole are heard; `transport` takes a token for each
  message before the handler does anything else and for each download in the guard
  ([ADR-0066](adr/0066-admitted-agents-are-rate-limited-per-host.md)).
- **Audit record** — `audit` is the port every security decision is recorded through;
  `audit_log` chains the entries by hash and `fs::FsAuditStore` keeps them under
  `config_dir/audit/` ([ADR-0063](adr/0063-an-append-only-audit-record-chained-by-hash.md)).
- **Server ports beyond storage** — `fleet` owns `CertificateSigner`, which the local CA in `ca`
  implements for the CSR flow and the renewal proof (ADR-0059), and
  `Clock`, which `clock::SystemClock` implements. The `[connection_offer]` and
  `[telemetry_offer]` sections become the fleet's offers in `config`.
- **REST views** — what the REST API reads and returns is shaped in `api`, which derives its
  OpenAPI schema ([ADR-0016](adr/0016-configurations-and-the-rest-api.md)); the core's types —
  among them the fleet view `AgentView` — carry no schema, and `api` converts them.
- **Client supervision** — `supervisor::ports` is the Port a Plugin implements
  ([ADR-0015](adr/0015-supervisor-mode-and-its-kinds.md)), and `shutdown` the handle every task
  stops on; the Plugins and the process runner are its adapters. How a `[[supervisor]]` block is
  read — its plugin, program, Agent type and timings — is `supervisor::block`; the registry of
  Plugins stays in `supervisor`, which knows the kinds. When a failed process is started again or
  held is `supervisor::restart`'s decision; the runner reads the clock and starts it.
  `supervisor::agent`, an Agent's decisions, names two more ports there: `AgentStorage`, which
  `storage::Storage` implements on the state directory, and `HostFacts`, which `host::SystemHost`
  implements from the platform.
- **Client configuration** — `config` is what `supervisor.toml` may say and the rules it must meet
  ([ADR-0037](adr/0037-five-crates-a-publishable-communication-layer-and-toml-configuration.md)); `config_file` reads it from disk, makes its
  directories absolute, and finds the identity the state directory holds.
- **Client connection** — `tls` decides which CA and which identity are in force, and `transport`
  describes the upstream `Connection` from `supervisor.toml` and runs the Engine over it. The
  settings verification in `connection`, the Gateway's upstream pool and its downstream listener,
  and the Supervisor Endpoint use the same `opamp` building blocks
  ([ADR-0036](adr/0036-the-whole-opamp-communication-layer-in-the-opamp-crate.md)). The Gateway's
  `revocations` keeps the list it fetches from its Server and refuses what that list names
  ([ADR-0064](adr/0064-client-modes-and-a-gateway-that-admits-by-certificate-and-refuses-what-the-server-revoked.md)).
- **Client engine** — `engine` routes the Server's replies to the Agents over one connection
  ([ADR-0064](adr/0064-client-modes-and-a-gateway-that-admits-by-certificate-and-refuses-what-the-server-revoked.md)). The transports, the Gateway, telemetry
  and the service runtime are adapters around it.
- **Client self-update** — `update` is the Client updating itself
  ([ADR-0044](adr/0044-the-client-updates-itself-from-a-signed-package.md)). It owns the port `SelfUpdater` and
  `SelfUpdate`, the state the Engine keeps about it: armed or not, probation committed, restart
  due. `update::installer` implements the port on the version directories and holds the start-up
  check of the process that follows an install.

## How it runs

The few paths worth following end to end, such as a request, a job, or a build, and where state
lives between them. Only what a newcomer would otherwise reconstruct from code.

**A Configuration reaches an Agent.** An operator saves a Configuration through `api` and rolls
it out; `fleet` assigns it to every Agent its Selector and Agent type match, composes each Agent's
configuration and pushes the offer over the Agent's WebSocket, or answers the next poll with it.
On the Client, `engine` routes the offer to the Agent's Supervisor, which writes the entries into
the Supervisor's `config/` directory and restarts its process; the Agent reports `APPLIED` or
`FAILED` with the reason, and the Server stops offering once the reported hash matches
([ADR-0016](adr/0016-configurations-and-the-rest-api.md)). A Configuration typed for the Client
itself carries `[[supervisor]]` blocks, which `reconfigure` checks and writes into
`supervisor.toml` before it starts or stops anything
([ADR-0051](adr/0051-a-delivered-block-brings-nothing-past-the-signature.md)).

**A package reaches an Agent.** An operator uploads an artifact into `packages`, puts it into a
Deployment with a Selector and the operator's signature, and releases it. The Agent is offered the
package; the Client downloads it from the Server's origin — presenting its certificate there and
nowhere else, and `api` serves it only when `fleet` finds it offered to an Agent the
certificate's host speaks for, by the test `packages` shares with the offer
([ADR-0068](adr/0068-a-host-fetches-only-the-packages-offered-to-its-own-agents.md)) — or from an
allowed mirror, checks hash and signature in `packages`, and the
Supervisor swaps the program, keeps the previous one for its grace period and rolls back if the
new one does not stay up. The Client's own package goes through `update` instead: a version
directory beside the running one, a self-check, the `current` pointer moved, and a restart on
probation that commits or rolls back.

**A host joins and stays.** A fresh host presents a bootstrap certificate while an operator has
the enrolment window open; its CSR waits until an operator approves it, and the certificate it
gets names a new host. From then on the Client renews at two thirds of the certificate's life,
proving with the old key which certificate it renews. A revocation closes the sessions it
concerns at once and follows every renewal.

**Where state lives.** The Server keeps its state under `config_dir` — Configurations, `agents/`,
`labels/`, `revocation/` and `audit/` — and its artifacts under `packages_dir`; a restart restores
the fleet from them and shows each Agent disconnected until it reports. The Client keeps its state
under `state_dir`: the issued certificate and key, the connection settings the Server offered,
and per Supervisor a directory with `config/` and `program/`. An installed Client runs from
`versions/<version>/` through the `current` pointer
([ADR-0061](adr/0061-the-client-as-an-installed-service-with-a-secure-first-configuration.md)).
