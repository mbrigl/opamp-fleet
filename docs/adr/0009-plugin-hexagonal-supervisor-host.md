# ADR-0009: A plugin/hexagonal Supervisor Host — many supervisors behind a `ManagedAgent` port, with a Custom Supervisor for non-OpAMP Foreign Agents

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

The [specification](../SPECIFICATION.md) defines the **Supervisor Host** as one process running **many
Supervisors** as plugins behind a hexagonal core, managing OpenTelemetry Collectors **and** non-OpAMP
**Foreign Agents** through Custom Supervisors (Goals 6–8). So far the Host runs exactly one supervisor,
and its OpAMP client loop is welded to a concrete `Collector`
([ADR-0008](0008-collector-supervisor-go-reference-compat.md)). Two things force the generalization
now, so it is no longer premature (the YAGNI objection [ADR-0008](0008-collector-supervisor-go-reference-compat.md)
raised):

1. Hosting **more than one** agent — the Goal-6 "one process, many Supervisors".
2. A **second, structurally different** agent type — a **Foreign Agent** that does not speak OpAMP and
   has no `opamp` extension to report health/effective config. This is exactly the second implementation
   the ports were waiting for.

The OpAMP client loop (connect, report identity, apply what the Server sends, report health/effective
config/status, heartbeat, reconnect, sequence gaps, restart command) is **agent-agnostic** — it is the
same for a Collector and a Foreign Agent. What differs is only *how you apply a config to the agent* and
*how you learn its health and effective config*. That difference is the port.

## Decision

We will restructure `opamp-supervisor` around a **`ManagedAgent` port**: the OpAMP client loop becomes a
generic `Supervisor<A: ManagedAgent>` (the hexagonal domain), and each concrete agent type is an adapter
(a plugin) implementing the port. The Host runs many supervisors concurrently, each its own OpAMP Agent.

- **The `ManagedAgent` port** (`agent.rs`) is what the domain depends on, never a concrete agent:
  - `prepare_config(config) -> config` — transform the remote config before applying (default:
    unchanged; the Collector injects its `opamp`/`health_check` extensions here).
  - `apply(config) -> Result<(), String>` — make a config take effect; `Err` is reported `FAILED`.
  - `restart()` — restart on the applied config (recovery / restart command).
  - `status() -> AgentStatus` — the agent's current health, and its effective config / description /
    available components when it reports them.
  - `change_signal() -> ChangeSignal` — a cloneable handle the loop awaits (without borrowing the agent)
    to forward a status change promptly; adapters with no push channel return one that never fires.
  - `supervise() -> Option<String>` — detect an unexpected exit and recover, returning the crash reason.
- **`Supervisor<A>` (the domain)** owns identity (persisted Instance UID), the control-loop state
  (applied hash/body, persisted under the supervisor's storage dir so a restart resumes without
  re-applying), reporting, heartbeat, reconnect, and sequence handling — all against the port.
- **Two adapters ship:**
  - **`CollectorAgent`** — the OpAMP-native Collector Supervisor from
    [ADR-0008](0008-collector-supervisor-go-reference-compat.md): owns an `otelcol` process, injects the
    `opamp` extension, and learns real health/effective config from the collector over a local OpAMP
    server. Its behaviour (validate → write → restart; oracle parity) is unchanged; only its seams move
    behind the port.
  - **`ProcessAgent`** — the **Custom Supervisor** for a **Foreign Agent** that does not speak OpAMP:
    it owns a plain process, applies a config by **writing a file and restarting** (or running a
    configured reload command), reports **process liveness** as health and **echoes** the written config
    as effective config. This is how a non-OpAMP agent is translated into the OpAMP control loop and
    appears in the fleet like any other Agent (Goal 7).
- **The Host runs many supervisors** (`host.rs`): each supervisor is spawned as its own task with its
  own OpAMP connection, Instance UID, and storage subdirectory, so the Server sees each as a distinct
  Agent (Goal 6). The Host owns startup and shutdown for all of them.
- **A host configuration file** (`serde_yaml`) declares the supervisors to run — a list of entries, each
  `type: collector` or `type: custom`, with the per-agent fields. Adding a kind of agent is a new
  adapter + a new entry `type`, not a change to the domain (Goal 8).

## Alternatives considered

- **Keep one supervisor per process, run several processes.** Loses "one process, many Supervisors"
  (Goal 6) and multiplies operational surface. Rejected.
- **`dyn ManagedAgent` trait objects in a `Vec`.** Async methods make the trait not object-safe without
  boxing every future; and the Host does not need heterogeneous agents in one collection — it spawns one
  task per supervisor. A **generic** `Supervisor<A>` monomorphised per adapter is simpler and avoids
  `async-trait`/boxing. Rejected in favour of generics + `impl Future + Send` return types.
- **Make the Foreign Agent speak OpAMP (run a shim that adds an `opamp` extension).** Only works for
  agents built on the Collector; a genuine Foreign Agent (nginx, a legacy daemon) cannot. The Custom
  Supervisor translating lifecycle to OpAMP is the whole point (specification). Rejected.
- **Report a Foreign Agent's health via a health-check command inside `status()`.** `status()` is
  synchronous and called on the hot path; running a command there is wrong. Liveness-as-health ships
  now; an async health-check hook is a later refinement.
- **One shared OpAMP connection multiplexing all agents.** The specification models each Supervisor as
  its own Agent with its own Instance UID; multiplexing would invent a private sub-protocol. Rejected.

## Sources / Prior art

- The requirement: [`SPECIFICATION.md`](../SPECIFICATION.md) (Supervisor Host, Collector/Custom
  Supervisor, Foreign Agent, Plugin, Port; Goals 6–8). The single-supervisor base:
  [ADR-0008](0008-collector-supervisor-go-reference-compat.md).
- Hexagonal (ports-and-adapters) architecture — Alistair Cockburn:
  <https://alistair.cockburn.us/hexagonal-architecture/>.
- Async fn / `impl Trait` in trait methods (RPITIT), stable since Rust 1.75:
  <https://blog.rust-lang.org/2023/12/21/async-fn-rpit-in-traits.html>.

## Consequences

- Positive: the OpAMP control loop is written **once** and reused for every agent type; a new kind of
  managed agent is a new adapter behind the same port; the Host manages a heterogeneous fleet from one
  process, and non-OpAMP agents join the same control loop. The Collector Supervisor's oracle-checked
  behaviour is preserved.
- Negative / trade-offs: a real refactor of the single-supervisor code into domain + port + adapters,
  and a new host configuration contract to keep stable. A Foreign Agent's health is only as good as
  process liveness until an async health-check hook lands. Each supervisor holds its own connection, so
  a large Host opens many connections (fine at dev scale; a concern only at very large fan-out).
- Follow-ups, each its own ADR when needed: an async health-check hook for Foreign Agents; hot-reload of
  the host configuration (add/remove supervisors without a restart); authenticated transport and package
  updates (still deferred from [ADR-0006](0006-rust-opamp-server-from-spec.md)/[ADR-0008](0008-collector-supervisor-go-reference-compat.md)).
