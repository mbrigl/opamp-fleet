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
| [0009](0009-version-baking-comparison-and-display.md) | The version — strict SemVer baked in at compile time, `-dev` for non-release builds, commit-hash build metadata that is provenance, and one helper every surface reads | 🟢 accepted |
| [0010](0010-client-os-service.md) | The Client as a multi-instance native OS service, installed and run through its own CLI | 🟢 accepted |
| [0011](0011-supervisor-mode-core-and-plugins.md) | Supervisor Mode — a hexagonal supervision core, compiled-in plugins that own their process kind, n Agents over one connection | 🟢 accepted |
| [0012](0012-selector-targeted-configurations-and-rest-api.md) | Selector-targeted Configurations and the OpenAPI-described REST API | 🟢 accepted |
| [0013](0013-agent-admission-on-the-opamp-endpoint.md) | Agent admission on the OpAMP endpoint — optional proofs, each enforced once configured, proving fleet membership | 🟢 accepted |
| [0014](0014-server-driven-connection-settings.md) | Server-driven OpAMP connection settings — credential rotation, offered heartbeat, movable endpoint | 🟢 accepted |
| [0015](0015-package-delivery-for-managed-processes.md) | Package delivery for Managed Processes — verified download, Supervisor-applied, health-gated, rolled back | 🟢 accepted |
| [0016](0016-what-a-package-is-and-how-it-is-aimed.md) | What a package is, and how the Server — not the host — aims it at Agents | 🟢 accepted |
| [0017](0017-package-artifacts.md) | A package's artifacts — an uploaded archive or a URL the Agents fetch, unpacked by the Agent, `.tar.gz` or encrypted `.7z` | 🟢 accepted |
| [0018](0018-client-self-update.md) | The Client updates itself — its own Agent, a staged version, a restart it does not issue, and a consent that names its package | 🟢 accepted |
| [0019](0019-supervisor-directory-programs-and-supervisor-set.md) | One directory per Supervisor — the program it runs, the paths that name it, and where its blocks come from | 🟢 accepted |
| [0020](0020-what-lives-in-which-crate.md) | What lives in which crate — the Client is a library with a thin binary on top, so a test can reach what it tests | 🟢 accepted |
| [0021](0021-release-pipeline-and-artifacts.md) | A release is a `version/*` tag, built for five targets and published as named artifacts an operator and the fleet can install | 🟢 accepted |
| [0022](0022-the-client-is-named-opamp-fleet-client.md) | The Client ships as `opamp-fleet-client` — the artifact, the installed binary, the version directory, and one service name on every platform | 🟢 accepted |
| [0023](0023-what-describes-an-agent.md) | What describes an Agent — `service.name` carries the type, `service.instance.name` the operator's name | 🟢 accepted |
| [0024](0024-agents-report-their-own-telemetry.md) | An Agent reports its own telemetry over OTLP/HTTP, through the OpenTelemetry SDK, to the destinations the Server offers | 🟢 accepted |
| [0025](0025-gateway-mode.md) | Gateway Mode — a lazily grown pool, sticky by `instance_uid`, and a hop that invents nothing | 🟢 accepted |
| [0026](0026-the-servers-agent-records.md) | The Server's Agent records — an Agent that stops reporting goes stale, and an operator can forget one | 🟢 accepted |
| [0027](0027-interoperability-against-opamp-go.md) | Conformance proved against `opamp-go` — a second reading of the specification | 🟢 accepted |
| [0028](0028-a-rollout-is-an-explicit-act.md) | A rollout is an explicit act — saving never distributes, and the operator releases per Agent or for all matching Agents | 🟢 accepted |
| [0029](0029-the-linux-service-executes-from-opt.md) | The Linux service executes from `/opt`, and the system service may run under an operator-named account that owns the instance's files | 🟢 accepted |
| [0030](0030-glpi-agent-supervision-and-packages.md) | The GLPI Agent through the fleet — supervised as the Supervisor's own child, machine-installed or from self-contained packages on both platforms | 🟢 accepted |

