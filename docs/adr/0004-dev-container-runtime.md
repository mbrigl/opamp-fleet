# ADR-0004: The Dev Container keeps the host daemon out of reach and its Features locked

- **Status:** 🟢 accepted
- **Date:** 2026-09-05
- **Deciders:** NUC maintainer
- **Applies to:** `.devcontainer/`, `.vscode/settings.json`, `scripts/check-devcontainer.sh`, and everything the container pulls in

## Context

The template ships a Dev Container so that the first run is reproducible without committing to a
language toolchain. Agents often want Docker access, and the obvious way — the
`docker-outside-of-docker` Feature — bind-mounts the **host** Docker socket, handing host-level
control to anything inside. Managing host containers from VS Code does not need the socket.

The container also pulls in third-party code that runs next to the agent credentials it mounts:
**Features**, executed at build time. A Feature tag is mutable and can be repointed, so a tag alone
does not say what a commit builds.

## Decision

We will keep the host container engine out of the Dev Container's reach — **no host Docker socket
is mounted and no Feature that would mount one is added**, in every variant — and pin Features by
major version tag with `.devcontainer/devcontainer-lock.json` committed, so the same commit yields
the same Features. Host containers are managed from an extension pinned to the host side via
`remote.extensionKind` in [`.vscode/settings.json`](../../.vscode/settings.json).

**Out of scope:** which base image, Features, and extensions a project picks, and whether it pins
extensions; its toolchain; whether an engine runs inside the container and how the work reaches
the containers it needs, a founding decision the catalogue entry *Container access* of the
`propose-adr` skill prepares; whether work may happen outside the container; hardening the
container, which is not a security boundary; and how the lock is refreshed.

## Alternatives considered

- **`docker-outside-of-docker`** — mounts the host socket and hands host-level control to
  everything inside, the agent included.
- **Ignore the Feature lock** — a repointed tag reaches the credentials before anyone sees a diff.
- **Digests directly in `devcontainer.json`** — buries hashes in the hand-edited file and loses the
  readable tag.

## Sources / Prior art

- Dev Container specification — <https://containers.dev/implementors/json_reference/> and
  <https://containers.dev/features>.
- `remote.extensionKind` — <https://code.visualstudio.com/docs/devcontainers/containers>.
- Docker daemon attack surface —
  <https://docs.docker.com/engine/security/#docker-daemon-attack-surface>.

## Consequences

- Positive: the security posture is the same in every variant; the same commit gives the same
  Features, and a changed digest shows up in review.
- Negative / trade-offs: Feature updates stop arriving on their own and a stale lock is invisible.
- Follow-ups: whether an automated update path for Features should be wired up.

## Enforcement

[`scripts/check-devcontainer.sh`](../../scripts/check-devcontainer.sh) (job `devcontainer` in
[`checks.yml`](../../.github/workflows/checks.yml)) reads the files this decision names and
fails when [`devcontainer.json`](../../.devcontainer/devcontainer.json) mounts a host socket or
adds a Feature that would, when a Feature carries no major version tag, when the lock is missing
or names other Features than the definition, or when
[`.vscode/settings.json`](../../.vscode/settings.json) no longer pins the container-management
extension to the host side, or when the container's `postStartCommand` no longer enables the
repository's git hooks with the merge settings they rely on (`core.hooksPath`, `merge.ff`,
`pull.ff`, `merge.adr-index.driver`), the container's share of the local gate of
[ADR-0002](0002-decisions-verified-by-tests.md). Its self-test cites this ADR
([ADR-0002](0002-decisions-verified-by-tests.md)). Not checked: a variant definition under a
subdirectory of `.devcontainer/`, whether the lock's digests are the ones the tags currently
resolve to, and the container itself — no CI job builds it.
