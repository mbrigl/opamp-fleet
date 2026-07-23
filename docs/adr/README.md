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
| [0010](0010-client-os-service-and-cli.md) | Client as a multi-instance OS service — clap subcommand CLI, per-instance identity, versioned install layout | 🟡 proposed |

