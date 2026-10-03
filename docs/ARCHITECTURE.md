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

Five crates in one workspace ([ADR-0034](adr/0034-five-crates-a-publishable-wire-layer-and-toml-configuration.md)):

- **`opamp`** — the OpAMP wire layer, publishable on its own: the generated types, framing, the
  endpoint's body rules, an Agent's protocol state machine and drivers behind `client`, and the
  server endpoint behind `server`
  ([ADR-0031](adr/0031-one-opamp-crate-a-publishable-wire-layer-with-client-and-server-features.md)). It knows nothing of
  this project.
- **`fleet-core`** — what the Server and the Client implement identically beyond the protocol: the
  version, the platform aliases, the PEM readers.
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
- **Server ports beyond storage** — `fleet` owns `CertificateSigner`, which the local CA in `ca`
  implements for the CSR flow ([ADR-0017](adr/0017-admission-and-authentication.md)), and
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
  ([ADR-0011](adr/0011-workspace-crates-and-configuration.md)); `config_file` reads it from disk, makes its
  directories absolute, and finds the identity the state directory holds.
- **Client engine** — `engine` routes the Server's replies to the Agents over one connection
  ([ADR-0009](adr/0009-client-modes-and-the-gateway.md)) and owns `SelfUpdater`, which
  `selfupdate::Installer` implements on the version directories
  ([ADR-0021](adr/0021-the-client-updates-itself.md)). The transports, the Gateway, telemetry and
  the service runtime are adapters around it.

## How it runs

The few paths worth following end to end, such as a request, a job, or a build, and where state
lives between them. Only what a newcomer would otherwise reconstruct from code.

TODO — fill in once there is more than one part to connect.
