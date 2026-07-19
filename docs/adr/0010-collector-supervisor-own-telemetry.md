# ADR-0010: The Collector Supervisor reports the Collector's own telemetry, configured from the Server's connection settings

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

[ADR-0008](0008-collector-supervisor-go-reference-compat.md) commits the Collector Supervisor to
**feature compatibility with the upstream Go OpAMP Supervisor**, the behavioural oracle from
[ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md). One capability of that oracle is still
missing from the Rust supervisor: **own telemetry**. The Go supervisor lets the Server route the
*Collector's own* metrics, logs, and traces to a destination of the Server's choosing, and reports that
it does so through the `ReportsOwnMetrics` / `ReportsOwnLogs` / `ReportsOwnTraces` capabilities.

The OpAMP protocol models this as a Server→Agent offer. In [`opamp.proto`](../../crates/opamp-proto/proto/opamp/v1/opamp.proto):

- `AgentCapabilities.ReportsOwnMetrics` (`0x40`), `ReportsOwnLogs` (`0x80`), `ReportsOwnTraces`
  (`0x20`) advertise that the Agent will report its own telemetry if the Server offers a destination.
- `ServerToAgent.connection_settings` (`ConnectionSettingsOffers`) carries `own_metrics`, `own_logs`,
  and `own_traces`, each a `TelemetryConnectionSettings` with a `destination_endpoint` (an OTLP/HTTP
  URL), optional `headers` (e.g. an auth token), and optional `certificate` / `tls` / `proxy`.

Today our supervisor declares none of these capabilities ([`supervisor.rs`](../../crates/opamp-supervisor/src/supervisor.rs),
`CAPABILITIES`) and reads only `heartbeat_interval_seconds` out of `connection_settings`; the
`own_metrics` / `own_logs` / `own_traces` offers are ignored. So the fleet cannot ask a managed
Collector to ship its process metrics/logs/traces anywhere, and the Server sees an Agent that claims not
to support it.

This is **architecture-relevant** (hence this ADR, per `AGENTS.md` §3): declaring a capability is a
**wire-level promise**, and honouring the offer requires a **mechanism** decision — how the supervisor
turns a `TelemetryConnectionSettings` into running Collector behaviour, and how that composes with the
remote config, the base config (ADR-0008 follow-up), and the deferred TLS/auth work
([ADR-0008](0008-collector-supervisor-go-reference-compat.md) holds TLS out of scope).

## Decision

We will make the Collector Supervisor **report the Collector's own telemetry by translating the
Server's `TelemetryConnectionSettings` into the Collector's `service.telemetry` configuration**, the
same mechanism the Go supervisor uses.

- **Declare the capabilities it honours.** Add `ReportsOwnMetrics`, `ReportsOwnLogs`, and
  `ReportsOwnTraces` to the declared `CAPABILITIES`, each **gated by supervisor configuration**
  (`own_metrics` / `own_logs` / `own_traces`, defaulting to match the Go supervisor). We declare a
  capability only when we are configured to act on it — consistent with ADR-0008's "declare exactly what
  we implement".
- **Route through the Collector, not the supervisor.** On a `ServerToAgent` carrying
  `connection_settings.own_{metrics,logs,traces}`, the supervisor injects the offered
  `destination_endpoint` (and `headers`) into the Collector's `service.telemetry.{metrics,logs,traces}`
  as an OTLP/HTTP exporter, merges that into the config it applies, and **restarts the Collector** to
  apply it — the same file-plus-restart model as remote config (ADR-0008). The supervisor does not
  export telemetry itself; the Collector is the telemetry engine.
- **Layer it deterministically.** Own-telemetry config is merged **on top of** the remote config (which
  is itself on top of the base config, ADR-0008 follow-up), so a Server telemetry offer wins over
  whatever `service.telemetry` the configs carry. Restart only on an **actual change** to the
  effective own-telemetry settings — a re-offer of the same destination must not restart the Collector
  (ADR-0008's "a spurious restart is a spurious outage").
- **Persist the last offer** alongside the applied config in the storage dir, so a supervisor restart
  resumes reporting to the same destination without waiting for a re-offer.
- **Scope now:** honour `destination_endpoint` and `headers` over plain OTLP/HTTP, matching the
  unauthenticated, plain-transport scope of ADR-0008 and [ADR-0006](0006-rust-opamp-server-from-spec.md).
  The `certificate` / `tls` fields of `TelemetryConnectionSettings` are **deferred to the TLS/auth ADR**
  that ADR-0008 already foresees; until then an `https://` destination is used as offered (no custom CA
  / client cert), and we do not silently pretend to a security property we do not implement.

## Alternatives considered

- **Export the supervisor's own process metrics directly (bypass the Collector).** Rejected: it
  diverges from the oracle, duplicates an OTLP pipeline the Collector already is, and would report
  telemetry about the supervisor process rather than the managed Collector the fleet cares about.
- **Statically configure self-telemetry, ignore the Server's connection settings.** Rejected: it drops
  the actual OpAMP loop (the Server chooses the destination) and would make `ReportsOwnMetrics` a false
  claim when the Server's offer is ignored.
- **Declare the capabilities now, wire the mechanism later.** Rejected: declaring a capability we do not
  honour is exactly the false-promise ADR-0008 warns against; capability and mechanism land together.
- **Do nothing (status quo).** Rejected here: it leaves a named ADR-0008 parity gap open; this ADR is
  the increment that closes it.

## Sources / Prior art

- OpAMP specification — own telemetry and connection settings offers:
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- The vendored schema this builds on: `TelemetryConnectionSettings`, `ConnectionSettingsOffers`,
  and the `ReportsOwn*` capabilities in
  [`crates/opamp-proto/proto/opamp/v1/opamp.proto`](../../crates/opamp-proto/proto/opamp/v1/opamp.proto).
- The behavioural oracle — the upstream OpAMP Supervisor's own-telemetry handling (translating the
  offer into the Collector's `service.telemetry`):
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>.
- The Collector's `service.telemetry` configuration surface:
  <https://opentelemetry.io/docs/collector/internal-telemetry/>.
- Builds on: [ADR-0008](0008-collector-supervisor-go-reference-compat.md) (Collector Supervisor scope
  and the config file-plus-restart model), [ADR-0006](0006-rust-opamp-server-from-spec.md) (plain,
  unauthenticated transport for now).

## Consequences

- Positive: the fleet can direct a managed Collector's own metrics/logs/traces to a destination it
  chooses, closing a named ADR-0008 parity gap; the Agent's declared capabilities match its behaviour.
- Negative / trade-offs: another input to the config the supervisor composes (base → remote →
  own-telemetry), so the merge order and its tests grow; a telemetry-settings change costs a Collector
  restart (mitigated by restarting only on an actual change); handling `connection_settings` now spans
  more than the heartbeat, enlarging the `ServerToAgent` handler.
- Follow-ups: the **TLS/auth ADR** must cover `TelemetryConnectionSettings.certificate` / `tls` (and the
  matching OpAMP transport TLS) so an authenticated telemetry destination is fully honoured; the
  `other_connections` offer and `AcceptsOtherConnectionSettings` remain out of scope for their own ADR.
