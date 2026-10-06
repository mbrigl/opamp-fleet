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
| [0002](0002-dev-container-runtime.md) | Debian Dev Container without host Docker access, pinned to the distribution it builds for | 🟢 accepted |
| [0003](0003-client-modes-and-connection-multiplexing.md) | One Client binary with two composable modes, multiplexing Agents over a connection pool | 🟢 accepted |
| [0004](0004-protocol-baseline-and-conformance.md) | Pin the protocol to a Baseline version, track conformance in a dedicated document, and prove it against `opamp-go` | 🟢 accepted |
| [0005](0005-workspace-and-crates.md) | Four-crate Cargo workspace on tokio and axum — the Client is a library under a thin binary, the shared crate holds what both ends implement identically, the package tools live in their own crate | 🟢 accepted |
| [0006](0006-proto-vendoring-and-codegen.md) | Vendor the Baseline's protobuf schema and compile it with prost via protox (no system protoc) | 🟢 accepted |
| [0007](0007-dual-transport-and-tls.md) | Both OpAMP transports on both ends — plain HTTP(S) polling and WebSocket on one endpoint, TLS via rustls | 🟢 accepted |
| [0008](0008-toml-configuration.md) | TOML configuration files for the Server and the Client | 🟢 accepted |
| [0009](0009-version-from-cargo-toml-and-git.md) | The version is baked at build time from `Cargo.toml` and git, read through one helper in `crates/opamp`, and compared and shown without its build metadata | 🟢 accepted |
| [0010](0010-client-os-service-and-installation-layout.md) | The Client is an OS service the product names — clap subcommand CLI, one build-time name, a versioned install layout, one account | 🟢 accepted |
| [0011](0011-supervisor-mode-and-lifecycle-port.md) | Supervisor Mode — a hexagonal supervision core, compiled-in plugins, n Agents over one connection, and one lifecycle vocabulary every plugin executes | 🟢 accepted |
| [0012](0012-selector-targeted-configurations-and-rest-api.md) | Selector-targeted Configurations with a content role and an optional Agent type, behind the OpenAPI-described REST API | 🟢 accepted |
| [0013](0013-opamp-endpoint-admission.md) | Admission on the OpAMP endpoint — optional Basic and Bearer credentials, mutual TLS with a Server-issued client certificate, and no authorization between admitted Agents | 🟢 accepted |
| [0014](0014-server-driven-connection-settings.md) | Server-driven OpAMP connection settings — credential rotation, offered heartbeat, movable endpoint | 🟢 accepted |
| [0015](0015-package-delivery-for-managed-processes.md) | Package delivery for Managed Processes — verified, unpacked by the Agent, Supervisor-applied as a file or a tree, health-gated, rolled back only to a kept predecessor | 🟢 accepted |
| [0016](0016-a-package-is-a-versioned-set.md) | A package is a versioned Set for one Agent type, aimed by a Selector and chosen by the Server — identified by name, Agent type, and version, with one entry per platform | 🟢 accepted |
| [0017](0017-client-self-update-and-its-consent.md) | The Client updates itself — its own Agent, a staged version, a restart it does not issue, and a consent that stands unless it is withdrawn | 🟢 accepted |
| [0018](0018-supervisor-directory-and-client-installed-programs.md) | One directory per Supervisor — the Client manages only programs it installs, and a Foreign Agent finds its own directories by placeholder | 🟢 accepted |
| [0019](0019-release-pipeline-and-artifact-names.md) | A release is a `version/*` tag built for five targets, published as artifacts the Client can install, and named by four fields separated by `_` | 🟢 accepted |
| [0020](0020-installing-the-client-and-native-installers.md) | The Client is installed by `service install` — the first configuration is asked once and never overwritten, and native `.deb`, `.rpm` and `.msi` packages deliver the binary and call it | 🟢 accepted |
| [0021](0021-one-platform-vocabulary.md) | One platform vocabulary from the release file name to the offer — a package is one name with one artifact per platform | 🟢 accepted |
| [0022](0022-agent-type-instance-name-and-the-supervisor-name.md) | An Agent's type and its instance name are two attributes, and the fleet's own agent is called `supervisor` — the type, the package, the release, the program and its configuration file | 🟢 accepted |
| [0023](0023-agents-report-their-own-telemetry.md) | An Agent reports its own telemetry over OTLP/HTTP, through the OpenTelemetry SDK, to destinations the Server offers as a class of their own | 🟢 accepted |
| [0024](0024-gateway-mode.md) | Gateway Mode — a lazily grown pool, sticky by `instance_uid`, and a hop that invents nothing | 🟢 accepted |
| [0025](0025-agent-records-staleness-and-forgetting.md) | Agent records persist behind a storage port — a silent Agent goes stale, and one that is not reporting can be forgotten | 🟢 accepted |
| [0026](0026-the-client-logs-to-a-file-in-service-mode.md) | A Client running as a service logs to a file, on every platform | 🟢 accepted |
| [0027](0027-server-set-labels.md) | The Server labels an Agent — rollout rings that are not a file on the host | 🟢 accepted |
| [0028](0028-agents-report-host-network-addresses.md) | Agents report the host's network addresses, CPU model, and OS build | 🟢 accepted |
| [0029](0029-supervisor-set-from-the-server.md) | The Client accepts its Supervisor set from the Server — a delivered block names only a Client-owned program, the rest of `supervisor.toml` stays the operator's, and a removed Supervisor is purged | 🟢 accepted |
| [0030](0030-a-rollout-is-an-explicit-act.md) | A rollout is an explicit act — saving never distributes, and the operator releases per Agent or for all matching Agents | 🟢 accepted |
| [0031](0031-the-glpi-agent.md) | The GLPI Agent gets a kind of its own, delivered as self-contained packages — the Windows zip as published, the Linux AppImage repacked as a tree | 🟢 accepted |
| [0032](0032-agent-and-operator-planes-on-their-own-listeners.md) | The Agent plane and the Operator plane get their own listeners — OpAMP and package downloads on `4320`, the REST API and UI on loopback `4321` behind optional Basic authentication, and both bound connection setup | 🟢 accepted |
| [0033](0033-icinga-2-supervision-and-enrolment.md) | Icinga 2 is supervised by a kind of its own — the Icinga master stays the CA, the ticket travels as a Configuration, and the block keeps only what enrolment needs | 🟢 accepted |
| [0034](0034-repacked-icinga-2-artifacts.md) | Repacked vendor packages as relocatable Icinga 2 trees — one artifact built on the oldest glibc it must serve, and the Windows artifact verified by its publisher | 🟢 accepted |
| [0035](0035-what-reaches-an-agent.md) | What reaches an Agent — fit, aim, and the version it is already running | 🟢 accepted |
| [0036](0036-a-servers-capabilities-bind-what-the-client-reports.md) | A Server's capabilities bind what the Client reports — optimistic until it speaks, and an offer outranks its bitmask | 🟢 accepted |
| [0037](0037-a-kind-knows-its-own-agent.md) | A kind knows its own agent — a block names a decision, never a layout | 🟢 accepted |
| [0038](0038-telegraf-gets-a-kind-of-its-own.md) | Telegraf gets a kind of its own | 🟢 accepted |
| [0039](0039-a-package-is-what-an-agent-type-runs-at-a-version.md) | A Package is what an Agent type runs at a version — the name and the aim leave it | 🟡 proposed |
| [0040](0040-a-deployment-aims-packages-at-a-channel.md) | A Deployment aims Packages at a channel, signs them, and is the only thing rolled out — and an Agent belongs to at most one | 🟡 proposed |
