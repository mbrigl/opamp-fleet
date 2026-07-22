# OpAMP Fleet

[![Docs & ADR checks](https://github.com/mbrigl/opamp-fleet/actions/workflows/docs-check.yml/badge.svg)](https://github.com/mbrigl/opamp-fleet/actions/workflows/docs-check.yml)

**OpAMP Fleet** is a Rust implementation of OpenTelemetry [OpAMP](https://opentelemetry.io/docs/specs/opamp/)-based
fleet management: an API-first **Server** that manages a fleet over the protocol and exposes an
OpenAPI-described REST API for any UI or portal, and a **Client** that supervises many managed
processes at once — OpenTelemetry Collectors and, through plugins, foreign agents that do not speak
OpAMP — and that can equally run as a **gateway** multiplexing other clients upstream. The work is driven by a written
**specification** ([`docs/SPECIFICATION.md`](docs/SPECIFICATION.md)) and **Architecture Decision
Records** ([`docs/adr/`](docs/adr/)), so intent and the reasoning behind every structural choice stay
explicit and reviewable. How much of the protocol each end implements is tracked in
[`docs/CONFORMANCE.md`](docs/CONFORMANCE.md).

> For agent instructions, see [`AGENTS.md`](AGENTS.md) — the single source of truth for all coding agents.

## Overview

A telemetry fleet is a heap of agents on a heap of machines, each configured by a local file. That
works for one agent and breaks down for a fleet: changing what a hundred agents do means reaching a
hundred machines, and nobody can say with certainty what each one is *actually* running. Configuration
drifts, rollouts are ad-hoc, and a bad configuration shows up as missing telemetry rather than as a
report.

[OpAMP](https://opentelemetry.io/docs/specs/opamp/) — the Open Agent Management Protocol — closes that
loop: an agent accepts configuration over the protocol and reports back what it applied and how it is
doing. **OpAMP Fleet** is a Rust implementation of both ends, built for a *heterogeneous* fleet —
OpenTelemetry Collectors **and** agents that were never built to speak OpAMP:

- **Server** — an API-first control plane (Linux). It holds the configuration the fleet should run,
  tracks what each agent reports back, and only reconfigures an agent whose configuration actually
  differs. Its contract is an **OpenAPI-described REST API**, so any UI or portal can read the fleet's
  state and change what it runs; the Server ships only a rudimentary UI of its own and is built to be
  integrated into an existing portal.
- **Client** — one process, installed as a native operating-system service on Linux, macOS, and
  Windows and able to update its own binary in place. It has two **modes**, independent of each other
  and combinable on the same host: **Supervisor Mode** runs **many supervisors at once**, each
  managing one process, applying the configuration it is sent and reporting health and effective
  configuration back; **Gateway Mode** accepts other clients' OpAMP connections and folds them onto a
  small pool of upstream ones, so a fleet can grow past one connection per agent. Every supervisor
  also exposes a **Supervisor Endpoint** on loopback — not a mode of its own, but part of what a
  supervisor is — because the Collector's `opampextension` is a *client only* and needs something to
  connect to; a Collector carrying it reports through that endpoint instead of being watched from
  outside. A Collector supervisor manages a Collector natively; a
  **custom supervisor** manages a **foreign agent** — an agent of a kind the project does not already
  know, needing a plugin written for it — by translating its lifecycle into the protocol.
- **Plugins over a hexagonal core** — supervisors are plugins behind stable ports. Bringing a new kind
  of process under management means writing a plugin, not changing the core, so the same control loop
  reaches agents OpAMP was never designed for.
- **The protocol, in full and on the record** — both ends implement OpAMP as completely as the
  protocol allows, against a pinned upstream version, with every capability's status and maturity
  written down in [`docs/CONFORMANCE.md`](docs/CONFORMANCE.md) rather than left to be discovered.

The goal is one place — reachable by any UI — to decide what every agent in the fleet runs and to see
what each one is really running, whether or not it speaks OpAMP. The full problem statement, goals,
vocabulary, and non-goals live in the **specification** ([`docs/SPECIFICATION.md`](docs/SPECIFICATION.md));
the reasoning behind each structural choice lives in the ADRs ([`docs/adr/`](docs/adr/)).

## Architecture

The picture keeps the shape of the [OpAMP reference architecture](https://opentelemetry.io/docs/specs/opamp/)
— a supervisor owning a Collector, exchanging OpAMP with a backend — and extends it with what makes
OpAMP Fleet different: an **API-first Server** whose contract is an OpenAPI REST API, a single
**Client** whose two modes compose freely, **Supervisors as plugins** behind a hexagonal core — each
exposing a **Supervisor Endpoint** for a Collector that speaks the protocol itself — a **Custom
Supervisor** that brings a **non-OpAMP Foreign Agent** into the same control loop, and a
**Connection Pool** that carries many Agents over few connections.

```mermaid
flowchart TB
  UI("UI / Portal<br/>external · any frontend"):::ext
  TB("Telemetry Backend"):::ext

  subgraph SRV["OpAMP Fleet Server — API-first · Linux"]
    direction TB
    API("OpenAPI REST + SSE"):::server
    LOOP("Fleet control loop<br/>config-hash diff · package delivery"):::server
    ROUTE("Agent registry<br/>routed by instance_uid"):::server
    STORE[("Configuration<br/>+ Packages")]:::store
    API --> LOOP --> STORE
    LOOP --- ROUTE
  end

  UI -->|"read fleet · change config"| API

  subgraph HOST["Client — one process, two independent modes"]
    direction TB
    CORE("Supervision domain<br/>hexagonal core · ports"):::core
    POOL("Connection Pool<br/>n Agents over m connections"):::core

    subgraph SUP["Supervisor Mode"]
      direction TB
      CS("Collector Supervisor<br/>plugin"):::host
      XS("Custom Supervisor<br/>plugin"):::host
      LS(["Supervisor Endpoint<br/>loopback · always present"]):::local
      CS --- LS
    end

    GW("Gateway Mode<br/>multiplexes other Clients"):::host

    CORE --- CS
    CORE --- XS
    CORE --- POOL
    GW --- POOL
  end

  ROUTE <==>|"OpAMP · each Agent = one instance_uid"| POOL

  COL("Collector<br/>without opampextension"):::agent
  COLX("Collector<br/>with opampextension"):::agent
  FA("Foreign Agent<br/>needs a plugin of its own"):::agent
  RC("Other Clients<br/>downstream"):::ext

  CS -->|"config · restart · binary update"| COL
  XS -->|"translate lifecycle to OpAMP"| FA
  COLX -->|"OpAMP · loopback"| LS
  RC -->|"OpAMP"| GW

  COL -->|OTLP| TB
  COLX -->|OTLP| TB
  FA -.->|telemetry| TB

  classDef server fill:#eef2ff,stroke:#6366f1,stroke-width:1px,color:#1e1b4b;
  classDef core fill:#e0e7ff,stroke:#4f46e5,stroke-width:1px,color:#1e1b4b;
  classDef host fill:#ecfdf5,stroke:#10b981,stroke-width:1px,color:#064e3b;
  classDef agent fill:#f0fdfa,stroke:#14b8a6,stroke-width:1px,color:#134e4a;
  classDef ext fill:#f8fafc,stroke:#94a3b8,stroke-width:1px,color:#0f172a;
  classDef store fill:#fffbeb,stroke:#f59e0b,stroke-width:1px,color:#78350f;
  classDef local fill:#d1fae5,stroke:#059669,stroke-width:1px,color:#064e3b;

  style SRV fill:transparent,stroke:#6366f1,stroke-width:2px;
  style HOST fill:transparent,stroke:#10b981,stroke-width:2px,stroke-dasharray:6 4;
  style SUP fill:transparent,stroke:#34d399,stroke-width:1px,stroke-dasharray:3 3;
```

On the wire the Server sees only **Agents**, told apart by `instance_uid` and never by the connection
that carried them — so whether an Agent is a Collector Supervisor, a Custom Supervisor fronting a
Foreign Agent, a Collector reporting through its own `opampextension`, or a Client several hops away
behind a Gateway is invisible to it. The Supervisor Endpoint is bound to loopback and comes up with
every supervisor; a Foreign Agent speaks no OpAMP, so nothing connects to it there and that is the
whole of the handling. What separates a Collector from a Foreign Agent is which plugin has to exist
for it, not whether it speaks OpAMP: one Collector supervisor serves every Collector, with or without
the extension, while each kind of foreign agent needs a custom supervisor written for it. Adding a
new kind of managed process means writing another plugin against the
same ports — the core does not change. The terms used here (Server, Client, Agent, Client Modes,
Supervisor Endpoint, Connection Pool, Collector/Custom Supervisor, Foreign Agent, Plugin, Port,
Selector, Package, …) are defined in [`docs/SPECIFICATION.md`](docs/SPECIFICATION.md).

## Prerequisites

- [VS Code](https://code.visualstudio.com/) with the
  [Dev Containers](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers)
  extension — or any DevContainer-compatible IDE
- Docker / Podman (rootless) available on the host

## Getting Started

1. Open the repository in VS Code and choose **Reopen in Container** — the Dev Container and
   preconfigured agent extensions build automatically.
2. Authenticate your coding agent inside the container (for Claude Code: `claude login`).
3. Start working with the agent — drive the work from the specification and the ADRs.

## Build, Test & Run

The toolchain is **Rust stable**, provided by the Dev Container; the code is one Cargo workspace 
with three crates — `opamp` (shared library), `server`, and `client` (the Client, in all its modes). 
This section is the single source for build/test/run commands — both humans and agents rely on 
it (AGENTS.md links here).

- **Build:** TODO <!-- e.g. `make build` -->
- **Test:** TODO <!-- e.g. `make test` -->
- **Run:** TODO <!-- e.g. `make run` -->

## Usage

<!-- Once there is something to use, show how to use the built software: the primary commands or
     API, a minimal example, and the expected output. Keep build/test/run mechanics in the section
     above — this section is about using the result, not producing it. -->

TODO — show a minimal example of using the project.

## Project Layout

```
README.md             # overview & setup for humans
AGENTS.md             # single source of truth for coding agents
docs/SPECIFICATION.md # the specification: problem, goals, vocabulary
docs/CONFORMANCE.md   # OpAMP Protocol Baseline + capability conformance matrix
docs/adr/             # Architecture Decision Records (+ template)
crates/               # Cargo workspace: opamp (shared) · server · client
scripts/check-docs.sh # documentation & protocol-baseline consistency checks
rust-toolchain.toml   # pinned Rust toolchain (stable + rustfmt + clippy)
.devcontainer/        # Dev Container definition (base image + Features)
.vscode/              # shared editor settings
.claude/CLAUDE.md     # pointer for Claude Code to read AGENTS.md
.claude/settings.json # Claude Code permissions: prompt before git/gh writes
```

## Dev Container

The environment is defined entirely in [`.devcontainer/devcontainer.json`](.devcontainer/devcontainer.json):
it starts from a prebuilt base image and layers Dev Container Features and VS Code extensions on top —
no Dockerfile or Compose file required. Customise the environment by adding Features, switching the
base image, or adding extensions.

### Host container management

The Dev Container deliberately has **no access to the host Docker daemon** — the socket is not mounted
([ADR-0002](docs/adr/0002-dev-container-runtime.md)). To manage the host's containers from VS
Code, run the **Container Tools** extension (`ms-azuretools.vscode-containers`) on the **host** side:
install it in your host VS Code. [`.vscode/settings.json`](.vscode/settings.json) already pins it to
run locally via `remote.extensionKind`, so it keeps talking to the host engine even when this folder
is reopened in the container.

## Coding Agents

This Dev Container preinstalls the **Claude Code** and **Mistral Vibe** VS Code extensions (see
[`.devcontainer/devcontainer.json`](.devcontainer/devcontainer.json)); other agents (OpenAI Codex,
Cursor, OpenCode, GitHub Copilot) work too once you add them. Authenticate your agent inside the
container (for Claude Code: `claude login`).

The rules every agent follows live in [`AGENTS.md`](AGENTS.md); how each agent is wired to read them
is recorded in [ADR-0001](docs/adr/0001-agent-governance-model.md).

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md) for the workflow (specification- and ADR-driven, small
reviewable changes) and [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) for the community standards we
expect of everyone taking part. Security issues: please follow [`SECURITY.md`](SECURITY.md) instead
of opening a public issue.

## License

Released under the Apache License 2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
