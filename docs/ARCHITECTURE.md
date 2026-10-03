# Architecture — OpAMP Fleet

> The system **as it stands today**: the parts it is built from, what each is responsible for, and
> how they fit together. It holds no rule and no decision. It names the ADR behind each structural
> choice; where it disagrees with an accepted ADR, the ADR is right and this document is stale. It
> is updated in the same change as the structure it describes
> ([ADR-0001](adr/0001-agent-governance-model.md)).
>
> **Kept current by:** <one named role or person>. A document everyone may edit and nobody owns is
> the one that goes stale.
>
> **Last design revision:** none yet, due after 20 changes. The revision that ran moves the
> date; the number is this project's to set; a sensor counts the changes outside `docs/` since
> the date and says when the next is due ([ADR-0004](adr/0004-feature-layer.md)).

## Context

What sits outside the system and what crosses its boundary. A diagram earns its place here more
than anywhere else, and none beats one that has stopped being true.

TODO — fill in once the system has a boundary worth drawing.

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
  `Handler` and `server::listen`, the listener with its TLS and its bounds on connection setup.
  `tls` reads PEM and installs the ring provider. It reads no file and knows nothing of this
  project.
- **`fleet-core`** — what the Server and the Client implement identically beyond the protocol: the
  version and the platform aliases.
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
  ([ADR-0038](adr/0038-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes.md)).
- **Server admission** — `transport::Admission` requires the fleet credential and the client
  certificate on every request to `/v1/opamp`, admits a bootstrap certificate only while the
  enrolment window is open, and guards the package download with the same certificate. Two core
  modules hold the state behind it: `enrolment`, the operator-opened window and the queue of
  requests an operator approves through `api`, and `throttle`, the per-address back-off after
  repeated failures, which both planes use
  ([ADR-0039](adr/0039-admission-requires-both-proofs-and-enrolment-is-approved.md)).
- **Server ports beyond storage** — `fleet` owns `CertificateSigner`, which the local CA in `ca`
  implements for the CSR flow ([ADR-0039](adr/0039-admission-requires-both-proofs-and-enrolment-is-approved.md)), and
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
  ([ADR-0036](adr/0036-the-whole-opamp-communication-layer-in-the-opamp-crate.md)).
- **Client engine** — `engine` routes the Server's replies to the Agents over one connection
  ([ADR-0040](adr/0040-client-modes-and-a-gateway-that-admits-over-mutual-tls.md)). The transports, the Gateway, telemetry
  and the service runtime are adapters around it.
- **Client self-update** — `update` is the Client updating itself
  ([ADR-0044](adr/0044-the-client-updates-itself-from-a-signed-package.md)). It owns the port `SelfUpdater` and
  `SelfUpdate`, the state the Engine keeps about it: armed or not, probation committed, restart
  due. `update::installer` implements the port on the version directories and holds the start-up
  check of the process that follows an install.

## How it runs

The few paths worth following end to end, such as a request, a job, or a build, and where state
lives between them. Only what a newcomer would otherwise reconstruct from code.

TODO — fill in once there is more than one part to connect.
