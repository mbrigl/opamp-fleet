# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities **privately** — do not open a public issue or pull request.

- Use GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
  (**Security → Report a vulnerability**) if enabled, or
- email the maintainer: [security@hivevm.org](mailto:security@hivevm.org).

Please include enough detail to reproduce the issue (affected version/commit, steps, and impact).
We aim to acknowledge reports within a reasonable time frame and will coordinate a fix and
disclosure with you.

## Dev Container & agent execution

OpAMP Fleet runs coding agents inside the Dev Container defined in
[`.devcontainer/devcontainer.json`](.devcontainer/devcontainer.json). Two properties shape its
security posture:

- **The Dev Container has no access to the host container engine.** The host Docker/Podman socket is
  **not** mounted into the container (see [ADR-0002](docs/adr/0002-dev-container-runtime.md)),
  so code or agents running inside cannot control the host engine. Host containers are managed from a
  host-side VS Code extension (see the README), keeping that capability outside the container's reach.
  The container is still not a strong security boundary, so run only agents and code you trust in it.
- **Git and GitHub writes require explicit human approval.** The agent must get a go-ahead before
  each commit, push, or `gh` action ([`AGENTS.md`](AGENTS.md) §6). For Claude Code this is enforced,
  not just documented: [`.claude/settings.json`](.claude/settings.json) prompts on `git add`,
  `git commit`, `git push`, and `gh`. Authentication uses `gh`'s web flow with no stored tokens.

## Fleet trust model

**Admission is a fleet-wide trust boundary, not per-Agent authentication**
([ADR-0047](docs/adr/0047-admission-is-a-fleet-wide-trust-boundary.md)). A peer reaches the OpAMP
endpoint by proving *fleet membership* — the [ADR-0013](docs/adr/0013-opamp-endpoint-authentication.md)
credential and/or the [ADR-0035](docs/adr/0035-mutual-tls-and-the-server-issued-client-certificate.md)
client certificate. Neither identifies *which* Agent is speaking: an Agent's `instance_uid` is
self-asserted (the Server may itself re-key it), a certificate is deliberately not bound to it, and a
Gateway ([ADR-0037](docs/adr/0037-gateway-mode.md)) forwards many Agents' reports under one
certificate.

The consequence, which is a design property rather than a defect: **within one admitted fleet there
is no authorization between Agents.** Any admitted peer can send a report under any `instance_uid`
and update that Agent's Server-side record (health, effective config, remote-config status, and so
the Configuration offered to it) — most cleanly over plain HTTP, which offers nothing to tell two
pollers apart. This is *not* a cross-fleet or unauthenticated exposure: it is bounded by admission.

**What this means for operators:** treat one fleet (one Server, one shared admission) as a single
trust domain. Do not place mutually distrusting Agents in the same fleet; isolate them by separate
Server instance or network segment. The rationale, and the alternatives that were weighed and
rejected (binding certificates to `instance_uid`, trust-on-first-use pinning, sequence-number
checks), are in [ADR-0047](docs/adr/0047-admission-is-a-fleet-wide-trust-boundary.md).

## Supported versions

<!-- TODO: document which versions/branches receive security fixes once the project has releases. -->
The project is pre-release; a support policy will be defined once it reaches its first release.
