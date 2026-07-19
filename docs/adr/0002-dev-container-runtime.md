# ADR-0002: Debian Dev Container without host Docker access

- **Status:** ⚪ superseded by [ADR-0003](0003-compose-dev-environment-with-opamp-sidecars.md)
- **Date:** 2026-06-22
- **Deciders:** Maintainer
- **Note:** Records a decision already embodied in
  [`.devcontainer/devcontainer.json`](../../.devcontainer/devcontainer.json). Documented retroactively
  so the template follows its own method; accepted by the maintainer on 2026-06-22.

## Context

The template must provide a reproducible, ready-to-use environment without committing to any language
toolchain (it has to stay language-agnostic). Coding agents frequently want Docker access, and the
obvious way to grant it — the `docker-outside-of-docker` Feature — bind-mounts the **host** Docker
socket into the container. That mount means any code or coding agent running in the container can drive
the host daemon — effectively host-level access and a large blast radius (recorded in
[`SECURITY.md`](../../SECURITY.md)). Examining the actual requirement, the only thing we need is to
**manage the host's containers from VS Code**. That is a host-side capability and does not require
exposing the socket to the container at all.

## Decision

We will base the Dev Container on `mcr.microsoft.com/devcontainers/base:debian` with **no Docker
Feature**, so no host Docker socket is mounted and the container has **no access to the host daemon**.
Host containers are managed from a VS Code extension pinned to the **host (UI) side** via
`remote.extensionKind` in [`.vscode/settings.json`](../../.vscode/settings.json), which talks to the
host engine directly even when the folder is reopened in the container. The base image ships no
language toolchain; each project adds its own and fills in the **Build, Test & Run** section of
[`README.md`](../../README.md).

## Alternatives considered

- **`docker-outside-of-docker` (mount the host socket)** — convenient, but exposes the host daemon to
  everything in the container; an unacceptable blast radius for autonomous agents.
- **docker-in-docker** — a nested, privileged daemon; isolated from the host but therefore *cannot*
  manage the host's containers (the actual goal), and adds privileged-container risk.
- **Rootless Podman/Docker inside the container** — isolated and unprivileged, but again a *separate*
  engine that does not manage host containers.
- **Bundling a language toolchain in the base** — premature for a template meant to fit any stack.
- **No container tooling at all** — loses the host-container management use-case entirely.

## Sources / Prior art

- Dev Container Features and specification — <https://containers.dev/features>.
- VS Code Dev Containers — forcing an extension to run locally/remotely via `remote.extensionKind`:
  <https://code.visualstudio.com/docs/devcontainers/containers>.
- Docker daemon attack surface (why mounting the socket grants host-level control):
  <https://docs.docker.com/engine/security/#docker-daemon-attack-surface>.

## Consequences

- Positive: small image, fast start, no daemon to manage; the container cannot control the host
  engine; VS Code still manages host containers through the host-side extension.
- Negative / trade-offs: coding agents inside the container cannot build or run containers; no
  toolchain works out of the box until a project adds one; the host-management path depends on
  installing the extension on the host and on the `remote.extensionKind` pin.
- Follow-ups: each project records its own toolchain choice (a new ADR if it constrains future
  choices) and completes the **Build, Test & Run** commands. If in-container container builds become
  genuinely necessary, add an **isolated** (rootless) engine through a new ADR rather than mounting the
  host socket.
