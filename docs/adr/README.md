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
| [0002](0002-dev-container-runtime.md) | Debian Dev Container without host Docker access | ⚪ superseded by ADR-0003 |
| [0003](0003-compose-dev-environment-with-opamp-sidecars.md) | Compose-based Dev Container with OpAMP sidecars | 🟢 accepted |
| [0004](0004-rust-toolchain-dev-container.md) | Rust toolchain and build tooling in the Dev Container | 🟢 accepted |

