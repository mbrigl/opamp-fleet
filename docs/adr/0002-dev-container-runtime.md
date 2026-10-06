# ADR-0002: Debian Dev Container without host Docker access, pinned to the distribution it builds for

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

## Context

The template must provide a reproducible, ready-to-use environment without committing to any language
toolchain (it has to stay language-agnostic). Coding agents frequently want Docker access, and the
obvious way to grant it — the `docker-outside-of-docker` Feature — bind-mounts the **host** Docker
socket into the container. That mount means any code or coding agent running in the container can drive
the host daemon — effectively host-level access and a large blast radius (recorded in
[`SECURITY.md`](../../SECURITY.md)). Examining the actual requirement, the only thing we need is to
**manage the host's containers from VS Code**. That is a host-side capability and does not require
exposing the socket to the container at all.

The container is also the build host for a shipped artifact.
[ADR-0034](0034-repacked-icinga-2-artifacts.md) and
[ADR-0034](0034-repacked-icinga-2-artifacts.md) make the Icinga 2
artifact a repack of vendor packages that bundles **everything except glibc**, and therefore reaches
exactly the hosts whose glibc is at least the build host's. The build host is not a flag and cannot be
one — `opamp-package-fetch` refuses to build for a distribution the host is not, because the tree
carries the libraries `ldd` resolves *there*. ADR-0034 states the consequence plainly: *"picking the
build host is now a real decision, with a floor that has to be chosen deliberately."*

Two facts make a floating image tag the wrong shape for that decision:

- **A floating tag moves the reach.** The day `:debian` moves from bookworm to trixie, every artifact
  built here silently stops running on Debian 12, Ubuntu 22.04 and RHEL 9. Nothing in the repository
  would change, no review would see it, and the failure surfaces on the fleet as *"does not run on this
  host"* after a rollout.
- **Without Icinga's runtime libraries the container cannot build the artifact at all.** The repack
  refuses by name (correctly) rather than shipping a tree missing them. A throwaway `rust:bookworm`
  container per build works around this, but leaves the Dev Container unable to run the operator tool
  this repository ships.

## Decision

### Isolation from the host

1. **The Dev Container is based on `mcr.microsoft.com/devcontainers/base:debian12` with no Docker
   Feature**, so no host Docker socket is mounted and the container has **no access to the host
   daemon**.
2. **Host containers are managed from a VS Code extension pinned to the host (UI) side** via
   `remote.extensionKind` in [`.vscode/settings.json`](../../.vscode/settings.json), which talks to the
   host engine directly even when the folder is reopened in the container.
3. **The base image ships no language toolchain.** Each project adds its own and fills in the
   **Build, Test & Run** section of [`README.md`](../../README.md).

### The image is the artifact's reach

4. **The image is pinned to a Debian release rather than the floating tag, chosen as the oldest
   distribution the fleet's artifacts must serve.** bookworm's vendor packages declare
   `libc6 >= 2.34`, so artifacts built on `debian12` reach Debian 12+, Ubuntu 22.04+ and RHEL 9+ —
   across families, because glibc is backward compatible (ADR-0034).
5. **The image line is the reach.** Changing it is a decision about which hosts the fleet can serve,
   not a maintenance chore, and the comment on it says so.
6. **The libraries `icinga2-bin` needs beyond the base image are installed with the developer
   tooling** — six Boost packages and `libprotobuf-lite32`. They are not used by anything in this
   repository; they are what the artifact is built *from*, which is why they are named here rather
   than left to a per-build container.
7. **The Dev Container is the documented build host for the Linux artifact.** A container of another
   distribution stays the way to build a *different* reach, which is the case the recipe presents as
   the exception it is.

## Alternatives considered

- **`docker-outside-of-docker` (mount the host socket)** — convenient, but exposes the host daemon to
  everything in the container; an unacceptable blast radius for autonomous agents.
- **docker-in-docker** — a nested, privileged daemon; isolated from the host but therefore *cannot*
  manage the host's containers (the actual goal), and adds privileged-container risk.
- **Rootless Podman/Docker inside the container** — isolated and unprivileged, but again a *separate*
  engine that does not manage host containers.
- **Bundling a language toolchain in the base** — premature for a template meant to fit any stack.
- **No container tooling at all** — loses the host-container management use-case entirely.
- **Keep the floating `:debian` tag.** Less to maintain, and correct for a container that only compiles
  this repository's own code. Rejected: the reach of a shipped artifact would then change on whichever
  day upstream retags, with no commit to review it in.
- **Keep building in a throwaway `rust:bookworm` container.** Works, and it keeps the Dev Container free
  of Boost. Rejected as the default: it costs a container per build and leaves the Dev Container unable
  to run a tool this repository ships — a tool whose whole contract is that it builds for the host it
  runs on.
- **Pin to `bullseye` for a 2.30 floor.** Widest reach ADR-0034 tabulates, and it would serve
  Debian 11 and Ubuntu 20.04 too. Rejected for now: it dates the whole development environment for
  hosts nobody in this deployment runs, and the pin can be lowered the day one appears — which is
  exactly the deliberate decision clause 5 wants it to be.
- **Install the libraries at build time from the tool instead of the image.** Would keep the image
  neutral, and it would mean an operator tool running `apt-get install` on its host. Rejected: the
  refusal that names the packages is the better contract, and ADR-0034 already settled that the
  build host is equipped in advance.

## Sources / Prior art

- Dev Container Features and specification — <https://containers.dev/features>.
- VS Code Dev Containers — forcing an extension to run locally/remotely via `remote.extensionKind`:
  <https://code.visualstudio.com/docs/devcontainers/containers>.
- Docker daemon attack surface (why mounting the socket grants host-level control):
  <https://docs.docker.com/engine/security/#docker-daemon-attack-surface>.
- Measured in this container (2026-08-18): `bookworm`, glibc 2.36; `icinga2-bin` 2.16.5's unresolved
  closure was seven sonames, provided by six Boost packages and `libprotobuf-lite32`, and
  `monitoring-plugins-basic` needed nothing the base image lacked.
- [ADR-0034](0034-repacked-icinga-2-artifacts.md),
  [ADR-0034](0034-repacked-icinga-2-artifacts.md) — the bundling
  rule and the reach rule clauses 4–7 follow from.
- Debian's glibc versions per release, and Icinga's `Depends: libc6 (>= …)` per vendor build.

## Consequences

- Positive: small image, fast start, no daemon to manage; the container cannot control the host
  engine; VS Code still manages host containers through the host-side extension.
- Positive: the reach of every Linux artifact is a line in a reviewed file rather than a property of
  the day it was built.
- Positive: the Dev Container can run `opamp-package-fetch --agent icinga2` directly, so the recipe
  needs no second container and no `cargo run` somewhere else.
- Negative / trade-offs: coding agents inside the container cannot build or run containers; no
  toolchain works out of the box until a project adds one; the host-management path depends on
  installing the extension on the host and on the `remote.extensionKind` pin.
- Negative / trade-offs: the image needs a deliberate bump, and a stale pin is a real cost —
  the development environment ages with the oldest host the fleet serves.
- Negative / trade-offs: the container grows Boost and, through `libboost-regex1.74.0`, ICU. The
  artifact grows with it: 2.16.5 links `boost_regex`, which the 2.14.6 spike in ADR-0034 did not.
- Negative / trade-offs: the package names carry bookworm's versions (`1.74.0`, `32`) and have to be
  updated together with the image — two lines that must move as one.
- Follow-ups: each project records its own toolchain choice (a new ADR if it constrains future
  choices) and completes the **Build, Test & Run** commands. If in-container container builds become
  genuinely necessary, add an **isolated** (rootless) engine through a new ADR rather than mounting the
  host socket.
- Follow-ups: none for Windows, whose artifact comes from an MSI and needs no such host. If the RPM
  path is ever built (ADR-0034 left it optional), it needs a build host of its own, and this
  decision is the shape that question takes.
