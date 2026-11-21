# SRV-two-use-case-layers: Two use-case layers stand between the Server's planes and its core

- **Status:** 🟢 accepted
- **Date:** 2026-11-21
- **Deciders:** Markus Brigl
- **Applies to:** the module structure of `crates/fleet-server/src/`: the Agent-plane endpoints in `transport.rs`, the Operator-plane endpoints in `api.rs` and every later Operator-plane endpoint, the two use-case modules between them and the core, the wiring in `lib.rs` and `main.rs`, the roles in `crates/fleet-core/tests/dependency_direction.rs`, and `docs/ARCHITECTURE.md`

## Context

The Server serves two planes on two listeners
([SEC-tls13-two-planes-bounded-input §7](SEC-tls13-two-planes-bounded-input.md)):
the Agent plane, `/v1/opamp`, the package download and the Gateways' revocation list in
`transport.rs`, and the Operator plane, the REST API and the bundled UI in `api.rs`. Behind both
sits one `AppState` (`crates/fleet-server/src/fleet.rs`), which holds the fleet in memory and every
store and service. Its operations have been cut into the modules of `fleet` (`session`, `delivery`,
`rollout`, `approval` and others), but both endpoint modules still call into `AppState` and into the
services beside it directly: `api.rs` names `AppState` 63 times, `Enrolment`, `Revocations`,
`Throttle` and `DeploymentStore` besides; `transport.rs` names `AppState`, `Enrolment`,
`Revocations`, `Throttle` and `AgentRate`. The planes are separated by listener and router, not by
what their code may do. A handler of one plane can reach any operation of the other, and a reviewer
has to read `fleet` and every module of it to know what the Agent plane can change.

On PostgreSQL several instances serve one fleet, and each serves both planes today
([SRV-instances-share-one-postgresql](SRV-instances-share-one-postgresql.md)). The proposed
[SRV-instance-opens-chosen-planes](SRV-instance-opens-chosen-planes.md) follows this ADR and lets
each instance open the planes its configuration names. The planes
already meet only through the database there: an operator's act on one instance reaches the
sessions on another by a notification
([SRV-instances-share-one-postgresql §9](SRV-instances-share-one-postgresql.md)) or by the
re-read after a missed one
([SRV-instances-share-one-postgresql §10](SRV-instances-share-one-postgresql.md)), and the
Operator plane reads the Agents from the database rather than from memory
([SRV-instances-share-one-postgresql §13](SRV-instances-share-one-postgresql.md)). What keeps them
together is the code. For availability, an instance that serves one plane alone would let a
reconnecting fleet not crowd out the operators who must act on it, and a heavy Operator-plane read
not slow the sessions; but such an instance is safe only once it is clear what each plane's code
reaches.

[DEV-ports-and-adapters](DEV-ports-and-adapters.md) structures the code as ports and adapters with
every dependency pointing at the core, and leaves the internal structure of the core open.
`crates/fleet-core/tests/dependency_direction.rs` gives every module the role core, adapter or
composition root, and both endpoint modules are adapters that may name every core module.

The target architecture recorded outside the ADRs, the working note "server split", puts this
split first (D1). Of its later steps, a database behind the ports and several Server instances
have landed without it; the rest — an MCP endpoint on the Operator plane, an instance that serves
one plane alone — each change what one plane does, and each is easier to review when the
operations a plane can reach are listed in one place.

The split changes no interface: not the REST API, the OpAMP exchange, the audit entries or
the persisted layout. It is recorded because it binds every later change to the endpoints, and
because a reader who finds an operation that both planes need written twice, or a handler that
cannot call the obvious method on `AppState`, would take it for a mistake.

The specification's goal G-5 makes the REST API the contract through which any UI drives the
fleet; the split makes that contract one module's surface.

## Decision

We will put one use-case module per plane between that plane's endpoints and the Server's core,
and let an endpoint reach state only through its own plane's module.

1. **Two use-case modules.** `agent_cases` holds every operation the Agent plane performs:
   admission, a report, a certificate request, an enrolment, a download, and the Gateways' read of
   the revocation list. `operator_cases` holds every operation the Operator plane performs: what the
   REST API, the bundled UI and any later Operator-plane endpoint read and change. Each is one type,
   `AgentCases` and `OperatorCases`, whose methods are the plane's operations.

2. **An endpoint must call its own plane's module and nothing else that holds state.**
   `transport` calls `AgentCases`; `api` calls `OperatorCases`. Neither may name a type of the core
   that holds state or a port — `AppState`, `ConfigStore`, `DeploymentStore`, `PackageStore`,
   `PackageOffering`, `Enrolment`, `Revocations`, `BootstrapCertificates`, `Throttle`,
   `AgentRate`, the ports `Audit` and `CertificateSigner`, and every such type added later.

3. **An endpoint may name the core's values.** Identifiers, records, summaries and error types of
   the core are named by the endpoints to map them to JSON or protobuf. A use-case module must not
   copy them into types of its own.

4. **The use-case modules must not name each other, nor an endpoint.** An operation both planes
   need is a method of the core that each module calls. Audit, the throttle, the rate limits and
   the revocation check stay in the core; the Agent plane's admission must reach them through
   `AgentCases`, the Operator plane's through `OperatorCases`. An endpoint must not name the other
   plane's endpoint either.

5. **One core, wired once.** The composition root must build the core, hand it to both modules,
   and give each router its own plane's module alone. A test may construct a module over a core of
   its choosing.

6. **Nothing an Agent or an operator sees may change.** The REST API and its OpenAPI document,
   the OpAMP exchange, the capabilities, the audit entries and the files under `data_dir` must stay
   as they are. A change of behaviour must not ride on the split.

**Out of scope:** splitting `AppState` itself or moving the modules of `fleet`; a third plane; an
MCP endpoint; an instance that serves one plane alone, and separate programs per plane; renaming the
planes. A rename is not decided here: the Operator plane would become the Control Plane, serving the
REST API, the OpenAPI document, the bundled UI and MCP, and the Agent plane the OpAMP plane, serving
`/v1/opamp`, the package download and the Gateways' revocation list. It touches the glossary and
every ADR that names a plane, so it is a decision of its own. Host telemetry in the UI is not
decided here either: logs, traces and metrics per host, read from ClickHouse and drawn with uPlot,
conflict with the specification's Non-Goal "Being a telemetry backend" and need a specification
change first. Its first check, if it is taken up, is whether an Agent's own telemetry carries its
Instance UID, so that it can be filtered per host.

## Alternatives considered

- **Two façades per domain** (fleet, packages, enrolment, revocation, and so on, each with an Agent
  and an Operator façade). It is finer, but it multiplies modules and wiring, and the question a
  reviewer asks — what can this plane do — is answered in a dozen places instead of one. Rejected.
- **Use-case modules that hand out types of their own.** Endpoints would not see the core at all.
  Most such types would copy a core value field by field, and the copies drift. Rejected; values
  cross, operations do not.
- **Admission, throttle and rate limits as the transport's own business.** They are what the Agent
  plane does before anything else, and keeping them outside the use cases would leave the endpoint
  naming `Revocations`, `Throttle` and `AgentRate` directly — the reach this decision removes.
  Rejected.
- **Separate programs per plane**, an OpAMP server and a control server built and released
  apart. It isolates the planes as well as a role per instance does, which one program can take
  (see *Consequences*), but adds a second artifact, image and release path, and a separate
  signing or admission service would add a network boundary for no current requirement. Rejected.
- **A role per instance now, without the split.** A switch that opens one listener alone is a
  small change, but nothing would show that an instance serving one plane never needs an
  operation the other plane's code holds; a handler that reaches `AppState` directly could rely on
  state only the other plane fills. Rejected; the role follows the split.
- **No split; review `fleet.rs` as a whole.** The later steps each touch what one plane may do,
  and would each have to show that the other plane is unaffected by reading all of it. Rejected.

## Sources / Prior art

- The working note "server split", section D1 and its order of work, recorded outside the ADRs.
- Alistair Cockburn, [Hexagonal architecture](https://alistair.cockburn.us/hexagonal-architecture/):
  application services as the boundary every driving adapter calls.
- The code as it stands: `AppState` in `crates/fleet-server/src/fleet.rs`; the call sites in
  `crates/fleet-server/src/api.rs` and `crates/fleet-server/src/transport.rs`; the roles in
  `crates/fleet-core/tests/dependency_direction.rs`.

## Consequences

- Positive: what each plane can do is one module's list of methods. A change to one plane shows
  in that module, and a review of the Agent plane does not read the Operator plane's operations.
- Positive: an MCP endpoint and an instance that serves one plane alone each start from a stated
  surface.
- Negative / trade-offs: a layer of methods that often only pass a call on, and two places to
  touch when an operation is added. Large handlers move into the modules, so the change is wide
  even though no behaviour changes.
- Negative / trade-offs: the list of state-holding types in clause 2 is kept by hand in the test;
  a new such type must be added to it.
- Follow-ups: a role per instance on PostgreSQL, one program whose instances open the Agent plane,
  the Operator plane or both, so that each plane is scaled and kept available apart.
  [SRV-instance-opens-chosen-planes](SRV-instance-opens-chosen-planes.md), proposed, decides it
  and builds on this split. Splitting `AppState` along the same line; renaming the planes; adding
  the use-case layer to the glossary.

## Enforcement

`crates/fleet-core/tests/dependency_direction.rs` will carry `Verifies:
SRV-two-use-case-layers`:

- A table of what each plane's modules may not name: `transport` names neither `operator_cases`
  nor `api`, `api` neither `agent_cases` nor `transport`, and neither use-case module names the
  other or an endpoint. A module both endpoints use names neither use-case module. Submodules are
  held to their module's row. In the dependency direction both use-case modules keep the role of
  an adapter, since they read certificates, CSRs and credentials through adapters; the table, not
  a role, decides their reach.
- Neither `transport` nor `api` names a type of the list in clause 2 outside `#[cfg(test)]`; the
  test keeps the list by hand.

The existing tests of the REST API and the OpAMP exchange stay unchanged and green, which is what
clause 6 asks.
