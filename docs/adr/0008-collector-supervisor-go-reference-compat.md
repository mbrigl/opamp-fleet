# ADR-0008: The Supervisor Host's first supervisor — an OpAMP-native Collector Supervisor, feature-compatible with the Go reference Supervisor

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The [specification](../SPECIFICATION.md) makes the **Supervisor Host** the client that runs many
**Supervisors** as plugins behind a hexagonal core, managing OpenTelemetry Collectors and — later —
non-OpAMP Foreign Agents. So far the `opamp-supervisor` crate is only a skeleton
([ADR-0005](0005-cargo-workspace-layout.md) named the plugin/hexagonal structure a later ADR).

The first useful thing the Host can host is the one the ecosystem already proves: an **OpAMP-native
Collector Supervisor**. The development environment runs the upstream **OpenTelemetry OpAMP Supervisor**
(Go) as the `opamp-agent` sidecar, kept as the **behavioural oracle**
([ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md)). This milestone is: our Collector
Supervisor reaches **feature compatibility with that Go reference Supervisor** — for the same
configuration it connects to the same Server, reaches the same config hash, and reports the same status.

What "feature compatibility with the Go reference Supervisor" concretely means (from the upstream
supervisor's behaviour):

- Connect to the Server over the OpAMP WebSocket and keep the session open, reconnecting with backoff.
- Persist a stable 16-byte **Instance UID** across restarts, and adopt a Server-assigned UID.
- Declare exactly the capabilities it implements, and report identity (`AgentDescription`).
- Receive **remote configuration**, **validate** it, write it to disk, and **restart** the Collector
  to apply it; report `APPLYING` → `APPLIED`/`FAILED` with the collector's own error on rejection.
- Report the Collector's **actual health and effective configuration** — obtained by running a **local
  OpAMP server** the Collector's bundled `opamp` extension connects back to (the same mechanism the Go
  supervisor uses), rather than the supervisor's assumptions.
- Ship a **startup fallback configuration** so the Collector runs before the Server answers.
- Send periodic **heartbeats**, honour a Server-set heartbeat interval, handle **sequence-number gaps**
  (`ReportFullState`) and the **restart command**.

## Decision

We will implement an **OpAMP-native Collector Supervisor** in the `opamp-supervisor` crate, over the
shared `opamp-proto` crate, and have the Supervisor Host process run it (one supervisor for now).

- **Owns a Collector process, does not embed it.** Applying a configuration is: **validate** it with
  the collector's own `validate` subcommand against a throwaway copy (so a bad config is reported
  `FAILED` *without* taking the running collector down), then write the file and **restart** the
  process. Restart only on an actual hash change — a spurious restart is a spurious outage.
- **A local OpAMP server for the Collector.** The supervisor injects the collector's `opamp` (pointed
  at a loopback local server) and `health_check` extensions into the distributed config, so the
  collector reports its real health and effective config back; the supervisor forwards those to the
  Server. This local server **only observes** — configuration is still applied by file + restart.
- **Identity & persistence.** A UUIDv7 Instance UID persisted under a storage dir; the applied config
  hash persisted next to the config so a supervisor restart resumes without re-applying.
- **Capabilities declared** (and no more): `ReportsStatus`, `AcceptsRemoteConfig`,
  `ReportsEffectiveConfig`, `ReportsHealth`, `ReportsRemoteConfig`, `ReportsHeartbeat`,
  `AcceptsRestartCommand`, `ReportsAvailableComponents`.
- **Dependencies:** `tokio-tungstenite` (the OpAMP WebSocket client, and the local server) and
  `serde_yaml` (to merge the `opamp`/`health_check` extensions into the collector config).
- **Correctness is measured against the oracle.** For a given configuration the Collector Supervisor
  must reach the same config hash and report the same status as the upstream Go Supervisor connected to
  the same Server ([ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md)). The Go sidecar is
  **not** replaced — it stays as the reference.
- **Scope now:** plain-`ws`, unauthenticated, no package updates — matching the initial Server
  ([ADR-0006](0006-rust-opamp-server-from-spec.md)). Deliberately **out of scope**, each under its own
  ADR: TLS + shared-token authentication, OpAMP **package/binary updates**, and the **plugin/hexagonal
  generalization** that lets the Host run *many* supervisors and **Custom Supervisors** for non-OpAMP
  Foreign Agents.

## Alternatives considered

- **Keep the skeleton; manage only the upstream Supervisor.** The smaller option, but the specification
  commits the project to owning the Agent side in Rust; this ADR delivers the first, feature-compatible
  increment of it.
- **Replace the Go Supervisor sidecar with ours.** Rejected: it destroys the behavioural oracle —
  "our agent is correct" would become unfalsifiable, the trap [ADR-0006](0006-rust-opamp-server-from-spec.md)
  warns about for the Server.
- **Fork/wrap `otel-opamp-rs` (the client crate).** Pre-0.1; couples the Agent to an unstable API for
  message types we already generate from the vendored schema. Rejected for the same reason
  [ADR-0006](0006-rust-opamp-server-from-spec.md) rejected it for the Server.
- **Embed the Collector in-process / apply config over the local OpAMP channel.** Rejected: the Go
  supervisor owns a *separate* collector process and applies config by file + restart; matching that is
  the point. The local OpAMP server observes health/effective config, it does not push config.
- **Build the plugin/hexagonal port abstraction now.** Premature (YAGNI) with a single supervisor type;
  the second type (a Custom Supervisor for a Foreign Agent) is what forces the ports, so that lands with
  it, under its own ADR.

## Sources / Prior art

- The oracle and the dev sidecar: [ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md); the
  requirement: [`SPECIFICATION.md`](../SPECIFICATION.md) (Supervisor Host, Collector Supervisor).
- OpAMP specification — the Agent (client) side, capabilities, `sequence_num`, `ReportFullState`,
  agent identification: <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- OpenTelemetry OpAMP Supervisor — the behaviour we mirror and check against (config apply, restart,
  `startup_fallback_configs`, the local OpAMP server for the collector's `opamp` extension, storage):
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>.
- The Collector `opamp` and `health_check` extensions:
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/extension/opampextension>.
- The protocol layer this builds on: [ADR-0006](0006-rust-opamp-server-from-spec.md),
  [ADR-0005](0005-cargo-workspace-layout.md).

## Consequences

- Positive: the whole control loop is now one Rust stack — Server *and* an Agent we shape to it — and
  the Agent is checked against a real oracle. The Collector's *actual* health and effective config reach
  the fleet via the local OpAMP server.
- Negative / trade-offs: a second component to keep correct against a moving alpha oracle; the Agent
  side of OpAMP is now ours to own, with its own bugs. The supervisor runs a second (loopback) listener
  for the collector. Scope must be held to the oracle's real capabilities to avoid pretending to
  features the ecosystem lacks.
- Follow-ups, each its own ADR: authenticated/encrypted transport; OpAMP package/binary updates
  (with the separate-updater handoff to update the supervisor itself); and the **plugin/hexagonal
  generalization** — many supervisors in one Host, and **Custom Supervisors** bringing non-OpAMP
  Foreign Agents into the same control loop.
