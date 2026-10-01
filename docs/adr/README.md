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
| [0021](0021-the-client-updates-itself.md) | The Client updates itself — as its own Agent, by a consent that stands, through a staged version and a restart it does not issue | crates/client/src/selfupdate.rs, the Client's own Agent in crates/client/src/supervisor/, the `[self_update]` section, the self-update flags of `service install`, the MSI `SELFUPDATE` property | 🟢 accepted |
| [0022](0022-a-supervisors-directory-program-and-set.md) | Each Supervisor owns one directory and runs only a program installed there, and the Server manages the set of Supervisors | crates/client/src/config.rs (supervisor_dir, program resolution), crates/client/src/supervisor/ (start, placeholders), crates/client/src/reconfigure.rs (the Supervisor-set apply), the `[[supervisor]]` blocks of supervisor.toml | 🟢 accepted |
| [0023](0023-releases-installers-and-the-name-supervisor.md) | The fleet's own agent is called `supervisor`, and a release ships it as `.tar.gz` archives and native installers that run its own install | .github/workflows/release.yml, packaging/, the `[package.metadata.deb]` and `[package.metadata.generate-rpm]` tables of crates/client/Cargo.toml, the program, Agent type and configuration-file names of the Client, `service install --endpoint` | 🟢 accepted |
| [0024](0024-what-an-agent-reports-about-itself.md) | An Agent reports its type, its operator's name, and its host as the conventions define them | AgentDescription building in crates/client/src/supervisor/agent.rs, the Agent type resolution in crates/client/src/supervisor/mod.rs, [attributes] and service_namespace in supervisor.toml, the fleet view's name and network columns | 🟢 accepted |
| [0025](0025-own-telemetry.md) | An Agent reports its own telemetry over OTLP/HTTP to the destinations the Server names | crates/client/src/telemetry.rs, the telemetry half of crates/client/src/connection.rs, the operation spans in the Client's transport, engine, reconfigure, packages, selfupdate and Supervisor modules, the Server's `[telemetry_offer]` (crates/server/src/config.rs, crates/server/src/fleet.rs), and the OpenTelemetry crates in Cargo.toml | 🟢 accepted |
| [0026](0026-the-fleet-record.md) | The Server keeps a persisted record per Agent, derives its status, forgets it only on request, and labels it | crates/server/src/fleet.rs, crates/server/src/agent_store.rs, crates/server/src/labels.rs, the /api/v1/agents routes, stale_after_secs in server.toml, the agents/ and labels/ directories under config_dir | 🟢 accepted |
| [0027](0027-rollout-and-what-reaches-an-agent.md) | A rollout is an explicit act per Agent that pins what it releases, and a package reaches an Agent only when it fits, is aimed at it, and moves it forward from what it runs | the assignments and rollout acts in `crates/server/src/fleet.rs`, the saved and retained revisions in `crates/server/src/configs.rs`, the matching and offer functions of `crates/server/src/packages.rs`, the rollout routes of `crates/server/src/api.rs`, the persisted Agent record, the rollout column of the bundled UI, the Client's check of an offer for its own package | 🟢 accepted |
| [0028](0028-glpi-agent-and-telegraf.md) | The GLPI Agent and Telegraf each get a kind of their own, and their packages are upstream's artifacts as published or repacked as a self-contained tree | crates/client/src/supervisor/glpi.rs, crates/client/src/supervisor/telegraf.rs, the zip container in crates/client/src/archive.rs, glpi_plans and telegraf_plans in opamp-package-fetch, docs/artifacts/glpi-agent.md, docs/artifacts/telegraf.md | 🟢 accepted |
| [0029](0029-icinga-2.md) | Icinga 2 runs as the `icinga2` kind from a repacked vendor tree, enrols with its Icinga master, and reaches the hosts whose glibc is at least its build host's | crates/client/src/supervisor/icinga2.rs, the preflight, version parser and process-group stop in crates/client/src/supervisor/process.rs, icinga2_plans and windows_plan in opamp-package-fetch, the Dev Container image and its system packages, docs/artifacts/icinga2.md | 🟢 accepted |
| [0030](0030-packages-and-deployments.md) | A Package is what an Agent type runs at a version, and a Deployment aims Packages at a channel, signs them, and is the only thing rolled out — an Agent belongs to at most one | `crates/server/src/packages.rs`, `crates/server/src/deployments.rs`, the package and deployment routes of `crates/server/src/api.rs`, the package assignment in `crates/server/src/fleet.rs` and `crates/server/src/agent_store.rs`, the Packages and Deployments tabs of the bundled UI, `docs/SPECIFICATION.md` | 🟡 proposed |

### Superseded and rejected

The record of what was decided against or replaced: read one when a change touches what it
governed, to learn why the current decision stands. A row moves here in the pull request that
flips its status.

None yet.
