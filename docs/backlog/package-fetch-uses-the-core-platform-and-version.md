# package fetch maps platforms through the core's aliases

**Kind:** defect, structure.

**Observed.** `opamp-fleetctl package fetch` keeps a platform alias table of its own,
`normalize_os` and `normalize_arch` in `crates/fleet-tools/src/fetch.rs` (near lines 798 and 806
on `main`), beside `fleet_core::platform`, and the two disagree: the core maps `osx`, `win`,
`win32`, `win64`, `x86-64` and `x64`, the tool maps `i386` and knows only `macos`. On `main` the
tool also orders versions by a hand-written numeric key (near line 632) rather than by
`fleet_core::version`'s precedence. ADR-0008 puts the platform aliases and the version in
`fleet-core`, because the tool makes what an end must accept.

**What the old branch had.** Revision ticket 07 of `feat/revision-2026-10-31`, open, not worked.
`feat/core-repack` already orders the versions by `fleet_core::version::precedence` in `fetch.rs`,
so once it lands what is left is the alias table: map upstream names through
`fleet_core::platform`, and add to the core an alias the tool needs and the core lacks.
