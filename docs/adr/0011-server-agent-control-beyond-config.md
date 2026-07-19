# ADR-0011: The Server drives Agents beyond config distribution — own-telemetry offers, a restart command, and a heartbeat interval

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The Collector Supervisor (the Agent side) now implements the full Server→Agent control surface of the
OpAMP loop: it honours **own-telemetry connection-settings offers**
([ADR-0010](0010-collector-supervisor-own-telemetry.md)), the **`AcceptsRestartCommand`**, and a
**Server-set heartbeat interval** ([ADR-0008](0008-collector-supervisor-go-reference-compat.md)). But the
Rust Server never **sends** any of these. Its reply is a single comparison
([ADR-0006](0006-rust-opamp-server-from-spec.md)): [`build_reply`](../../crates/opamp-server/src/server.rs)
only ever carries `remote_config` and the `ReportFullState` flag. So three Agent capabilities go
unexercised against our own Server, and — most visibly — **[ADR-0010](0010-collector-supervisor-own-telemetry.md)
own-telemetry cannot be triggered end-to-end** without a fuller OpAMP server (a Go/Bindplane one).

Two accepted ADRs shape this deliberately:

- [ADR-0006](0006-rust-opamp-server-from-spec.md) keeps the Server **minimal** ("the control loop is a
  single comparison").
- [ADR-0007](0007-rest-api-and-fleet-ui.md) made **writing `config/collector.yaml` the only way to
  reconfigure the fleet** — the API/UI deliberately has **no per-agent command path**.

Sending per-agent commands and connection-settings offers is therefore a **new control surface** beyond
both — architecture-relevant, hence this ADR. [ADR-0010](0010-collector-supervisor-own-telemetry.md)
decided only the Agent side of own telemetry and explicitly left the **Server side** — making the offer,
and where the destination comes from — open.

## Decision

We will **extend the Server to drive Agents with three explicit control offers beyond config
distribution**, sourced from the Server's own configuration and (for restart) a targeted operator action.
This **extends** [ADR-0007](0007-rest-api-and-fleet-ui.md)'s control surface; it does **not** reverse its
rule that config-file writes are the only *config-distribution* path — it adds *separate* channels for
things a config file cannot express.

- **Own-telemetry offers (completes [ADR-0010](0010-collector-supervisor-own-telemetry.md)).** A new
  Server configuration section names the OTLP/HTTP **destination endpoint (and optional headers)** to
  offer for `own_metrics` / `own_logs` / `own_traces`. The Server includes them in
  `ServerToAgent.connection_settings` for an Agent that declares the matching `ReportsOwn*` capability,
  and declares the corresponding `ServerCapabilities` (`OffersConnectionSettings`). The Agent already
  translates the offer into the Collector's `service.telemetry` (ADR-0010).
- **Heartbeat interval.** A Server configuration value sets
  `connection_settings.opamp.heartbeat_interval_seconds`, so the fleet's report cadence is Server policy.
- **Restart command.** A **targeted operator action** — `POST /api/agents/{uid}/restart`, and a per-agent
  button on the fleet page — makes the Server send a `ServerToAgentCommand{Restart}` to that agent's
  connection. This is an explicit, per-agent control action, **distinct from config distribution**.
- **When offers are sent.** On the Agent's first / full-state report, and whenever the configured offer
  changes. The Agent **de-duplicates** — an unchanged own-telemetry offer or heartbeat does not restart
  the Collector (ADR-0010, ADR-0008) — so re-sending is safe and cannot cause a spurious outage.
- **Scope now.** Plain OTLP/HTTP destinations and the unauthenticated `:4321` surface, matching
  [ADR-0006](0006-rust-opamp-server-from-spec.md)/[ADR-0007](0007-rest-api-and-fleet-ui.md)/[ADR-0010](0010-collector-supervisor-own-telemetry.md).
  The `certificate` / `tls` fields of the telemetry offer stay **deferred to the TLS/auth ADR** (as
  ADR-0010 already holds), and the new mutating `restart` endpoint inherits the unauthenticated trust
  model that ADR-0007 flags — making the deferred auth work more pressing, not less.

## Alternatives considered

- **Server-assigned instance UID (`agent_identification.new_instance_uid`).** Rejected (**YAGNI**): the
  Agent already persists a stable UUIDv7 ([ADR-0008](0008-collector-supervisor-go-reference-compat.md));
  Server assignment only resolves UID *collisions*, which is not a present need and would add
  identity-tracking and persistence to the deliberately minimal Server for no current benefit. Left out
  until a concrete collision problem exists.
- **Do nothing (status quo).** Rejected here: it leaves ADR-0010 un-exercisable against our own Server
  and three Agent capabilities dead on this side — the named parity gap stays open.
- **Put every trigger in the config file (no API/UI command path).** Rejected: a restart is a *per-agent*
  action, which a fleet-wide file cannot express; the telemetry destination and heartbeat *are* Server
  policy and fit a config value, but the restart trigger needs a targeted call.
- **Overload `PUT /api/config` for all of it.** Rejected: config distribution and per-agent commands are
  different actions; conflating them into the config write muddies the stable contract
  ([ADR-0007](0007-rest-api-and-fleet-ui.md)).
- **Implement the Agent-side de-duplication on the Server instead (track per-agent applied offers to
  avoid re-sending).** Deferred as an optimisation: the Agent already de-duplicates, so the Server may
  re-send safely; per-agent offer bookkeeping on the Server is only needed if re-sends become costly.

## Sources / Prior art

- OpAMP specification — `ConnectionSettingsOffers` / `TelemetryConnectionSettings`,
  `ServerToAgentCommand`, `AgentIdentification`, and `ServerCapabilities`:
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- The vendored schema this builds on — `connection_settings`, `command`, `ServerCapabilities` in
  [`crates/opamp-proto/proto/opamp/v1/opamp.proto`](../../crates/opamp-proto/proto/opamp/v1/opamp.proto).
- The Agent side this completes: [ADR-0010](0010-collector-supervisor-own-telemetry.md) (own telemetry),
  [ADR-0008](0008-collector-supervisor-go-reference-compat.md) (restart command, heartbeat interval).
- The Server model it extends: [ADR-0006](0006-rust-opamp-server-from-spec.md) (minimal server),
  [ADR-0007](0007-rest-api-and-fleet-ui.md) (control surface, config-only distribution).
- The behavioural oracle — the upstream Go/Bindplane OpAMP servers that do send these offers:
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>.

## Consequences

- Positive: [ADR-0010](0010-collector-supervisor-own-telemetry.md) own-telemetry becomes **end-to-end
  testable against our own Server**; the Agent's `AcceptsRestartCommand` and Server-set heartbeat are
  exercised; the fleet can direct a managed Collector's own telemetry to a destination it chooses and
  restart a single agent from the API/UI.
- Negative / trade-offs: the Server grows a control surface **beyond the single comparison**
  ([ADR-0006](0006-rust-opamp-server-from-spec.md)) and **beyond config-file-only**
  ([ADR-0007](0007-rest-api-and-fleet-ui.md)) — more Server state and a **new mutating API/UI path**
  (restart) on the still-unauthenticated `:4321`, which raises the stakes of the deferred authentication
  work. A new Server configuration section must be designed and kept stable.
- Follow-ups: the **TLS/auth ADR** must cover the telemetry offer's `certificate` / `tls` fields and
  **authenticating the `restart` endpoint** before `:4321` is exposed; `AcceptsOpAMPConnectionSettings`
  (re-pointing the OpAMP connection) and `AcceptsOtherConnectionSettings` remain their own future ADRs.
