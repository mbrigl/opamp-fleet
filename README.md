# OpAMP Fleet

[![CI](https://github.com/mbrigl/opamp-fleet/actions/workflows/ci.yml/badge.svg)](https://github.com/mbrigl/opamp-fleet/actions/workflows/ci.yml)
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

A Cargo workspace ([ADR-0005](docs/adr/0005-cargo-workspace-layout.md)); the toolchain is pinned in
`rust-toolchain.toml` and `protoc` is provided by the Dev Container ([ADR-0004](docs/adr/0004-rust-toolchain-dev-container.md)).

- **Build:** `cargo build --workspace --all-targets`
- **Test:** `cargo test --workspace`
- **Lint / format:** `cargo clippy --workspace --all-targets -- -D warnings` and `cargo fmt --all --check`
- **Run the server:** `cargo run -p opamp-server` — serves the OpAMP endpoint on `:4320` and the fleet
  REST API + UI on `:4321`, distributing [`config/collector.yaml`](config/collector.yaml).
- **Run the Supervisor Host** (skeleton): `cargo run -p opamp-supervisor`.

## Usage

Bring the environment up (**Reopen in Container**): the server runs in the `dev` container and the three
OpAMP sidecars connect to it ([ADR-0003](docs/adr/0003-compose-dev-environment-with-opamp-sidecars.md)).
Then:

```bash
cargo run -p opamp-server          # in the dev container
```

- Open the **fleet UI** at <http://localhost:4321/> — each connected agent (the upstream OTel
  Supervisor, Bindplane, Splunk) appears with its health, configuration status, and effective config.
- Or read the fleet over the **REST API** ([ADR-0007](docs/adr/0007-rest-api-and-fleet-ui.md)):

```bash
curl -s localhost:4321/api/fleet | jq        # every connected agent and its status, as JSON
curl -s localhost:4321/api/config            # the collector configuration being distributed
curl -T new.yaml localhost:4321/api/config   # change it — the server pushes it to the fleet
```

> The `:4321` surface is **unauthenticated** — anyone who can reach it can reconfigure the whole fleet.
> It is a development server; do not expose it beyond a trusted network
> ([ADR-0006](docs/adr/0006-rust-opamp-server-from-spec.md), ADR-0007).

## Project Layout

```
README.md                     # overview & setup for humans
AGENTS.md                     # single source of truth for coding agents
docs/SPECIFICATION.md         # the specification: problem, goals, vocabulary
docs/adr/                     # Architecture Decision Records (+ template)
Cargo.toml                    # Cargo workspace (crates/*)
rust-toolchain.toml           # pinned Rust channel (container + CI agree)
config/collector.yaml         # the collector configuration the server distributes
crates/
  opamp-proto/                # shared OpAMP wire layer (vendored .proto + WS framing)
  opamp-server/               # the OpAMP Fleet Server (OpAMP endpoint, REST API, UI)
  opamp-supervisor/           # the Supervisor Host (skeleton)
.devcontainer/
  devcontainer.json           # Compose-based Dev Container (dev service)
  docker-compose.yml          # dev + OpAMP agent sidecars
  install-tools.sh            # protoc + pinned otelcol-contrib (onCreate)
  opamp-agent/                # upstream OTel Supervisor + Collector sidecar (oracle)
  splunk-collector/           # Splunk OTel Collector sidecar config
.vscode/                      # shared editor settings
.claude/CLAUDE.md             # pointer for Claude Code to read AGENTS.md
.claude/settings.json         # Claude Code permissions: prompt before git/gh writes
```

## Dev Container

The environment is a **Docker Compose** project defined in
[`.devcontainer/docker-compose.yml`](.devcontainer/docker-compose.yml) and attached to by
[`.devcontainer/devcontainer.json`](.devcontainer/devcontainer.json)
([ADR-0003](docs/adr/0003-compose-dev-environment-with-opamp-sidecars.md)). The IDE starts the project
on the **host** engine and attaches to the `dev` service, where the Rust toolchain
([ADR-0004](docs/adr/0004-rust-toolchain-dev-container.md)) is installed and the OpAMP Fleet Server
(`:4320` OpAMP, `:4321` REST API) runs. Three OpAMP agent sidecars run beside it on one Compose network
and connect back to `ws://dev:4320/v1/opamp`:

- **`opamp-agent`** — the upstream OpenTelemetry OpAMP Supervisor and the Collector it owns, the
  behavioural oracle the project's own agents are checked against.
- **`bindplane-agent`** and **`splunk-collector`** — two independent third-party OpAMP clients, for
  protocol-conformance breadth. Both are **spike-pending**: whether each image actually connects to the
  server can only be confirmed by bringing the project up on the host (see below).

The container mounts **no Docker socket and has no Docker CLI** — it cannot touch the host daemon; only
the IDE, host-side, drives Compose.

The server's listeners are **published to the host loopback** (`docker-compose.yml` → `dev.ports`,
bound to `127.0.0.1`), so a browser reaches the fleet UI + REST API at <http://localhost:4321> and the
OpAMP endpoint at `localhost:4320` directly — no IDE port-forwarding required. They are bound to
loopback only because the server is unauthenticated and must not be exposed beyond the host.

### Managing the sidecars

Because the Dev Container has no Docker access, sidecar lifecycle is a **host-side** concern. From a
terminal on the host, in the repository root, use the Compose project (named `opamp-fleet`):

```
docker compose -f .devcontainer/docker-compose.yml logs -f opamp-agent
docker compose -f .devcontainer/docker-compose.yml restart opamp-agent
```

To manage the host's containers from VS Code instead, run the **Container Tools** extension
(`ms-azuretools.vscode-containers`) on the **host** side.
[`.vscode/settings.json`](.vscode/settings.json) already pins it to run locally via
`remote.extensionKind`, so it keeps talking to the host engine even when this folder is reopened in the
container.

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
