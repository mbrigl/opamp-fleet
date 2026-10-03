# ADR-0009: One Client binary with two composable modes, carrying n Agents over m connections

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/gateway/, crates/fleet-agent/src/supervisor/endpoint.rs, the [gateway] configuration section, and every place either end keeps per-Agent state

## Context

The [specification](../SPECIFICATION.md) asks the client side to cover three shapes: supervise
local processes, serve a Collector that speaks OpAMP itself, and act as a gateway that carries many
other Clients upstream over a small Connection Pool (goal 15). They are not three peers.

**Supervising and serving an OpAMP-speaking Collector are inseparable.** The Collector's
`opampextension` is an OpAMP *client only*; it must be given something to connect to. The natural
counterpart is the Supervisor that owns that Collector, because it holds the configuration to hand
down and the upstream connection to relay onto. An endpoint without a Supervisor has nothing to
relay to, and a Supervisor without one cannot manage an extension-carrying Collector. Gatewaying for
other machines is genuinely independent: a host may supervise, gateway, or do both.

**The protocol provides for multiplexing.** The Baseline documents `ServerToAgent.instance_uid`:
*"When communication with multiple Agents is multiplexed into one WebSocket connection (for example
when a terminating proxy is used) the `instance_uid` field allows to distinguish which Agent the
ServerToAgent message is addressed to."* Over plain HTTP the same pooling follows from each request
carrying its own `instance_uid`. Any end that keys Agent state on the connection misroutes messages
as soon as a gateway sits in front of it, and that assumption is the expensive one to remove later.

**A Gateway must not invent messages.** The specification forbids resolving a protocol gap by
inventing semantics of this project's own. When something fails on one side of a hop, the tempting
fix is to synthesise a message toward the other, and that is exactly what is not available. Nor is
a connection an identity in either direction: upstream connection state says nothing about whether a
downstream Client is alive.

**A pool that is a fixed cost is a pool nobody keeps small.** The prior art opens ten connections at
startup; ten connections for a gateway in front of three Agents is worse than three.

## Decision

We will ship one Client binary with exactly two independent Client Modes, Supervisor Mode and
Gateway Mode, give every Supervisor a Supervisor Endpoint unconditionally, route by `instance_uid`
alone on both ends, and implement Gateway Mode as a `[gateway]` section serving both transports
downstream over a lazily grown, sticky upstream pool that forwards messages unchanged and
synthesises none.

1. **Two modes, freely composable, neither implying the other.** Supervisor Mode (`[[supervisor]]`
   blocks) and Gateway Mode (`[gateway]`) run alone or together in one process. A mode is a
   composition of Ports, not a fork of the core: the supervision domain does not learn which mode
   it is in.

2. **Every Supervisor exposes a Supervisor Endpoint, unconditionally.** It binds `127.0.0.1` on the
   block's `endpoint_port` ([ADR-0015](0015-supervisor-mode-and-its-kinds.md)) before the Managed
   Process starts, so a taken port fails startup. It is WebSocket-only, declares `AcceptsStatus` and
   `AcceptsEffectiveConfig`, and folds what the process reports into the owning Agent, whose
   `instance_uid` stays the Supervisor's. For a Managed Process that speaks no OpAMP nothing ever
   connects, and that is the whole of the handling. The Supervisor Endpoint is not a mode.

3. **Neither end keys state on a connection.** The Server indexes Agents by `instance_uid` only; the
   Client maps Agents onto connections without assuming one-to-one. *n* Agents ride *m* connections
   (*n* ≥ *m* ≥ 1), and the pool size is a deployment choice, not a consequence of the Agent count.

4. **`[gateway]` arms Gateway Mode.**

   | Key | Default | Rule |
   |---|---|---|
   | `listen` | none | Required. A Gateway that binds nothing is a configuration error; loopback is the Supervisor Endpoint's job. A bind failure fails startup. |
   | `upstream_connections` | `10` | The pool's cap, not its size. `0` is refused at load. |
   | `max_carried_agents` | `10000` | The most distinct Agents one downstream connection may carry. Past it a report for a *new* Agent is dropped and logged; Agents already carried keep working. `0` is refused at load. |
   | `[gateway.tls]` | absent | `cert_file`, `key_file`, optional `client_ca_file` for the downstream hop. |

   The Client's own `endpoint` must be `ws://` or `wss://` when `[gateway]` is present: a polling
   connection cannot carry the Server's pushes to the Agents behind the Gateway.

5. **The downstream endpoint serves both transports.** A downstream Client picks its transport by
   the scheme of its endpoint ([ADR-0012](0012-transports-tls-and-the-servers-two-planes.md)), so the
   Gateway serves a WebSocket upgrade and a plain-HTTP POST on `/v1/opamp`, with the Baseline's
   message size limit, protobuf media type and gzip rules enforced per hop through the shared
   `opamp::endpoint` helpers ([ADR-0011](0011-workspace-crates-and-configuration.md) clause 7).

6. **axum serves it, as an ordinary dependency of the Client crate behind no Cargo feature.** The
   mode lives in `crates/fleet-agent/src/gateway/` (`mod.rs` the endpoint, `pool.rs` the upstream pool,
   `registry.rs` the downstream routes), beside `supervisor/`. The HTTP engine under axum is linked
   through `reqwest` anyway; axum adds the routing layer. TLS on the downstream hop uses the same
   `axum-server` rustls terminator as the Server.

7. **An Agent is assigned to the least-loaded live connection, and stays there.** Assignment is by
   `instance_uid` and sticky while that connection lives, which keeps an Agent's `sequence_num`
   stream and its `ReportFullState` exchanges on one upstream socket.

8. **The pool grows lazily to its cap, and never beyond.** The first Agent opens the first
   connection; a new one opens only when every live connection already carries an Agent and the cap
   is not reached. Past the cap, Agents share. Existing Agents are not re-balanced when a connection
   is added.

9. **A dropped upstream connection re-homes its Agents and says nothing on their behalf.** An Agent
   whose connection is gone is re-assigned by clause 7 on its next report, which is forwarded over
   the new connection. The Gateway sends no `agent_disconnect` for it. The Server marks those Agents
   disconnected because their owning connection dropped, and their next report corrects that.

10. **A downstream Client that vanishes is reported by its absence, not by a message.** An
    `agent_disconnect` it sends is forwarded like any other message. If it just goes away, its routes
    are released and the Gateway forwards nothing. Such an Agent stays connected in the fleet view
    with a `last_seen_ms` that stops advancing, and reads as stale once its staleness budget runs out
    when it declared `ReportsHeartbeat` ([ADR-0026](0026-the-fleet-record.md)).

11. **The Gateway makes no authentication decisions; its own hop is its own.** The downstream peer's
    `Authorization` credential is forwarded upstream untouched, so admission stays on the Server
    ([ADR-0017](0017-admission-and-authentication.md)) and a credential change never reaches a
    gateway's configuration. Mutual TLS is per hop: with `client_ca_file` set, a downstream client
    certificate is mandatory and must chain to it; upstream the Gateway presents the Client's own
    identity. A downstream certificate is never forwarded, and no header is invented for it. Without
    `[gateway.tls]` the hop is plaintext, and startup logs a warning saying so.

12. **The Gateway is its own Agent, and only its own.** The Client presents its own Agent upstream
    exactly as any Client does, and never presents itself as an Agent it carries.

13. **Routing is by `instance_uid` alone, in both directions.** Upward a report goes out on its
    Agent's connection. Downward the Gateway looks up, per `instance_uid`, the downstream connection
    or pending plain-HTTP exchange that last carried the Agent; a `ServerToAgent` for an Agent it
    does not carry is dropped with a log line, never broadcast.

**Out of scope:** re-balancing Agents across a grown pool; consolidating the three places TLS
material is configured on a Client.

## Alternatives considered

- **Separate deployables (a supervisor binary, a gateway binary).** Both shapes share their whole
  OpAMP stack, so this multiplies packaging and release surface to express a startup choice, and it
  forecloses a machine that supervises its own processes and fronts others.
- **A third, independently selectable "local server" mode.** It would let an operator configure
  combinations that cannot work and put a conditional on a path that has no reason to vary.
- **Enable the Supervisor Endpoint only for Collector Supervisors, or per block.** More precise, but
  it buys that with a configuration surface and a branch to avoid an idle loopback listener. The
  `opampsupervisor` brings its local server up unconditionally too.
- **One connection per Agent, or multiplexing only in Gateway Mode.** The first makes Gateway Mode
  impossible and induces the connection-equals-agent assumption on the Server; the second leaves two
  routing models in the code while the Server must support the general one anyway.
- **Authentication in the Gateway.** The specification places authentication on the Server; a
  gateway that decides duplicates policy and forces credential rotation to reach every gateway.
- **A fixed pool opened at startup.** The prior art's shape; it makes the pool a cost paid before
  any Agent arrives.
- **Round-robin instead of least-connections.** With sticky assignment it distributes arrivals
  rather than load, so a Gateway that loses and regains a connection stays lopsided.
- **Re-balancing Agents when a connection is added.** It trades stickiness for an even spread nobody
  asked for, and moving a stream between sockets mid-flight invites reordering.
- **Synthesising `agent_disconnect` for a vanished downstream Client.** The message would say the
  Agent said goodbye when it did not. Server-side staleness is the honest signal.
- **A WebSocket-only downstream endpoint.** The Baseline lets a Client choose either transport; a
  Gateway that silently excludes polling Clients fails as a Client that connects to nothing.
- **Hand-rolling the plain-HTTP endpoint on `hyper`.** One route sounds small until the Baseline's
  `413`, `415` and gzip rules are counted.
- **Gateway Mode behind a Cargo feature.** The released binary must have it on (goal 15, one binary
  for every shape), so it saves nothing where claimed, and a default-off feature is a branch that
  `cargo test` and `cargo clippy` never compile.
- **A crate of its own, `crates/gateway`.** It needs the configuration, TLS material, identity,
  transports and Agent state of the Client crate, so it would depend back on `client`. A crate is not
  a feature boundary either.
- **A Layer-4 passthrough gateway.** It carries client certificates end to end but cannot read OpAMP,
  so it cannot fold *n* Agents onto *m* connections.
- **Reusing the Server crate for the downstream endpoint.** The Client would depend on the Server,
  and the downstream side forwards rather than processes.

## Sources / Prior art

- [OpAMP specification, `ServerToAgent.instance_uid`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — the protocol's own provision for multiplexing through a terminating proxy.
- [OpAMP Gateway Extension](https://bindplane.com/blog/opamp-for-opentelemetry-managing-collector-fleets-and-introducing-the-new-opamp-gateway-extension)
  — an OpAMP server downstream and a client upstream, a pool of ten by default with least-connections
  balancing, messages forwarded unchanged, authentication delegated to the Server. Alpha; a design
  reference, not a dependency.
- [`opampsupervisor` specification](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/specification/README.md),
  [`supervisor/supervisor.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/supervisor.go)
  and [`supervisor/config/config.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/config/config.go)
  — a local OpAMP server started unconditionally on `localhost:<port>` (`agent::opamp_server_port`,
  random free port when unset), with the extension's configuration injected from an embedded template.
- [`opampextension`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension)
  — a client only, with a deliberately small capability set.

## Consequences

- Positive: one deployable to build, sign, install and update on every platform. A fleet scales
  connections independently of its Agent count, and the Server needs no change to sit behind a
  Gateway.
- Positive: the pool costs what it uses, so the default cap needs no tuning for small deployments and
  still bounds large ones.
- Positive: binding the Supervisor Endpoint to the Supervisor removes a class of invalid
  configurations instead of validating against it.
- Negative / trade-offs: every Supervisor binds a loopback port whether or not its Managed Process
  uses it.
- Negative / trade-offs: losing one pooled connection marks every Agent riding it disconnected until
  each reports again: one heartbeat interval with a heartbeat configured, otherwise until the Agent
  has something to say.
- Negative / trade-offs: a downstream Client that vanishes without a goodbye stays connected in the
  fleet view; only staleness tells, and only for an Agent that declared `ReportsHeartbeat`.
- Negative / trade-offs: the Gateway holds no authentication of its own, so its port is reachable
  until the Server rejects the peer; the Server's admission is load-bearing.
- Negative / trade-offs: the binary carries an HTTP routing layer it uses only in Gateway Mode, and
  Supervisor Mode plus Gateway Mode in one process is a real test surface.
- Follow-ups: re-balancing when the pool grows, if a real fleet is ever lopsided enough; consolidating
  TLS material on the Client if a fourth place appears.

## Enforcement

- [`crates/fleet-agent/tests/gateway_e2e.rs`](../../crates/fleet-agent/tests/gateway_e2e.rs):
  `two_agents_reach_the_server_over_one_folded_connection` (clauses 3, 13),
  `one_agent_opens_one_upstream_connection` (clause 8),
  `a_downstream_connection_carries_no_more_than_its_agent_cap` (clause 4),
  `a_downstream_peer_without_the_protobuf_content_type_is_refused`,
  `a_downstream_peer_may_gzip_its_report`, `a_gzip_bomb_is_refused_by_the_gateway` and
  `an_oversized_downstream_message_closes_with_1009` (clause 5).
- [`crates/fleet-agent/tests/gateway_tls.rs`](../../crates/fleet-agent/tests/gateway_tls.rs):
  `a_downstream_agent_with_a_certificate_reaches_the_server_over_tls`,
  `a_downstream_peer_without_a_certificate_is_refused` and
  `the_tls_endpoint_does_not_answer_plaintext` (clause 11).
- [`crates/fleet-agent/tests/gateway_and_supervisor_e2e.rs`](../../crates/fleet-agent/tests/gateway_and_supervisor_e2e.rs):
  `a_host_supervises_and_gateways_at_the_same_time`,
  `a_verified_offer_restarts_the_gateway_and_leaves_the_supervisors_running` and
  `a_gateway_that_cannot_bind_is_loud` (clauses 1, 4).
- [`crates/fleet-server/tests/ws_transport.rs`](../../crates/fleet-server/tests/ws_transport.rs)
  `two_agents_share_one_connection` and
  [`crates/fleet-agent/tests/e2e.rs`](../../crates/fleet-agent/tests/e2e.rs)
  `a_config_change_reaches_both_supervised_agents_over_one_connection` (clause 3).
- `crates/fleet-agent/src/supervisor/endpoint.rs`: `extension_reports_are_folded_into_process_events` and
  `shutdown_stops_the_endpoint` (clause 2); `crates/fleet-agent/src/config.rs`:
  `the_gateway_agent_cap_defaults_and_rejects_zero` (clause 4).

**Not mechanically decidable:** that no message is synthesised on an Agent's behalf (clauses 9, 10)
is an absence; review holds it.
