# ADR-0003: One Client binary with two composable modes, multiplexing Agents over a connection pool

- **Status:** 🟢 accepted
- **Date:** 2026-07-22
- **Deciders:** Markus Brigl

## Context

The [specification](../SPECIFICATION.md) asks the client side to cover the shapes a real deployment
needs:

1. **Supervise local processes** — one client process running several Supervisors, each managing a
   Collector or a Foreign Agent.
2. **Serve a Collector that speaks OpAMP itself** — an OpenTelemetry Collector carrying the
   `opampextension` reports its description, health, and effective configuration over OpAMP rather
   than being observed from the outside; something local has to receive that and relay it upstream.
3. **Act as a gateway** — stand at a network boundary, accept OpAMP connections from many other
   clients, and carry them upstream over a small pool of connections, so a fleet can grow past the
   point where one connection per agent is affordable.

These are not three peers. Shapes 1 and 2 are **inseparable**, and the reason is a property of the
`opampextension` itself: it is an OpAMP **client only** and cannot act as a server, so it must be
given something to connect *to*. The natural counterpart is precisely the Supervisor that owns that
Collector, because that is what holds the configuration to hand down and the upstream connection to
relay onto. Making the local endpoint independently selectable would produce configurations that
cannot work — an endpoint with no Supervisor has nothing to relay *to*, and a Supervisor without one
cannot manage an extension-carrying Collector at all. Shape 3, by contrast, is genuinely independent:
gatewaying for other machines has nothing to do with supervising this machine's processes, and a host
may reasonably do either, or both.

Two further forces shape how this is built.

**The protocol already provides for multiplexing.** The Baseline's field documentation for
`ServerToAgent.instance_uid` states: *"When communication with multiple Agents is multiplexed into
one WebSocket connection (for example when a terminating proxy is used) the `instance_uid` field
allows to distinguish which Agent the ServerToAgent message is addressed to."* This is not an
extension — it is the protocol's own provision, and shapes 2 and 3 depend on it. The provision is
stated for the WebSocket transport; over plain HTTP the same n-over-m pooling follows from each
request carrying its own `instance_uid`. It also imposes a hard constraint on the **Server**: any
implementation that keys agent state on the connection rather than on `instance_uid` will misroute
messages as soon as a gateway sits in front of it. The Server must be written for *n* Agents over
*m* connections from the outset, because retrofitting that assumption later means touching every
piece of connection-handling state.

**All three shapes share almost everything.** Each terminates OpAMP on one side and relays or
originates it on the other; each needs the same connection handling, the same message codec, the
same identity handling. They differ in where messages come from and what happens to them locally —
which is precisely what the hexagonal Ports in the specification already abstract.

The prior art is instructive here. The OpAMP Gateway Extension (alpha, from Bindplane) multiplexes
agent connections onto a configurable upstream pool (default 10) using least-connections balancing,
forwards OpAMP messages unchanged, and deliberately holds **no authentication logic** — it passes the
connecting peer's headers and remote address upstream so that all policy stays on the Server. The
`opampsupervisor` in the Collector contrib repository runs a **local OpAMP server** that the
Collector's `opampextension` connects to — `supervisor.go` calls `server.New(...)` and starts it on
`localhost:<port>`, where the port comes from the `agent::opamp_server_port` setting and falls back to
a randomly chosen free port when unset. It is therefore unconditional: it comes up even when nothing
is configured. The supervisor also injects the extension's configuration into the Collector from an
embedded `templates/opampextension.yaml`, so the Collector is pointed at that endpoint by the
supervisor rather than by the operator. The extension itself is a client only, implementing just
`ReportsEffectiveConfig`, `ReportsHealth`, `ReportsAvailableComponents`, and optionally
`AcceptsRestartCommand`. Shapes 2 and 3 are therefore not novel — they are established patterns, and
this decision is about how to package them.

## Decision

We will ship **one Client binary** with exactly **two independent Client Modes** — **Supervisor Mode**
and **Gateway Mode** — either or both of which may run in a single process; the **Supervisor Endpoint**
is **not a mode** but an intrinsic part of every Supervisor; and **`instance_uid` is the sole routing
key** on both ends, so that *n* Agents are carried over *m* connections (*n* ≥ *m* ≥ 1) through a
**Connection Pool** whose size is a deployment choice rather than a consequence of how many Agents
exist.

Concretely this binds four things:

- **Two modes, freely composable, neither implying the other.** Supervisor Mode and Gateway Mode are
  orthogonal. A Client may supervise, gateway, or do both on the same host.
- **Every Supervisor exposes a Supervisor Endpoint, unconditionally.** It is bound to loopback and
  comes up with the Supervisor rather than being enabled separately. For a Managed Process that
  speaks no OpAMP — a Foreign Agent — nothing ever connects to it, and that is the whole of the
  handling: no configuration, no conditional code path, no failure. This deliberately trades a
  never-used listener in some deployments for the removal of an entire class of invalid
  configurations.
- **Neither end may key state on a connection.** The Server indexes Agents by `instance_uid` only.
  The Client likewise maps Agents onto pool connections without assuming a one-to-one relationship.
- **A Gateway makes no authentication decisions.** It forwards messages unchanged and passes the
  connecting peer's headers and remote address upstream, so authentication policy stays on the Server
  and a credential change never requires reconfiguring gateways.

A mode remains a composition of Ports, not a fork of the core: the supervision domain does not learn
what mode it is in, and a mode wires different adapters to the existing Server-facing and
Managed-Process-facing Ports.

## Alternatives considered

- **Separate deployables (a supervisor binary, a gateway binary).** Rejected. Both shapes share their
  entire OpAMP stack, so this would multiply the packaging, installation, and release surface to
  express a startup choice. It also forecloses the genuinely useful combination of a machine that
  supervises its own processes *and* fronts others. Splitting later is far easier than merging later.
- **A third, independently selectable "local server" mode.** Rejected — this was the shape of an
  earlier draft of this ADR. It reads as symmetric with the other two but is not: the local server
  has no purpose without the Supervisor that owns the Collector and holds the upstream connection.
  Making it selectable would let an operator configure combinations that cannot work (a local server
  with nothing to relay to; a Supervisor unable to manage an extension-carrying Collector) and would
  put a conditional on a code path that has no reason to vary. Binding it to the Supervisor removes
  the question instead of answering it.
- **Enable the Supervisor Endpoint only for Collector Supervisors, or make it configurable per
  Supervisor.** Rejected. It is the more precise description — a Foreign Agent will never use it —
  but it buys that precision with a configuration surface and a branch, to avoid an idle loopback
  listener. The `opampsupervisor` likewise brings its local server up unconditionally. If binding a
  loopback port turns out to be genuinely unwanted in some deployment, that is a concrete future need
  and can be revisited then.
- **One connection per Agent, no multiplexing.** Rejected. It is the simpler model and would satisfy
  the original specification, but it makes Gateway Mode impossible and caps fleet size at the
  Server's connection limit. Critically, the *Server-side* assumption it induces — connection equals
  agent — is the expensive one to reverse, because it spreads through all connection-handling state.
  Simplicity that must be undone later is not simplicity.
- **Multiplexing only in Gateway Mode, one connection per Supervisor otherwise.** Rejected. It leaves
  two routing models in the code permanently, and the Server must support the multiplexed one
  regardless. Supporting only the general model is less code, not more.
- **Authentication in the Gateway.** Rejected, and it conflicts with the specification's placement of
  authentication on the Server (goal 17). A gateway that decides who may connect duplicates policy
  and forces credential rotation to reach every gateway.

## Sources / Prior art

- [OpAMP specification, `ServerToAgent.instance_uid` field documentation](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — the multiplexing provision and the role of `instance_uid` in disambiguating Agents. Baseline
  version `v0.18.0` (see [`CONFORMANCE.md`](../CONFORMANCE.md)).
- [OpAMP Gateway Extension](https://bindplane.com/blog/opamp-for-opentelemetry-managing-collector-fleets-and-introducing-the-new-opamp-gateway-extension)
  — connection pooling with least-connections balancing (default 10 upstream connections), unchanged
  message forwarding, and authentication delegated upstream. Alpha status; a design reference, not a
  dependency.
- [`opampsupervisor` specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md)
  — the local OpAMP server the Collector's extension connects to, and the bootstrap flow that starts
  the Collector with a noop configuration so its extension can report in. Confirmed against the
  implementation: [`supervisor/supervisor.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/supervisor.go)
  (`server.New(...)` started on `localhost:<port>`; embedded `templates/opampextension.yaml`) and
  [`supervisor/config/config.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/config/config.go)
  (`OpAMPServerPort` / `agent::opamp_server_port`, optional with a random-port fallback).
- [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — confirms the extension is a **client only**, with a deliberately small capability set. Local
  Server Mode therefore serves it; it does not serve us.

## Consequences

- Positive: one deployable to build, sign, install, and update on every platform, and one place where
  OpAMP is terminated. A fleet can scale connections independently of agent count. Combining
  supervision and gateway duties on one machine costs nothing extra.
- Positive: writing the Server for `instance_uid` routing from the start avoids the most expensive
  refactor this project could otherwise face.
- Positive: binding the Supervisor Endpoint to the Supervisor removes a class of invalid
  configurations rather than validating against it, and leaves exactly one way for an
  extension-carrying Collector to be managed.
- Negative / trade-offs: the Client carries code for modes a given deployment does not use, making the
  binary larger and mode interaction a real test surface — the Supervisor + Gateway combination must
  be tested, not just each mode in isolation. Multiplexing also makes connection-level failures
  coarser: losing one pooled connection affects every Agent riding it, so reconnection and
  re-registration need care.
- Negative / trade-offs: every Supervisor binds a loopback port whether or not its Managed Process can
  use it, so a Client supervising only Foreign Agents opens listeners nothing will ever reach. Port
  selection and collision handling therefore need a defined answer even in deployments that never use
  the feature.
- Negative / trade-offs: Gateway Mode holds no authentication of its own, so an unauthenticated
  gateway port is reachable until the Server rejects the peer. This is a deliberate placement of
  policy, and it makes the Server's authentication (goal 17) load-bearing rather than optional.
- Follow-ups: the concrete authentication mechanism (mutual TLS versus tokens, and how the
  `ConnectionSettings` offer flow rotates credentials) is a separate decision and needs its own ADR
  before that code is written. The pool's sizing and balancing strategy, and the failure semantics
  when a pooled connection drops, are design questions to settle when Gateway Mode is implemented.
