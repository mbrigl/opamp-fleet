# NUC-container-hides-host-daemon: A Dev Container keeps the host daemon out of reach, its Features locked, and each project under a path of its own

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** `.devcontainer/`, `.vscode/settings.json`, `scripts/check-devcontainer.sh`, `scripts/test-check-devcontainer.sh`, and `scripts/fixtures/check-devcontainer.txt`

## Context

The template ships a Dev Container so that the first run is reproducible without committing to a
language toolchain. Agents often want Docker access. The obvious way, the
`docker-outside-of-docker` Feature, bind-mounts the **host** Docker socket and hands host-level
control to anything inside. Managing host containers from VS Code does not need the socket.

The container also pulls in third-party code that runs next to the agent credentials it mounts:
**Features**, executed at build time. A Feature tag is mutable and can be repointed, so a tag alone
does not say what a commit builds.

A third cost comes from the mount. An agent that remembers across sessions keys what it remembers
by the directory it works in, and every session loads that memory before its first step. A
container that mounts every project at the same path gives every project one memory.

A Dev Container is a choice. A derived project may work without one, and a rule on a file it does
not have cannot hold. So these rules bind only a project that has a Dev Container.

## Decision

We will keep the host daemon out of a Dev Container's reach, lock its Features, and mount each
project under a path of its own.

1. **Scope.** These clauses bind only a project with a Dev Container definition under
   `.devcontainer/`.
2. **Socket.** The definition must not mount the host's Docker socket or add a Feature that
   mounts it, in any variant.
3. **Features.** The definition must reference each Feature by its major version tag alone. The
   project must commit `.devcontainer/devcontainer-lock.json`, naming exactly these Features.
4. **Host side.** A project that uses VS Code with its Dev Container must pin the
   container-management extension to the host side in `.vscode/settings.json`, through
   `remote.extensionKind`.
5. **Mount path.** The definition must mount the repository at `/workspaces/` and its host
   directory name, not at a path every project shares.
6. **Git settings.** The container must enable the git hooks and the merge settings they rely on
   at every start.

**Out of scope:**

- the base image, Features, and extensions, and whether extensions are pinned;
- the toolchain, a required role of [NUC-base-is-fixed §3](NUC-base-is-fixed.md);
- an engine inside the container, and how the work reaches the containers it needs;
- work outside the container;
- hardening the container;
- refreshing the lock;
- the local gate, which [NUC-checks-are-bash §6](NUC-checks-are-bash.md) decides.

## Latitude

A project may choose:

- whether it has a Dev Container at all; without one, no clause of this ADR binds it;
- the base image, the Features, and the extensions, and whether it pins the extensions;
- how the work reaches the containers it needs, from the entry
  [Container access](../../.agents/skills/propose-adr/concepts/container-access.md) of the
  catalogue;
- whether work may happen outside the container;
- how and when it refreshes the lock, by hand or by an automated update.

Only the way to the containers has a catalogue entry. The other choices need no ADR.

## Alternatives considered

- **`docker-outside-of-docker`.** It mounts the host socket and hands host-level control to
  everything inside, the agent included.
- **Ignore the Feature lock.** A repointed tag reaches the credentials before anyone sees a diff.
- **Digests directly in `devcontainer.json`.** It buries hashes in the hand-edited file and loses
  the readable tag.
- **One fixed mount path, such as `/workspace`.** It is shorter to type. Every project opened in the
  container then shares the memory an agent keeps under it, so each session pays for all of them.
- **Rules that bind every project.** A project without a Dev Container could not meet them, and a
  check would fail on files the project does not have.

## Sources / Prior art

- Dev Container specification — <https://containers.dev/implementors/json_reference/> and
  <https://containers.dev/features>.
- `remote.extensionKind` — <https://code.visualstudio.com/docs/devcontainers/containers>.
- Docker daemon attack surface —
  <https://docs.docker.com/engine/security/#docker-daemon-attack-surface>.

## Consequences

- Positive: the security posture is the same in every variant.
- Positive: the same commit gives the same Features, and a changed digest shows up in review.
- Positive: an agent's memory stays with the project it was written in.
- Positive: a project without a Dev Container carries no rule it cannot meet.
- Negative: Feature updates stop arriving on their own, and a stale lock is invisible.
- Negative: a project that moves off a shared mount path leaves the memory filed under the old one
  behind. The linked worktrees it made there name the old path until `git worktree repair` runs.
- Negative: two projects whose directories on the host share a name still share one memory.
- Follow-ups: whether an automated update path for Features is wired up.

## Enforcement

`scripts/check-devcontainer.sh`, job `devcontainer`, holds clauses 1 to 6. Its self-test cites
this ADR. The check fails when:

- a `.devcontainer/` holds no `devcontainer.json` (clause 1);
- the definition mounts a host socket or adds a Feature that would (clause 2);
- a Feature lacks a bare major version tag, or the lock is missing or names other Features
  (clause 3);
- a tree with `.vscode/` lacks the host-side pin in `.vscode/settings.json` (clause 4);
- `workspaceFolder` or the `workspaceMount` target is not
  `/workspaces/${localWorkspaceFolderBasename}` (clause 5);
- `postStartCommand` does not set `core.hooksPath`, `merge.ff`, `pull.ff`, and
  `merge.adr-index.driver` (clause 6).

Without `.devcontainer/`, the check passes and says that nothing binds (clause 1).

**Not mechanically decidable:** the check reads only the default definition and builds nothing.
The review holds variants (clause 2), current lock digests (clause 3), and a compose file's mount
(clause 5).
