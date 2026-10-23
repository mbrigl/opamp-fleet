# ADR-0054: Two use-case layers stand between the Server's planes and its core

- **Status:** 🟡 proposed
- **Date:** 2026-10-21
- **Deciders:** Markus Brigl
- **Applies to:** the module structure of `crates/fleet-server/src/`: the Agent-plane endpoints in `transport.rs`, the Operator-plane endpoints in `api.rs` and every later Operator-plane endpoint, the two use-case modules between them and the core, the wiring in `lib.rs` and `main.rs`, the roles in `crates/fleet-core/tests/dependency_direction.rs`, and `docs/ARCHITECTURE.md`

## Context

The Server serves two planes on two listeners
([ADR-0013](0013-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)):
the Agent plane, `/v1/opamp` and the package download in `transport.rs`, and the Operator plane,
the REST API and the bundled UI in `api.rs`. Behind both sits one `AppState`
(`crates/fleet-server/src/fleet.rs`), which holds the fleet in memory and every store and service,
and both endpoint modules call into it and into the services beside it directly: `api.rs` names
`AppState` 54 times, `Enrolment`, `Throttle` and `DeploymentStore` besides; `transport.rs` names
`AppState`, `Enrolment`, `Revocations`, `Throttle` and `AgentRate`. The planes are separated by
listener and router, not by what their code may do. A handler of one plane can reach any operation
of the other, and a reviewer has to read the whole of `fleet.rs` to know what the Agent plane can
change.

[ADR-0006](0006-architecture-style.md) structures the code as ports and adapters with every
dependency pointing at the core, and leaves the internal structure of the core open.
`crates/fleet-core/tests/dependency_direction.rs` gives every module the role core, adapter or
composition root, and both endpoint modules are adapters that may name every core module.

The target architecture recorded outside the ADRs, the working note "server split", puts this
split first (D1): later steps — a database behind the ports and several Server instances, an
MCP endpoint on the Operator plane, separate TLS identities per plane — each change what one
plane does, and each is easier to review when the operations a plane can reach are listed in one
place. The split changes no interface: the REST API, the OpAMP exchange, the audit entries and the
persisted layout stay as they are. It is recorded because it binds every later change to the
endpoints, and because a reader who finds an operation that both planes need written twice, or a
handler that cannot call the obvious method on `AppState`, would take it for a mistake.

The specification's goal G-5 makes the REST API the contract through which any UI drives the
fleet; the split makes that contract one module's surface.

## Decision

We will put one use-case module per plane between that plane's endpoints and the Server's core,
and let an endpoint reach state only through its own plane's module.

1. **Two use-case modules.** `agent_cases` holds every operation the Agent plane performs:
   admission, a report, a certificate request, an enrolment, a download. `operator_cases` holds
   every operation the Operator plane performs: what the REST API, the bundled UI and any later
   Operator-plane endpoint read and change. Each is one type, `AgentCases` and `OperatorCases`,
   whose methods are the plane's operations.

2. **An endpoint calls its own plane's module and nothing else that holds state.** `transport`
   calls `AgentCases`; `api` calls `OperatorCases`. Neither names a type of the core that holds
   state or a port — `AppState`, `ConfigStore`, `DeploymentStore`, `PackageStore`,
   `PackageOffering`, `Enrolment`, `Revocations`, `BootstrapCertificates`, `Throttle`,
   `AgentRate`, and every such type added later.

3. **An endpoint may name the core's values.** Identifiers, records, summaries and error types of
   the core are named by the endpoints to map them to JSON or protobuf. A use-case module does not
   copy them into types of its own.

4. **The use-case modules do not name each other.** An operation both planes need is a method of
   the core that each module calls. Audit, the throttle, the rate limits and the revocation check
   stay in the core; the Agent plane's admission reaches them through `AgentCases`, the Operator
   plane's through `OperatorCases`.

5. **One core, wired once.** The composition root builds the core, hands it to both modules, and
   gives each router its own plane's module alone. A test may construct a module over a core of
   its choosing.

6. **Nothing an Agent or an operator sees changes.** The REST API and its OpenAPI document, the
   OpAMP exchange, the capabilities, the audit entries and the files under `data_dir` stay as they
   are. A change of behaviour does not ride on the split.

**Out of scope:** splitting `fleet.rs` or `AppState` themselves; a third plane; an MCP endpoint;
separate TLS identities for the planes; separate processes; renaming the planes.

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
- **Separate processes per plane.** It isolates the planes' memory, but a separate signing or
  admission service adds a network boundary and a deployment for no current requirement; the
  working note chose one process. Rejected.
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
- Positive: a database behind the ports, several Server instances and an MCP endpoint each start
  from a stated surface.
- Negative / trade-offs: a layer of methods that often only pass a call on, and two places to
  touch when an operation is added. Large handlers move into the modules, so the change is wide
  even though no behaviour changes.
- Negative / trade-offs: the list of state-holding types in clause 2 is kept by hand in the test;
  a new such type must be added to it.
- Follow-ups: splitting `AppState` along the same line, once the database decision says what of
  it stays in memory; adding the use-case layer to the glossary.

## Enforcement

`crates/fleet-core/tests/dependency_direction.rs` will carry `Verifies: ADR-0054`:

- `agent_cases` and `operator_cases` get roles of their own; `transport` may name `agent_cases`
  and not `operator_cases`, `api` the reverse, and neither use-case module names the other.
- Neither `transport` nor `api` names a type of the list in clause 2 outside `#[cfg(test)]`.

The existing tests of the REST API and the OpAMP exchange stay unchanged and green, which is what
clause 6 asks.
