# OpAMP Fleet

[![Docs & ADR checks](https://github.com/mbrigl/opamp-fleet/actions/workflows/docs-check.yml/badge.svg)](https://github.com/mbrigl/opamp-fleet/actions/workflows/docs-check.yml)

**OpAMP Fleet** is a Rust implementation of OpenTelemetry [OpAMP](https://opentelemetry.io/docs/specs/opamp/)-based
fleet management: a headless **Server** that manages a fleet over the protocol and exposes a REST API
for any UI, and a **Supervisor Host** that runs many supervisors at once — for OpenTelemetry Collectors
and, through plugins, for foreign agents that do not speak OpAMP. The work is driven by a written
**specification** ([`docs/SPECIFICATION.md`](docs/SPECIFICATION.md)) and **Architecture Decision
Records** ([`docs/adr/`](docs/adr/)), so intent and the reasoning behind every structural choice stay
explicit and reviewable.

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

- **Server** — a headless control plane. It holds the configuration the fleet should run, tracks what
  each agent reports back, and only reconfigures an agent whose configuration actually differs. Its
  sole interface is a **REST API**, so any UI or portal — not one shipped with the project — can read
  the fleet's state and change what it runs.
- **Supervisor Host** — one client process that runs **many supervisors at once**. Each supervisor
  manages one agent, applies the configuration it is sent, and reports its health and effective
  configuration back. A Collector supervisor manages an OpenTelemetry Collector natively; a **custom
  supervisor** manages a **foreign agent** that does not speak OpAMP by translating its lifecycle into
  the protocol.
- **Plugins over a hexagonal core** — supervisors are plugins behind stable ports. Bringing a new kind
  of agent under management means writing a plugin, not changing the core, so the same control loop
  reaches agents OpAMP was never designed for.

The goal is one place — reachable by any UI — to decide what every agent in the fleet runs and to see
what each one is really running, whether or not it speaks OpAMP. The full problem statement, goals,
vocabulary, and non-goals live in the **specification** ([`docs/SPECIFICATION.md`](docs/SPECIFICATION.md));
the reasoning behind each structural choice lives in the ADRs ([`docs/adr/`](docs/adr/)).

## Architecture

The picture keeps the shape of the [OpAMP reference architecture](https://opentelemetry.io/docs/specs/opamp/)
— a supervisor owning a Collector, exchanging OpAMP with a backend — and extends it with what makes
OpAMP Fleet different: a **headless Server** whose only interface is a REST API, a single
**Supervisor Host** that runs **many** Supervisors as plugins behind a hexagonal core, a **Custom
Supervisor** that brings a **non-OpAMP Foreign Agent** into the same control loop, and an
authenticated, encrypted transport that both configuration and software updates ride on.

```mermaid
flowchart TB
  UI("UI / Portal<br/>external · any frontend"):::ext
  TB("Telemetry Backend"):::ext

  subgraph SRV["OpAMP Fleet Server — headless"]
    direction TB
    API("REST API + SSE"):::server
    LOOP("Fleet control loop<br/>config-hash diff · package delivery"):::server
    STORE[("Configuration<br/>+ Packages")]:::store
    API --> LOOP --> STORE
  end

  UI -->|"read fleet · change config"| API

  subgraph HOST["Supervisor Host — one process, many Supervisors"]
    direction TB
    CORE("Supervision domain<br/>hexagonal core · ports"):::core
    CS("Collector Supervisor<br/>plugin · OpAMP-native"):::host
    XS("Custom Supervisor<br/>plugin · adapter"):::host
    CORE --- CS
    CORE --- XS
  end

  LOOP <==>|"OpAMP over TLS + shared token<br/>each Supervisor = one Agent"| CORE

  COL("OpenTelemetry<br/>Collector"):::agent
  FA("Foreign Agent<br/>does not speak OpAMP"):::agent

  CS -->|"config · restart · binary update"| COL
  XS -->|"translate lifecycle to OpAMP"| FA

  COL -->|OTLP| TB
  FA -.->|telemetry| TB

  classDef server fill:#eef2ff,stroke:#6366f1,stroke-width:1px,color:#1e1b4b;
  classDef core fill:#e0e7ff,stroke:#4f46e5,stroke-width:1px,color:#1e1b4b;
  classDef host fill:#ecfdf5,stroke:#10b981,stroke-width:1px,color:#064e3b;
  classDef agent fill:#f0fdfa,stroke:#14b8a6,stroke-width:1px,color:#134e4a;
  classDef ext fill:#f8fafc,stroke:#94a3b8,stroke-width:1px,color:#0f172a;
  classDef store fill:#fffbeb,stroke:#f59e0b,stroke-width:1px,color:#78350f;

  style SRV fill:transparent,stroke:#6366f1,stroke-width:2px;
  style HOST fill:transparent,stroke:#10b981,stroke-width:2px,stroke-dasharray:6 4;
```

On the wire the Server sees only **Agents**; whether an Agent is an OpAMP-native Collector Supervisor
or a Custom Supervisor fronting a Foreign Agent is invisible to it. Adding a new kind of managed agent
means writing another plugin against the same ports — the core does not change. The terms used here
(Server, Supervisor Host, Collector/Custom Supervisor, Foreign Agent, Plugin, Port, Selector, Package,
Shared token, …) are defined in [`docs/SPECIFICATION.md`](docs/SPECIFICATION.md).

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

<!-- Fill in once the toolchain is chosen. This section is the single source for build/test/run
     commands — both humans and agents rely on it (AGENTS.md links here). -->

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
docs/adr/             # Architecture Decision Records (+ template)
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
