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

## Index

**Status legend:** 🟢 accepted · 🟡 proposed · 🔴 rejected · ⚪ superseded

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-agent-governance-model.md) | Specification + ADRs governed through a single `AGENTS.md` | 🟢 accepted |
| [0002](0002-dev-container-runtime.md) | Debian Dev Container without host Docker access | 🟢 accepted |
| [0003](0003-rust-toolchain-and-workspace.md) | Rust toolchain and a three-crate Cargo workspace | 🟢 accepted |
| [0004](0004-opamp-wire-contract-and-transport.md) | OpAMP wire contract from vendored proto, plain-HTTP transport first | 🟢 accepted |
| [0005](0005-server-runtime-and-rudimentary-ui.md) | Server on axum with in-memory fleet state and a rudimentary UI | 🟢 accepted |
| [0006](0006-supervisor-host-os-service-and-cli.md) | Supervisor Host as a cross-platform OS service with a subcommand CLI | 🟢 accepted |
| [0007](0007-in-place-self-update-with-rollback.md) | Self-update of the Supervisor Host via versioned installs with rollback (the Updater) | 🟢 accepted |
| [0008](0008-release-pipeline-and-versioning.md) | Release pipeline with tag-derived strict SemVer and per-platform archives | 🟢 accepted |
| [0009](0009-native-installer-packages.md) | Native installer packages for the Supervisor Host, payload-only and service-neutral | 🟡 proposed |

