# Architecture Decision Records

This directory contains all Architecture Decision Records (ADRs) for this project. Accepted ADRs are
**binding** for humans and coding agents alike (see [`AGENTS.md`](../../AGENTS.md) in the repository
root). ADRs derive from the specification in [`docs/SPECIFICATION.md`](../SPECIFICATION.md).

## Mechanics

The rules live in [`AGENTS.md` §3](../../AGENTS.md#3-adr-rules): when an ADR is written, how it
is developed, who flips its status, that an accepted one is immutable and changed only by
supersession. This section carries only the mechanics of the record;
[`scripts/check-docs.sh`](../../scripts/check-docs.sh) names in its header which of them it
verifies.

- **File.** Copy [`template.md`](template.md) to `NNNN-short-title.md` with the next free number.
  The `# ADR-NNNN` heading matches the filename, and the numbers run `0001..N` without gaps.
- **Index.** The tables below change in the same pull request as the ADR it describes, for
  additions, supersessions, and status flips alike. The status is shown via the legend's bullet
  and the ADR's `Applies to` header is mirrored verbatim in its own column. The index routes a
  reader from a change to the decisions that bind it, so a row says what its ADR governs without
  the file being opened. Two tables keep what a session reads apart from what it looks up: an
  accepted or proposed ADR sits under **Binding**, a superseded or rejected one under
  **Superseded and rejected**, and a status flip moves the row.
- **Numbers are permanent.** Never renumber, delete, or merge ADRs: other ADRs, commits
  (`Implements ADR-NNNN`), and code may reference a number. A superseded ADR keeps its file and
  its body. Its `Status` line flips to `⚪ superseded by ADR-NNNN` in the pull request that lands
  the superseding ADR, whose `Supersedes` field names it back.
- **Sprawl is curbed by supersession, never by editing.** One ADR may supersede several whose
  decisions have grown into one. Its `Supersedes` field names each, and each flips its status in
  the same pull request, so the set an agent reads for a change shrinks while every number and
  every body stays.
- **Cite only what exists.** Every `ADR-NNNN` reference names a file already in this directory.
  An anticipated follow-up is described by topic ("a follow-up ADR on session storage"), never by
  a number. In Markdown, cite an ADR as a link to its own file.

## Index

An ADR whose `Deciders` line names the **NUC maintainer** is inherited from the template. It
binds a derived project once its own maintainer adds their name to `Deciders` and flips the
`Status` line to `🟢 accepted` ([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules)). Change an
inherited decision by superseding it, never by editing. Once the template setup is done, check
12 of [`scripts/check-docs.sh`](../../scripts/check-docs.sh) fails on an inherited ADR still
proposed.

**Status legend:** 🟢 accepted · 🟡 proposed · 🔴 rejected · ⚪ superseded

### Binding

The decisions in force: accepted, and proposed ones that bind the work implementing them. This
is the table a session reads.

| ADR | Title | Applies to | Status |
|-----|-------|------------|--------|
| [0001](0001-agent-governance-model.md) | The document set: one specification, one ADR record, one `AGENTS.md`, one overview, one glossary, one conventions file | every document that carries rules for humans or agents, the architecture overview, the glossary, and the conventions | 🟢 accepted |
| [0002](0002-dev-container-runtime.md) | The Dev Container keeps the host daemon out of reach and its Features locked | `.devcontainer/`, `.vscode/settings.json`, and everything the container pulls in | 🟢 accepted |
| [0003](0003-decisions-verified-by-tests.md) | Every accepted decision and every success criterion is verified by a test that cites it | every accepted ADR, the Goals and Quality Goals of `docs/SPECIFICATION.md`, and the tests that verify them | 🟢 accepted |
| [0004](0004-feature-layer.md) | Work larger than one session is planned on the issue tracker: a feature spec cut into tracer-bullet tickets | every change larger than one agent session, the issue templates, the pull request template, and the `Last design revision` line of `docs/ARCHITECTURE.md` | 🟢 accepted |
| [0005](0005-procedures-as-skills.md) | The procedures of the rule file are Agent Skills under `.agents/skills/`, each carrying a how and no rule of its own | `.agents/skills/`, the pointers under `.claude/skills/`, and every document that describes how a procedure of `AGENTS.md` is carried out | 🟢 accepted |
| [0006](0006-architecture-style.md) | Application code is structured as ports and adapters, with every dependency pointing at the core | every module of the system's code, the golden path, and the structural test that decides the dependency direction | 🟡 proposed |
| [0007](0007-action-references.md) | GitHub Actions are referenced by their major version tag and kept current by Dependabot | every `uses:` reference in `.github/workflows/`, and `.github/dependabot.yml` | 🟢 accepted |
| [0008](0008-template-releases.md) | The template is released as SemVer tags on `main`, and every repository names the release it carries | the tags of the template repository, the **Template** section of `README.md`, and the pull request that lands a release | 🟢 accepted |
| [0009](0009-client-modes-and-the-gateway.md) | One Client binary with two composable modes, carrying n Agents over m connections | crates/client/src/gateway/, crates/client/src/supervisor/endpoint.rs, the [gateway] configuration section, and every place either end keeps per-Agent state | 🟢 accepted |
| [0010](0010-protocol-baseline-and-conformance.md) | The protocol is pinned to a released Baseline, compiled from a vendored schema, and checked against opamp-go | docs/CONFORMANCE.md, crates/opamp/build.rs, crates/opamp/proto/, the Protocol Baseline check in scripts/check-docs.sh, and every change that adds or alters protocol behaviour | 🟢 accepted |
| [0011](0011-workspace-crates-and-configuration.md) | Four crates in one Cargo workspace on tokio and axum, a shared crate by measurement, and TOML configuration | Cargo.toml, crates/opamp/, crates/client/src/lib.rs and main.rs, crates/package-tools/, the bundled UI under crates/server/static/, server.toml and supervisor.toml, and every new crate, module placement or dependency | 🟢 accepted |
| [0012](0012-transports-tls-and-the-servers-two-planes.md) | Both OpAMP transports on both ends over rustls, and a Server on two listeners split by audience with bounded connection setup | the OpAMP endpoint and both transports in `crates/server/src/transport.rs` and `crates/client/src/transport/`, TLS in `crates/server/src/tls.rs` and `crates/client/src/tls.rs`, how the Server binds and serves in `crates/server/src/listen.rs` and `main.rs`, and the `listen`, `[rest]` and `[tls]` keys of `server.toml` | 🟢 accepted |
| [0013](0013-versions.md) | `Cargo.toml` decides the version, the build stamps it with git's provenance, and the commit is compared nowhere | `Cargo.toml` `[workspace.package] version`, `crates/opamp/build.rs`, `crates/opamp/src/version.rs`, the `version` job of `.github/workflows/release.yml`, and every surface that states, compares or displays a version | 🟢 accepted |
| [0014](0014-the-client-as-an-installed-service.md) | The Client installs itself as a native service named after a build-time product name, from a versioned layout it can rewrite | `crates/client/src/cli.rs`, `crates/client/src/main.rs`, `crates/client/src/service/`, `crates/client/src/config_init.rs`, `crates/client/src/logging.rs`, `crates/client/src/product.rs`, `crates/client/build.rs`, and every path, name or account an installed Client uses | 🟢 accepted |
| [0015](0015-supervisor-mode-and-its-kinds.md) | Supervisor Mode is a hexagonal core with compiled-in kinds, and a kind is the authority on its own agent | crates/client/src/supervisor/ (core, ports, process runner, endpoint, every kind), the `[[supervisor]]` and `[supervisors]` sections of supervisor.toml, docs/artifacts/ | 🟢 accepted |
| [0016](0016-configurations-and-the-rest-api.md) | Configurations are named, Selector-targeted resources of an OpenAPI-described REST API | crates/server/src/configs.rs, crates/server/src/api.rs, the Configuration routes and the OpenAPI document under /api/v1, config_dir in server.toml, role handling in crates/client/src/storage.rs and the Supervisor plugins | 🟢 accepted |
| [0017](0017-admission-and-authentication.md) | Admission stacks a fleet credential and a Server-issued client certificate, proves membership rather than identity, and the Operator plane has Basic authentication of its own | Admission on `/v1/opamp` in `crates/server/src/transport.rs`, `credentials.rs`, `tls.rs` and `ca.rs`, the Operator plane's guard in `crates/server/src/api.rs`, the Client's credential, identity and enrolment in `crates/client/src/config.rs`, `tls.rs` and `csr.rs`, and the `[auth]`, `[tls]`, `[client_ca]` and `[rest.auth]` sections of `server.toml` and `supervisor.toml` | 🟢 accepted |
| [0018](0018-connection-settings-and-server-capabilities.md) | The Server offers connection settings in the Baseline's classes under one hash, the Client proves only what it can and acknowledges the whole offer, and a Server's capabilities bind what the Client reports | the `[connection_offer]` section of `server.toml`, the offer composition and capability declaration in `crates/server/src/fleet.rs`, the Client's offer handling in `crates/client/src/connection.rs`, `crates/client/src/transport/mod.rs` and `crates/client/src/engine.rs`, its persisted `connection-settings.pb`, and every gate on a Server capability in `crates/client/src/supervisor/agent.rs` | 🟢 accepted |
| [0019](0019-package-delivery-on-the-agent.md) | A Supervisor downloads, verifies, unpacks, swaps and health-gates a package, and rolls back only to a predecessor | crates/client/src/packages.rs, crates/client/src/archive.rs, crates/client/src/install.rs, crates/client/src/supervisor/process.rs, the package handling in crates/client/src/supervisor/agent.rs, the `[packages]` and `[updates]` sections and the `program_path` and `retain_previous_secs` keys of `supervisor.toml` | 🟢 accepted |
| [0020](0020-the-package-store.md) | The Server stores each release as one package per Agent type and version, with one entry per Platform, and offers an Agent only the artifact built for its type and machine | `crates/server/src/packages.rs`, the package routes and the download route of `crates/server/src/api.rs`, `packages_dir` and the package limits in `crates/server/src/config.rs`, the platform table in `crates/opamp/src/attributes.rs`, the `host.arch` the Client reports, the Packages tab of the bundled UI | 🟢 accepted |

### Superseded and rejected

The record of what was decided against or replaced: read one when a change touches what it
governed, to learn why the current decision stands. A row moves here in the pull request that
flips its status.

None yet.
