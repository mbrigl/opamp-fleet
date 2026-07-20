# Specification — OpAMP Fleet

> The **specification** of this project: the problem it solves, where it is going, and the
> vocabulary everyone (humans and agents) must use. This document is the **constitution** —
> every Architecture Decision Record in [`adr/`](adr/) derives from it and must not contradict it.

## Problem

A telemetry fleet is a heap of agents on a heap of machines, each configured by a file next to it.
That is fine for one agent and untenable for a fleet: changing what a hundred agents do means reaching
a hundred machines, and nobody can answer, with certainty, what each one is *actually* running right
now. Configuration drifts, rollouts are ad-hoc, and a bad configuration is discovered by the absence
of telemetry rather than by a report.

[OpAMP](https://opentelemetry.io/docs/specs/opamp/) — the Open Agent Management Protocol — exists to
close exactly this loop: an agent accepts configuration over the protocol and reports back what it
applied and how it is doing. But two gaps keep it from managing a real fleet. First, the protocol
needs a **Server** on the other end, and there is no such server in Rust. Second, a real fleet is
**heterogeneous**: alongside OpenTelemetry Collectors it runs agents that do **not** speak OpAMP at
all, and a purely OpAMP-native tool simply cannot see them — so the fleet is never managed from one
place, only the OpAMP-native slice of it is.

**OpAMP Fleet** is a Rust implementation that closes both gaps: the Server, and a client that manages
OpAMP-native and non-OpAMP agents alike behind the same protocol.

## Mission

Give operators one place — reachable by any UI through a REST API — to decide what every agent in a
heterogeneous fleet is running, and to see what each one is actually running, whether or not the agent
itself speaks OpAMP.

## Vision

An operator changes a configuration in one place and knows, within seconds, which agents took it and
which did not, and why — across OpAMP-native Collectors and foreign, non-OpAMP agents alike. A change
can address the whole fleet or a chosen subset of it, so a configuration can be rolled out to part of
the fleet before all of it. And what the Server distributes is not only configuration but the agents'
software itself: it can update an Agent's binary in place, verified before it is applied and rolled
back on failure. Agents that were never built for OpAMP are brought into the same control loop by
purpose-built supervisor plugins, so the whole fleet's real state is observable rather than assumed,
and a rejected configuration or a failed update is a reported event, not a silent outage.

The Server exposes a stable, OpenAPI-described REST API and carries only a rudimentary user interface of
its own,
so any external portal can integrate it easily and render the fleet however it likes. The client is one
process that can supervise many agents at once, installs as a native operating-system service that
updates itself in place, and runs on Linux, macOS, and Windows; growing to a new kind of agent means
writing a new plugin, not changing the core. The project grows
from the smallest thing that closes the loop and widens only as the protocol and its agents actually
allow.

## Strategy

- **Speak the protocol, do not reinvent it.** The wire contract is the OpAMP specification, implemented
  faithfully on both ends — the Server and the OpAMP-native supervisor. Any conforming agent, not only
  this project's, can be managed; a foreign agent is brought *up to* OpAMP by a plugin, never managed
  by a private side-channel.
- **Own both ends in Rust.** The Server and the client are one Rust stack, so the whole control loop is
  code this project controls. Where a reference implementation exists it stays the behavioural oracle
  the Rust code is checked against, rather than being replaced by it.
- **API-first Server, portal-friendly.** The Server's public contract is an **OpenAPI-described REST
  API**, so any portal can generate a client and drive the fleet through it. The Server bundles only a
  rudimentary UI for basic operation; a richer UI lives wherever the operator wants it — a standalone
  app or an existing portal — and is expected to be built outside this project.
- **One host, many supervisors, behind a hexagonal core.** The client is a single **Supervisor Host**
  process that runs many **Supervisor** instances. The supervision domain sits at the centre; **ports**
  abstract the Server-facing side and the managed-agent side; **plugins** are the adapters that
  implement concrete agent types. An OpenTelemetry Collector supervisor and a foreign-agent supervisor
  are two plugins behind the same ports.
- **Ship the Agent as a self-updating OS service across all platforms; the Server on Linux.** The
  Supervisor Host installs as a native operating-system service and can replace its own binary in place,
  and it is built for Linux, macOS, and Windows so one client shape manages a heterogeneous fleet. The
  Server targets Linux only.
- **Bring non-OpAMP agents into OpAMP.** A Custom Supervisor plugin owns a foreign agent and translates
  its lifecycle, configuration, and health into OpAMP toward the Server, so heterogeneous agents share
  one control loop and appear in the fleet like any other Agent.
- **Close the loop before widening it.** A working control loop — configure, apply, report back — for
  one agent comes first. Targeting a subset of the fleet and updating an Agent's software are core
  goals, built on top of that loop once it holds, not before it.
- **Distribute software, not only configuration.** The Server can deliver and apply agent binaries, not
  just their configuration — verified before applying and rolled back on failure — but only as far as
  the managed agents actually support it, never pretending to a capability an agent lacks.
- **Configure the whole fleet or a part of it.** A configuration can be directed at the whole fleet or
  at a selected subset, so a change can be rolled out gradually; an Agent outside a target is left
  running what it already runs.

## Core Concepts & Vocabulary

Use these exact words in code, comments, documentation, and ADRs.

- **OpAMP** — the Open Agent Management Protocol: the wire protocol between a Server and its Agents.
- **Server** — this project's control plane. It tells Agents which configuration they should be running
  and records what they report back. There is exactly one Server per fleet. It is **API-first**: its
  primary interface is the OpenAPI REST API, and it ships only a rudimentary UI, so any external portal
  can integrate it. The Server runs on Linux only.
- **REST API** — the Server's stable HTTP interface for reading fleet state and reading and changing
  configuration, described by an **OpenAPI** specification so clients and portals can be generated from
  it rather than hand-written. It is the integration contract any UI or portal builds on.
- **Fleet** — all Agents managed by the Server.
- **Agent** — a process that connects to the Server over OpAMP and is managed by it. On the wire the
  Server sees only an Agent; it does not distinguish an OpAMP-native supervisor from a Custom
  Supervisor fronting a foreign agent.
- **Supervisor Host** — the client process. A single Supervisor Host runs **multiple Supervisor
  instances** at once, loading each as a plugin. It installs as a native operating-system service, can
  update itself in place, and runs on Linux, macOS, and Windows. It is the one deployable that a machine
  runs to place its agents under management.
- **Supervisor** — a unit inside the Supervisor Host that manages exactly one **Managed Agent**:
  it applies the configuration it receives, and reports health and effective configuration. Each
  Supervisor is one **Agent** as the Server sees it.
- **Collector Supervisor** — the OpAMP-native Supervisor plugin: it owns an OpenTelemetry **Collector**,
  writes the configuration it receives to disk, and restarts the Collector to apply it.
- **Custom Supervisor** — a Supervisor plugin that manages a **Foreign Agent** and translates that
  agent's lifecycle, configuration, and health into OpAMP toward the Server. It is how a non-OpAMP
  agent is brought into the fleet.
- **Managed Agent** — the actual process a Supervisor manages: a Collector, or a Foreign Agent.
- **Foreign Agent** — a Managed Agent that does **not** implement OpAMP. It is reachable only through a
  Custom Supervisor, never directly by the Server.
- **Collector** — an OpenTelemetry Collector process. It does not speak OpAMP itself; it is managed
  *through* a Collector Supervisor.
- **Plugin** — a Supervisor implementation loaded by the Supervisor Host and plugged in behind the
  hexagonal ports. Supporting a new kind of agent means adding a plugin, not changing the core.
- **Port** — a boundary the supervision domain defines and depends on: the Server-facing side (speaking
  OpAMP) and the Managed-Agent side (a Managed Agent's lifecycle, configuration, and health). The
  domain is written against ports, never against a concrete agent.
- **Adapter** — a concrete implementation of a Port: the OpAMP client is an adapter on the
  Server-facing side; each Plugin is an adapter on the Managed-Agent side.
- **Remote configuration** — the configuration the Server distributes to an Agent. It is what the
  Server *wants* the Agent to run.
- **Effective configuration** — the configuration an Agent reports it is *actually* running. It may
  differ from the remote configuration (an Agent may merge in local configuration, or have rejected the
  remote one).
- **Config hash** — the identity of a configuration. An Agent reports the hash it last received; the
  Server compares it against the hash of the configuration the Agent should have, and sends a new
  configuration only when they differ. This comparison is the control loop.
- **Instance UID** — the identity of an Agent instance, generated as a **UUID v7** and stable across its
  restarts by default. The Server may reassign it (via OpAMP's AgentIdentification), and the Agent then
  adopts the new value for all further communication.
- **Capability** — a feature an Agent or the Server declares it supports (accepting remote
  configuration, reporting health, reporting effective configuration, …). Neither side may assume an
  undeclared capability.
- **Health** — an Agent's self-reported liveness and status.
- **Selector** — the rule by which the Server addresses a **subset** of the fleet for a configuration,
  so a change reaches the matching Agents and leaves the rest running what they already run. It is how
  a configuration is rolled out to part of the fleet rather than all of it.
- **Package** — a versioned, downloadable software artifact an Agent installs, verified against a
  content hash (and optionally a signature). The Server offers Packages; an Agent reports the status of
  each. This is how the Server updates an Agent's software, not only its configuration.
- **Updater** — the separate process that applies a Package: it stops the target (the Managed Agent, or
  the Supervisor itself), replaces its binary, restarts it, and rolls back on failure. A running
  process cannot reliably replace its own binary, so this work is handed off across a process boundary.

## Goals / Success Criteria

1. **The loop closes.** A configuration change made on the Server reaches a connected OpAMP-native
   Agent without the Agent asking for it, and the Agent reports it as applied.
2. **The Server knows the fleet's state.** For every connected Agent it can report: its identity, its
   health, the configuration it holds, and whether it accepted or rejected it — including the error
   when it rejected it.
3. **No redundant reconfiguration.** An Agent that already runs the intended configuration is not sent
   it again; the config-hash comparison gates every push.
4. **A rejected configuration is visible.** An Agent that refuses a configuration surfaces the reason
   rather than failing silently.
5. **Any UI can drive the fleet.** The OpenAPI-described REST API exposes fleet state and configuration
   as a stable contract; an external UI or portal reads the fleet and changes what it runs entirely
   through that API. The Server bundles only a rudimentary UI of its own.
6. **One host runs many supervisors.** A single Supervisor Host process runs multiple Supervisor
   instances concurrently, each appearing to the Server as its own independent Agent.
7. **A non-OpAMP agent is managed like any other.** A Foreign Agent placed under a Custom Supervisor is
   configured, reports health, and reports back through the same control loop, and appears in the fleet
   indistinguishably from an OpAMP-native Agent.
8. **A new agent type is a new plugin.** Adding support for another kind of Managed Agent is done by
   writing a Plugin against the existing ports, without changing the supervision domain core.
9. **A configuration can target a subset of the fleet.** The Server can direct a configuration at a
   selected subset of Agents via a Selector; the matching Agents apply it and every Agent outside the
   target keeps running what it already runs, so a change can be rolled out gradually rather than all at
   once.
10. **The Server updates an Agent's software, not only its configuration.** Via OpAMP package delivery
    the Server can update an Agent's binary — the Collector's, and the Supervisor's own — verifying each
    Package before it is applied, reporting the outcome, and rolling back on failure. A failed update is
    reported, not silent.
11. **The Agent runs and updates itself as an OS service, on every platform.** The Supervisor Host
    installs and runs as a native operating-system service on Linux, macOS, and Windows, and can replace
    its own binary in place — a self-update that survives the service restart and is rolled back on
    failure. The Server runs on Linux.

## Non-Goals

- **Being a telemetry backend.** OpAMP Fleet manages agents; it does not receive, store, or query the
  telemetry they produce.
- **Replacing an agent's configuration language.** The Server distributes each Managed Agent's own
  configuration; it does not invent an abstraction over the Collector's or a Foreign Agent's format.
- **Shipping a production UI.** The Server provides the OpenAPI REST API and only a rudimentary UI for
  basic operation; a production-grade user interface is external and out of scope for this project to
  build.
- **Authentication, authorization, and multi-tenancy.** The Server does not authenticate that a peer
  belongs to the fleet, distinguish *which* Agent may do *what*, nor separate one operator's fleet from
  another's. Transport security, per-Agent identity, roles, and tenancy are real needs deferred rather
  than half-built.
