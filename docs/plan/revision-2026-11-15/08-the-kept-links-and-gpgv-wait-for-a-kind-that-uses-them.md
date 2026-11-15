# Ticket: The kept links and `gpgv` wait for a kind that uses them

## What to build

The repack of `feat/core-repack` (#179) brought two mechanisms no kind on `main` uses:

- **Kept links.** `KindDefaults::tree_links` (`crates/fleet-agent/src/supervisor/ports.rs`, near
  line 423) is `&[]` for every kind, yet the empty list is threaded through
  `supervisor/mod.rs`'s `install_target`, `InstallTarget::Tree`, `process.rs`'s
  `stage_tree` and `archive::extract_tree_tar_gz`'s `check_kept_links` and
  `write_kept_link`, which run for every tree package. On the tool's side `pack_tree` and
  `write_members` (`crates/fleet-tools/src/fetch.rs`, near line 1037) carry
  `#[allow(dead_code)]`. `KeptLink` values are made only in tests.
- **`gpgv`.** `crates/fleet-tools/src/gpgv.rs` is `#[allow(dead_code)]` throughout, and no
  command calls it.

ADR-0050 clause 57 and ADR-0053 clauses 27 and 29 decide both, so they are not dead by mistake.
[`AGENTS.md` §1](../../../AGENTS.md#1-principles) adds an abstraction only for a present need,
and code no path reaches is tested only by its own tests: the archive_tree fuzz target broke on
it unseen (#179, bd1b5bbe1). The human chooses one:

- land the first kind that keeps a link or verifies an upstream signature, so the code has a
  caller; or
- keep `fleet_core::tree` and `gpgv` with their tests, and take the plumbing out of the Client's
  install path until a kind needs it, which leaves the ADRs standing as decisions not yet used.

## Acceptance criteria

- [ ] Given the human's choice, when the change lands, then no `#[allow(dead_code)]` is left in
  `fetch.rs` and `gpgv.rs`, or each one that stays names the ticket of the kind that will call it.
- [ ] Given a tree package of a kind on `main`, when the Client installs it, then the existing
  tests pass.

## Read first

- Touches: `crates/fleet-agent/src/supervisor/`, `crates/fleet-agent/src/archive.rs`,
  `crates/fleet-tools/src/fetch.rs`, `crates/fleet-tools/src/gpgv.rs`,
  `crates/fleet-core/src/tree.rs`
- ADR-0050 clause 57, ADR-0053 clauses 27 to 29, ADR-0051 (the kinds).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero. The choice is about scope, which is the human's.
