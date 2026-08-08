# ADR-0037: Gateway Mode — a lazily grown pool, sticky by `instance_uid`, and a hop that invents nothing

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

Gateway Mode is the last goal of the [specification](../SPECIFICATION.md) at zero. Goal 15 asks that
"many Clients reaching the Server through a Client in Gateway Mode appear as their own Agents, fully
manageable, while sharing a small Connection Pool — and the Gateway itself makes no authentication
decisions". [ADR-0003](0003-client-modes-and-connection-multiplexing.md) decided the shape — one
binary, two orthogonal modes, `instance_uid` as the sole routing key — and deliberately left three
questions open, in its own words: "the pool's sizing and balancing strategy, and the failure
semantics when a pooled connection drops, are design questions to settle when Gateway Mode is
implemented". This settles them.

**Almost everything else is already decided.** The Server has been written for *n* Agents over *m*
connections from the outset and is tested that way; it keys every piece of state on `instance_uid`
and marks Agents disconnected per connection, never per report.
[ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md) settled the security shape
that used to be the open question here: mutual TLS is **hop-by-hop** — Agent to Gateway, Gateway to
Server — and the per-Agent proof that survives a terminating hop is the credential, forwarded
unchanged. So a Gateway needs a client identity of its own upstream, which the CSR flow already
issues, and a server certificate plus a client CA downstream.

**The downstream side is not new either.** Every Supervisor already exposes an OpAMP *server* on
loopback ([`endpoint.rs`](../../crates/client/src/supervisor/endpoint.rs)): it accepts a WebSocket,
frames with `opamp::frame`, and enforces the Baseline's size limits in both directions. A Gateway's
downstream side is that same server side of the protocol, bound to a real address and **forwarding**
what it receives instead of folding it into an Agent.

Three forces shape what is left.

**A Gateway must not invent messages.** The specification's strategy forbids resolving a protocol
gap "by inventing semantics of this project's own", and ADR-0003 requires that a Gateway "forwards
messages unchanged". That constrains the failure semantics more than anything else: when something
goes wrong on one side, the tempting fix is to synthesise a message toward the other, and that is
exactly what is not available.

**Connections are not identities, in both directions.** The Server marks an Agent disconnected only
when the connection that *owns* it drops. Behind a Gateway the owning connection is the Gateway's,
so upstream connection state says nothing about whether a downstream Client is alive — and
downstream connection state must not be reported upstream as if the Agent had said it.

**A pool that is a fixed cost is a pool nobody wants small.** Prior art — the OpAMP Gateway
Extension — opens a configurable pool, ten by default, and balances least-connections. Ten
connections for a gateway serving twelve Agents is worse than one, and an operator who has to tune
the number down per deployment will not.

## Decision

We will implement **Gateway Mode as a `[gateway]` section** that arms a downstream OpAMP endpoint on
both transports and an **upstream pool grown lazily to a configured cap**, with each Agent **stuck to
one connection by `instance_uid`**, and a hop that **forwards messages unchanged and synthesises
none**.

1. **`[gateway]` arms the mode, and composes with Supervisor Mode.** A `listen` address (no default —
   a gateway that binds nothing is a configuration error, and loopback is the Supervisor Endpoint's
   job), `upstream_connections` (the cap, default 10 after prior art), and an optional `[gateway.tls]`
   with `cert_file`, `key_file`, and `client_ca_file` for the downstream hop. A Client may run
   Gateway Mode alone, or beside `[[supervisor]]` blocks on the same host — ADR-0003's orthogonality,
   unchanged.

2. **Both transports downstream.** A downstream Client selects its transport by the scheme of its
   endpoint (ADR-0007), so a Gateway that spoke only WebSocket would silently exclude every polling
   Client. The downstream endpoint therefore serves a WebSocket upgrade and a plain-HTTP POST on
   `/v1/opamp`, exactly as the Server does, with the Baseline's size limits enforced per hop.

3. **The downstream endpoint is served with axum, promoted to a real dependency of the Client — in
   the Client's own crate, behind no Cargo feature.** The Client serves no HTTP today: the
   Supervisor Endpoint is raw `tokio-tungstenite` and WebSocket-only, and axum is presently a
   *dev*-dependency, pulled in so a test can run the real Server in-process. Point 2 needs a server,
   and axum is the workspace's HTTP stack already (ADR-0005) — a second one would be the worse
   answer.

   **The cost is smaller than it first looks, and it was worth measuring before deciding.** The
   Client already links `hyper`, `http`, `http-body`, `tower`, `tower-http`, and `hyper-util`
   through `reqwest`, so what axum adds is a routing layer over an HTTP engine that is in the binary
   either way — not the engine itself.

   The mode lives in `crates/client/src/gateway/`, beside `supervisor/`: the same shape ADR-0011
   already uses for a self-contained subsystem with a port boundary, and the place where the
   configuration, TLS material, client identity, transports, and Agent state it needs all already
   are.

4. **An Agent is assigned to the least-loaded connection, and stays there.** Assignment is by
   `instance_uid` and is sticky for as long as that connection lives. Least-connections is the prior
   art's rule and needs no better one; stickiness is what keeps an Agent's `sequence_num` stream and
   its `ReportFullState` exchanges coherent on a single upstream socket, which nothing in the
   protocol requires but everything in debugging wants.

5. **The pool grows lazily to its cap, and never beyond.** Connections are opened as Agents appear:
   the first Agent opens the first connection, and a new one is opened only when every existing
   connection already carries at least one Agent and the cap is not reached. A Gateway serving three
   Agents holds three connections, not ten. Past the cap, Agents share.

6. **A dropped upstream connection re-homes its Agents, and says nothing on their behalf.** The
   Gateway reconnects with the transport's existing backoff, re-assigns the Agents that rode the lost
   connection by rule 4, and forwards their next reports over the new one. It sends no
   `agent_disconnect` for them: they never disconnected, and the Server keys state on `instance_uid`
   rather than on the connection, so nothing about their fleet state is lost. What the Server *does*
   do — mark those Agents disconnected because the connection that owned them dropped — is corrected
   by their next report, which is the same healing the direct case already relies on.

7. **A downstream Client that vanishes is reported by its absence, not by a message.** If it sends
   `agent_disconnect`, that is forwarded like any other message. If it simply goes away, the Gateway
   forwards **nothing**: fabricating a goodbye the Agent never said is exactly the invention ADR-0003
   and the specification's strategy forbid. Such an Agent stays "connected" in the fleet view, with a
   `last_seen_ms` that stops advancing, until the Server grows liveness of its own. That is a real
   gap, and it is named in the Consequences rather than papered over.

8. **The Gateway makes no authentication decisions, and its own hop is its own.** Downstream
   credentials are forwarded upstream untouched (ADR-0003, ADR-0013), and the connecting peer's
   address rides along. Mutual TLS is per hop (ADR-0035): downstream peers are verified against
   `[gateway.tls] client_ca_file` if one is configured, and the Gateway presents its *own* client
   identity upstream — obtained through the same CSR flow every Client uses. A downstream
   certificate is never forwarded; it cannot be, and inventing a header for it is the private side
   channel the specification forbids.

9. **The Gateway is its own Agent too, and only its own.** It presents the Client's self-Agent
   upstream exactly as any Client does — reporting its own version, health, and own telemetry — and
   it never presents itself as any Agent it carries. Its own Agent rides the pool like the others.

10. **Routing is by `instance_uid` alone, in both directions.** Downstream to upstream needs no
   routing at all — a report goes out on its Agent's connection. Upstream to downstream is a lookup:
   the Gateway keeps, per `instance_uid`, the downstream connection that last carried it, and a
   `ServerToAgent` for an Agent it has never seen is dropped with a log line rather than broadcast.

## Alternatives considered

- **A fixed pool opened at startup.** The prior art's shape, and simpler: no growth rule, no
  per-Agent assignment decision on first sight. Rejected in point 3 — it makes the pool a cost paid
  before any Agent arrives, which is the thing that makes an operator tune it down and get it wrong.
- **One upstream connection per downstream connection.** Trivially correct and needs no pool at all.
  Rejected: it is the absence of the feature. Folding connections is what Gateway Mode is for.
- **Round-robin instead of least-connections.** Cheaper and stateless. Rejected: with sticky
  assignment, round-robin distributes *arrivals* rather than *load*, so a Gateway that loses and
  regains a connection ends up lopsided until it restarts.
- **Re-balancing existing Agents when a connection is added.** The natural companion to lazy growth:
  each new connection could take a share of what the others carry. Rejected for now — it trades
  stickiness (point 4) for an even spread nobody has asked for, and moving an Agent's stream between
  sockets mid-flight is a source of reordering that is hard to reason about. If a real fleet ever
  ends up lopsided enough to matter, that is a follow-up with evidence behind it.
- **Synthesising `agent_disconnect` for a downstream Client that vanished.** It would make the fleet
  view correct, and a terminating proxy is arguably entitled to speak for the connection it
  terminated. Rejected in point 6: the message would say "this Agent said goodbye" when it did not,
  and this project does not put words in an Agent's mouth. The honest fix is Server-side liveness,
  which is a decision of its own.
- **A WebSocket-only downstream endpoint**, reusing what the Supervisor Endpoint already is. No new
  dependency, and every Client this project ships defaults to WebSocket anyway. Rejected: the
  Baseline lets a Client "choose either" transport, and a Gateway that silently excludes the polling
  half is one whose failure mode is a Client that connects to nothing with no explanation.
- **Hand-rolling the plain-HTTP endpoint** on the `hyper` already in the tree, avoiding axum on the
  Client. Rejected: it is one POST route and a size limit, which sounds small until the Baseline's
  `413`, `415`, and gzip rules are counted — all of which the Server already implements against a
  framework rather than by hand.
- **Putting Gateway Mode behind a Cargo feature**, so a Client that never gateways compiles none of
  it. Rejected twice over. A feature only saves an operator anything if the *released* artifact has
  it off — and the specification's "one binary covers every shape" plus goal 15 mean the release
  must have it on, so it would save nothing where it is claimed to and only help source builds. And
  a default-off feature is a branch that `cargo test` and `cargo clippy` never compile, which is the
  same failure mode this project already lives with for platform-gated code: invisible locally,
  caught in CI if at all. Buying a small binary with a permanently under-compiled branch is the
  wrong trade.
- **A crate of its own, `crates/gateway`.** Tempting for a subsystem this size, and it would read
  well beside `opamp`, `server`, and `client`. Rejected: a crate is not a feature boundary, so the
  dependency lands in the binary all the same the moment the binary uses it; the gateway needs the
  configuration, TLS material, client identity, transports, and Agent state that live in the Client
  crate, so it would either depend back on `client` — a layer for no gain — or force those out,
  which is churn against ADR-0024's "the Client is a library with a thin binary on top". A separate
  *binary* is not on the table at all: ADR-0003 rejected separate deployables outright, and that is
  binding.
- **Terminating nothing — a Layer-4 passthrough gateway.** It would carry client certificates end to
  end, which point 7 cannot. Rejected in ADR-0035 already, and again here: a gateway that cannot read
  OpAMP cannot fold *n* Agents onto *m* connections, so it solves reachability while giving up the
  connection scaling that is the goal.
- **Reusing the Server crate for the downstream endpoint.** It already serves both transports on
  `/v1/opamp`. Rejected: the Client would gain a dependency on the Server, ADR-0005's three-crate
  split exists to keep the two ends independent, and the downstream side needs to *forward* rather
  than to process — almost none of the Server's behaviour is wanted.

## Sources / Prior art

- [ADR-0003](0003-client-modes-and-connection-multiplexing.md) — the mode's shape and the three
  questions this ADR was told to answer; also the OpAMP Gateway Extension's pool default of ten and
  its least-connections balancing, and the rule that a Gateway forwards messages unchanged and makes
  no authentication decisions.
- [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md) point 10 — mutual TLS is
  hop-by-hop and the credential is the per-Agent proof that survives a terminating hop. This ADR
  inherits that rule rather than re-deciding it.
- [OpAMP Gateway Extension](https://bindplane.com/blog/opamp-for-opentelemetry-managing-collector-fleets-and-introducing-the-new-opamp-gateway-extension)
  — an OpAMP server downstream and an OpAMP client upstream, delegating all authentication to the
  Server above it: the same two-role shape, and the closest thing to a reference implementation.
- [OpAMP specification, `ServerToAgent.instance_uid`](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — the protocol's own provision for multiplexing "when a terminating proxy is used", which is the
  licence this whole mode rests on. Baseline `v0.19.0`.
- [`endpoint.rs`](../../crates/client/src/supervisor/endpoint.rs) — this project's existing server
  side of the protocol on the Client, and the template for the downstream endpoint.

## Consequences

- Positive: goal 15 is met and the specification has no goal left at zero. A fleet can grow past the
  point where one connection per Agent is affordable, and the Server needs no change at all — the
  `instance_uid` routing it was written for from the start is exactly what this exercises.
- Positive: the pool costs what it uses. A Gateway in front of three Agents holds three connections,
  so the default cap needs no tuning for small deployments and still bounds large ones.
- Negative / trade-offs: **a downstream Client that vanishes without a goodbye stays "connected"**
  in the fleet view until it is noticed by hand. This is the sharpest edge of the decision, and it
  follows from refusing to invent a message. Server-side liveness — marking an Agent stale after a
  missed heartbeat interval — is the fix, and it is a decision of its own rather than something to
  bolt on here.
- Negative / trade-offs: a lost upstream connection makes the Server mark every Agent riding it
  disconnected until each reports again. With a heartbeat configured that is one interval; without
  one it is until the Agent has something to say. The blast radius of a single connection is the
  price of folding connections at all, and ADR-0003 named it when it chose to.
- Negative / trade-offs: the Client binary grows a routing layer it only uses in Gateway Mode
  (point 3) — measured as small, since the HTTP engine under it is already linked, but not nothing.
  If it ever does matter, the lever is a Cargo feature, and that is a smaller decision *then*, with
  a number behind it, than a guess now.
- Negative / trade-offs: the Client grows a second listener and a second connection manager, and the
  combination of Supervisor Mode and Gateway Mode on one host is now a real test surface rather than
  a hypothetical one — as ADR-0003 predicted. It is covered in
  [`gateway_and_supervisor_e2e.rs`](../../crates/client/tests/gateway_and_supervisor_e2e.rs): both
  modes armed in one process, a gateway that cannot bind leaving the host's own management intact,
  and the interaction that only exists because they share a process — a verified offer restarting
  the gateway task while the Supervisors run straight through it.
- Follow-ups: re-balancing when the pool grows, if a real fleet is ever lopsided enough to warrant
  it. Server-side liveness for Agents behind a Gateway. And the Gateway's own downstream
  `[gateway.tls]` is a third place TLS material is configured on a Client — worth consolidating if a
  fourth ever appears.
