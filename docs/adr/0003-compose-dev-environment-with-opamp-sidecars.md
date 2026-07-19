# ADR-0003: Compose-based Dev Container with OpAMP sidecars

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer
- **Supersedes:** [ADR-0002](0002-dev-container-runtime.md) (its *runtime model* — the base-image Dev
  Container), which is therefore marked `⚪ superseded by ADR-0003`.

## Context

The [specification](../SPECIFICATION.md) requires the environment to *prove* the control loop against
real agents: Goal 12 ("The environment proves it") calls for bringing up the Server together with the
upstream OpenTelemetry Supervisor and Collector **and at least one third-party collector**, and the
strategy bullet "Develop against real agents" makes this a first-class property, not a test detail.

[ADR-0002](0002-dev-container-runtime.md) chose a bare **`mcr.microsoft.com/devcontainers/base:debian`
image** with **no Docker Feature and no host Docker socket**, and explicitly deferred sidecars and the
toolchain to per-project ADRs ("If in-container container builds become genuinely necessary, add an
**isolated** engine through a new ADR rather than mounting the host socket"). Two forces now collide:

1. The upstream Supervisor and its Collector, and the third-party collectors, are **container
   workloads**; the natural way to run them beside the code is Docker Compose.
2. The **no-host-daemon rule is binding**: no `docker-outside-of-docker` (host socket) and no
   docker-in-docker (privileged nested daemon). A coding agent with the host socket has host-level
   blast radius ([`SECURITY.md`](../../SECURITY.md)).

The resolution is that the Dev Container does not *need* Docker to have the stack running beside it. A
Dev Container may itself **be a service of a Compose project** (`dockerComposeFile` + `service` in
`devcontainer.json`). The IDE launches that project on the **host** engine; the workspace container and
the sidecars land on one Compose network and reach each other by service name. Nothing inside the
container touches a Docker daemon. This changes ADR-0002's *runtime model* (a bare `image` becomes a
Compose `service`) while preserving its **core security property verbatim**, so it supersedes ADR-0002
rather than contradicting it silently (`AGENTS.md §3.2`).

Verified constraints that shape the design (July 2026):

- The supervisor image `otel/opentelemetry-collector-opampsupervisor` (~15 MB) ships **only** the
  supervisor binary; `agent.executable` must point at a collector binary on the same filesystem. The
  sidecar image must therefore combine the supervisor with `otel/opentelemetry-collector-contrib`.
- The upstream supervisor implements remote configuration but **not** package/binary updates; it is
  **alpha**, so its configuration surface may change between collector releases — images stay pinned.
- Because the Dev Container has no Docker, whether the **third-party** images actually connect to our
  WebSocket server can only be confirmed by bringing the project up on the **host**. Those sidecars are
  therefore **SPIKE-PENDING** until a host-side connection spike confirms them.

## Decision

We will define the Dev Container through **Docker Compose** (`.devcontainer/docker-compose.yml`), with
the workspace container as the `dev` service and three OpAMP agent sidecars.

- **No Docker socket, no docker-in-docker, no Docker CLI** in the container — the no-host-daemon rule of
  [ADR-0002](0002-dev-container-runtime.md) is preserved verbatim. The Compose project is started by the
  IDE on the host engine; `shutdownAction: stopCompose` tears it down with the window.
- **The Server under development runs inside `dev`**, listening on `:4320` (OpAMP) and `:4321` (REST
  API). Sidecars reach it over the Compose network at `ws://dev:4320/v1/opamp`.
- **`opamp-agent`** — the upstream OpenTelemetry OpAMP Supervisor plus the Collector it owns, built from
  a small multi-stage Dockerfile that copies both binaries into one debuggable image, **pinned to one
  release** (`0.156.0`), never `latest`/`nightly`. It is the **behavioural oracle** the project's own
  agents are checked against, and is configured with `startup_fallback_configs` so its Collector still
  starts when the Server is unreachable (e.g. while it is being edited).
- **`bindplane-agent`** (`ghcr.io/observiq/bindplane-agent`) and **`splunk-collector`**
  (`quay.io/signalfx/splunk-otel-collector`, running the bundled `opampextension` as a reporting
  client) — two independent third-party OpAMP clients, for conformance breadth (Goal 12's "at least one
  third-party collector"). Both are **SPIKE-PENDING**: connect-first / observation-only, their exact
  images and config best-effort until the host-side spike confirms they speak our WebSocket transport.
  If an image is HTTP-only, stop and raise an OpAMP-HTTP-transport ADR rather than working around it.
- **Sidecar lifecycle (logs, restart, rebuild) is a host-side concern** — managed from a host-pinned VS
  Code extension, never from inside the container (the price of keeping the socket out).

## Alternatives considered

- **`docker-outside-of-docker` (mount the host socket)** — lets the agent run `docker compose` itself,
  at the host-level blast radius this project rejects. Excluded by the binding no-host-daemon rule.
- **docker-in-docker** — a privileged nested daemon in the workspace container. Rejected for the same
  privilege reason.
- **Run the supervisor and collector as plain processes inside `dev`** — no Compose, but it mixes the
  managed fleet into the workspace container and makes the Collector's environment indistinguishable
  from the developer's, the exact separation an OpAMP server exists to manage. Rejected for the oracle;
  the project's *own* agents under development may still run in `dev` (a later ADR, once the workspace
  exists).
- **Point the sidecar at a published OpAMP server in a third container** — then the code under
  development is not in the loop, defeating the environment's purpose.
- **Use the `opampsupervisor` image as-is** — impossible: it ships no collector binary for
  `agent.executable` to launch.

## Sources / Prior art

- The requirement this serves: [`SPECIFICATION.md`](../SPECIFICATION.md) (Goal 12, "Develop against real
  agents"). The decision it supersedes: [ADR-0002](0002-dev-container-runtime.md).
- Dev Containers with Docker Compose (`dockerComposeFile`, `service`, `workspaceFolder`,
  `shutdownAction: stopCompose`) — <https://containers.dev/implementors/json_reference/> and
  <https://code.visualstudio.com/docs/devcontainers/create-dev-container#_use-docker-compose>.
- OpAMP Supervisor README (config, capabilities, unimplemented package updates, alpha) —
  <https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/README.md>.
- Images: supervisor (binary only) — <https://hub.docker.com/r/otel/opentelemetry-collector-opampsupervisor>;
  contrib collector (binary at `/otelcol-contrib`) — <https://hub.docker.com/r/otel/opentelemetry-collector-contrib>;
  Bindplane agent — <https://github.com/observIQ/bindplane-agent>; Splunk OTel Collector —
  <https://github.com/signalfx/splunk-otel-collector>.
- Docker daemon attack surface (why the host socket stays out) —
  <https://docs.docker.com/engine/security/#docker-daemon-attack-surface>.

## Consequences

- Positive: the full control loop (Server → agents → report back) is exercisable end-to-end from the
  first commit, on every developer machine, with one "Reopen in Container", against a real oracle and
  two real third-party clients. The container still cannot touch the host daemon.
- Negative / trade-offs: the environment is heavier than one image (a Compose project, an image build,
  ~4 containers). Nobody inside the container can `docker compose logs`/restart a sidecar — that is
  host-side friction, the price of keeping the socket out. The upstream supervisor is alpha, so images
  stay pinned and are upgraded deliberately. BindPlane and Splunk are unverified until the host-side
  spike; the environment must not be presented as conformance-proven until then.
- Follow-ups: the host-side connection spike for the two third-party sidecars; an ADR for the project's
  own Rust agents running inside `dev` once the Cargo workspace exists; the Rust toolchain and build
  tooling are decided separately in [ADR-0004](0004-rust-toolchain-dev-container.md).
