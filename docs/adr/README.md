# Architecture Decision Records

This directory contains all Architecture Decision Records (ADRs) for this project.
Accepted ADRs are **binding** for humans and coding agents alike (see [`AGENTS.md`](../../AGENTS.md)
in the repository root). ADRs derive from the specification in [`docs/SPECIFICATION.md`](../SPECIFICATION.md).

## Process

1. Copy [`template.md`](template.md) to `NNNN-short-title.md` (next free number).
2. Fill in context, decision, alternatives, and consequences. Set status `proposed`.
3. A human reviewer accepts or rejects the ADR. **Only humans change the status.**
4. Add the ADR to the index below, with its status shown via the colored bullet from the legend.
5. A decision is changed by a *new* ADR that supersedes the old one — never by editing an
   accepted ADR.
6. **Once this template is in use, ADRs are immutable and their numbers are permanent.** Never
   renumber, delete, or merge ADRs — other ADRs, commits (`Implements ADR-NNNN`), and code may
   reference a number. Superseded ADRs stay as historical record (status `superseded by ADR-NNNN`);
   filter active ones via the Status column. To curb sprawl, supersede — do not consolidate. (The
   template itself may still consolidate its own seed ADRs before any project builds on them, since
   nothing external references those numbers yet.)
7. **Never reference an ADR number that does not exist yet.** Every `ADR-NNNN` reference must point
   to a file that is already present in this directory. Anticipated follow-up decisions are
   described by topic (e.g., "a follow-up ADR on session storage") in the Consequences section —
   the concrete number is cited only once that ADR file exists.

## Index

**Status legend:** 🟢 accepted · 🟡 proposed · 🔴 rejected · ⚪ superseded

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-agent-governance-model.md) | Specification + ADRs governed through a single `AGENTS.md` | 🟢 accepted |
| [0002](0002-dev-container-runtime.md) | Debian Dev Container without host Docker access | 🟢 accepted |
| [0003](0003-client-modes-and-connection-multiplexing.md) | One Client binary with two composable modes, multiplexing Agents over a connection pool | 🟢 accepted |
| [0004](0004-protocol-baseline-and-conformance-tracking.md) | Pin the protocol to a Baseline version and track conformance in a dedicated document | 🟢 accepted |
| [0005](0005-workspace-and-server-runtime.md) | Three-crate Cargo workspace; tokio runtime; axum serves OpAMP, REST API, and the bundled UI on one port | 🟢 accepted |
| [0006](0006-proto-vendoring-and-codegen.md) | Vendor the Baseline's protobuf schema and compile it with prost via protox (no system protoc) | 🟢 accepted |
| [0007](0007-dual-transport-and-tls.md) | Both OpAMP transports on both ends — plain HTTP(S) polling and WebSocket on one endpoint, TLS via rustls | 🟢 accepted |
| [0008](0008-toml-configuration.md) | TOML configuration files for the Server and the Client | 🟢 accepted |
| [0009](0009-version-derivation-and-baking.md) | Version computed from git in `build.rs` — strict SemVer from `version/*` tags, `-dev` pre-release for non-release builds, commit-hash build metadata | 🟢 accepted |
| [0010](0010-client-os-service-and-cli.md) | Client as a multi-instance OS service — clap subcommand CLI, per-instance identity, versioned install layout | 🟢 accepted |
| [0011](0011-supervisor-mode-hexagonal-core-and-plugins.md) | Supervisor Mode — hexagonal supervision core, compiled-in plugins, n Agents over one connection | 🟢 accepted |
| [0012](0012-selector-targeted-configurations-and-openapi-rest-api.md) | Selector-targeted Configurations and the OpenAPI-described REST API | 🟢 accepted |
| [0013](0013-opamp-endpoint-authentication.md) | Static Basic and Bearer authentication on the OpAMP endpoint, optional by default | 🟢 accepted |
| [0014](0014-server-driven-connection-settings.md) | Server-driven OpAMP connection settings — credential rotation, offered heartbeat, movable endpoint | 🟢 accepted |
| [0015](0015-package-delivery-for-managed-processes.md) | Package delivery for Managed Processes — verified download, Supervisor-applied, health-gated, rolled back | 🟢 accepted |
| [0016](0016-configuration-content-role.md) | Carry the Baseline's `AgentConfigFile.role` through the Configuration model | 🟢 accepted |
| [0017](0017-selector-targeted-packages.md) | Selector-targeted packages, chosen by the Server rather than named on each host | 🟢 accepted |
| [0018](0018-packages-imported-from-a-url.md) | A package is an uploaded archive or a URL the Agents fetch — unpacked by the Agent, `.tar.gz` or encrypted `.7z` | 🟢 accepted |
| [0019](0019-one-step-back.md) | One step back — the package store remembers the version it replaced | 🟢 accepted |
| [0020](0020-client-self-update.md) | The Client updates itself — its own Agent, a staged version, and a restart it does not issue | 🟢 accepted |
| [0021](0021-supervisor-directory-and-path-implied-package-consent.md) | One directory per Supervisor — a bare program name means the Client owns it and updates it, an absolute path means it does not | 🟢 accepted |
| [0022](0022-supervisor-path-placeholders-in-process-arguments.md) | A Foreign Agent is pointed at its own directory by placeholder, never by a path an operator has to keep in sync | 🟢 accepted |
| [0023](0023-multi-file-packages.md) | A package may be a directory tree, unpacked whole beside the one it replaces | 🟢 accepted |
| [0024](0024-client-library-target.md) | The Client is a library with a thin binary on top, so a test can reach what it tests | 🟢 accepted |
| [0025](0025-release-pipeline-and-artifacts.md) | A release is a `version/*` tag built for five targets and published as `.7z` artifacts the Client can install | 🟢 accepted |
| [0026](0026-version-from-cargo-toml.md) | The release version is the one in `Cargo.toml`, and the pipeline creates the tag from it | 🟢 accepted |
| [0027](0027-interactive-install-writes-the-first-configuration.md) | The first configuration is written by an interactive install — asked once, never overwritten, validated before the service is registered | 🟢 accepted |
| [0028](0028-the-client-is-named-opamp-fleet-client.md) | The Client ships as `opamp-fleet-client` — the artifact, the installed binary, and the version directory | 🟢 accepted |
| [0029](0029-a-version-is-compared-and-shown-without-its-build-metadata.md) | A version is compared and shown without its build metadata — the commit is provenance, not identity | 🟢 accepted |
| [0030](0030-one-service-name-on-every-platform.md) | One service name on every platform — `opamp-fleet-client`, with the instance as a suffix | 🟢 accepted |
| [0031](0031-per-platform-package-variants.md) | One platform vocabulary from the release file name to the offer — a package is one name with one artifact per platform | 🟢 accepted |
| [0032](0032-release-artifacts-separate-their-fields-with-underscores.md) | A release artifact separates its four fields with `_` — `name_version_os_arch.7z` | 🟢 accepted |
| [0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md) | An Agent's type and its instance name are two attributes — `service.name` carries the type, `service.instance.name` the operator's name | 🟢 accepted |
| [0034](0034-a-package-states-the-agent-type-it-is-built-for.md) | A package states the Agent type it is built for, and reaches no Agent of another | 🟢 accepted |
| [0035](0035-mutual-tls-and-the-server-issued-client-certificate.md) | Mutual TLS with a Server-issued client certificate — the credential bootstraps it, the CSR flow renews it | 🟢 accepted |
| [0036](0036-agents-report-their-own-telemetry.md) | An Agent reports its own telemetry over OTLP/HTTP, through the OpenTelemetry SDK | 🟢 accepted |
| [0037](0037-gateway-mode.md) | Gateway Mode — a lazily grown pool, sticky by `instance_uid`, and a hop that invents nothing | 🟢 accepted |
| [0038](0038-an-agent-that-stops-reporting-goes-stale.md) | An Agent that stops reporting goes stale — liveness beside connectedness, never instead of it | 🟢 accepted |
| [0039](0039-forgetting-an-agent.md) | Forgetting an Agent — the fleet view drops a record, and reaches no host | 🟢 accepted |
| [0040](0040-interoperability-against-opamp-go.md) | Conformance proved against `opamp-go` — a second reading of the specification | 🟢 accepted |
| [0041](0041-the-client-logs-to-a-file-in-service-mode.md) | A Client running as a service logs to a file, on every platform | 🟢 accepted |
| [0042](0042-server-set-labels.md) | The Server labels an Agent — rollout rings that are not a file on the host | 🟢 accepted |

