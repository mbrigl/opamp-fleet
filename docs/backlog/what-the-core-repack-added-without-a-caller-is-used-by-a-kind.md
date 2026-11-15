# What the core repack added without a caller is used by a kind

**Kind:** structure, decision taken.

**Decided.** The human keeps the mechanisms (2026-11-15). The kinds that call them land after the
next normalization of the ADR record, so their drafts are written once against the restated record:
glpi uses the repack, the kept links and the version probe; icinga2 the repack, `gpgv` and the
version probe; telegraf only `gpgv`. Until then the `#[allow(dead_code)]` markers stand.

**What was ticketed.**

The core repack (#179, 834010bff) added mechanisms that no kind on `main` calls. ADRs decide each
of them, so none is dead by mistake:

- **The kept-link chain** (ADR-0033 clause 57). `KindDefaults::tree_links`
  (`crates/fleet-agent/src/supervisor/ports.rs`, near line 423) is `&[]` for every kind. The empty
  list still runs through `install_target`, `InstallTarget::Tree`, `stage_tree` and
  `archive::extract_tree_tar_gz`'s kept-link checks for every tree package. `KeptLink` values are
  made only in tests.
- **`gpgv` and the repack** (ADR-0036 clauses 27 to 29). `crates/fleet-tools/src/gpgv.rs` carries
  `#[allow(dead_code)]` throughout, and no command calls it. In `crates/fleet-tools/src/fetch.rs`,
  `pack_tree`, `write_members`, `Member` and `collect_members` (near lines 1037 to 1180) carry it
  too, as do `MAX_TREE_MEMBERS` (near line 39), `Resolution`, `MAX_LINK_HOPS`, `resolve_inside` and
  `mode_of` (near lines 1252 to 1376), and so do three fields of `Release` (near lines 385 to 403).
  No kind on `main` repacks, so `--repack` (clause 28) has no agent to number.
- **Most fields of `KindDefaults`** (`ports.rs`, near lines 407 to 453). The Foreign Agent kind
  (`command.rs`) returns `KindDefaults::none()`, and the Collector kind (`collector.rs`) sets only
  `endpoint_port`.
- **`VersionProbe::parse`.** It is `None` in every kind (`collector.rs`, near line 145;
  `command.rs`, near line 105). `fleet_core::version::written_as_semver` says that a kind reports
  its version by it, yet only `fetch.rs` (near line 253) calls it.
- **The wrapped-kinds loop in `scripts/check-rust.sh`** (near lines 62 to 68). It reads the
  features of `crates/fleet-agent/Cargo.toml` other than `default`, and falls back silently to an
  empty list. That `[features]` table holds only `default = []`, and no test covers the loop. A
  test that fails on an empty list would break `main` today.

[`AGENTS.md` §1](../../AGENTS.md#1-principles) adds an abstraction only for a present need. Code
that no path reaches is tested only by its own tests: the `archive_tree` fuzz target broke on it
unseen (#179, bd1b5bbe1). The human chooses one:

- land one kind that uses them, so the code has a caller; the rebased branches `feat/telegraf`,
  `feat/glpi` and `feat/icinga2` carry such kinds; or
- remove the plumbing, which changes the ADRs that decide it through a superseding ADR
  (`propose-adr`).

The choice must be settled before the next normalization of the ADR record.

**Effort.** The decision is S; the removal is M.

**Its acceptance criteria.**

- [ ] Given the human's choice, when the change lands, then no `#[allow(dead_code)]` is left in
  `fetch.rs` and `gpgv.rs`.
- [ ] Given the change, when `KindDefaults` and `VersionProbe` are read, then every field is set by
  a kind on `main`.
- [ ] Given the change, when `check-rust.sh` reads the wrapped kinds, then a test covers the read,
  or the loop is gone.
- [ ] Given a tree package of a kind on `main`, when the Client installs it, then the existing
  tests pass.

**Its reading.**

- Touches: `crates/fleet-agent/src/supervisor/`, `crates/fleet-agent/src/archive.rs`,
  `crates/fleet-tools/src/fetch.rs`, `crates/fleet-tools/src/gpgv.rs`,
  `crates/fleet-core/src/tree.rs`, `crates/fleet-core/src/version.rs`, `scripts/check-rust.sh`
- ADR-0033 clause 57, ADR-0036 clauses 27 to 29, ADR-0018 (the kinds), ADR-0034 clause 6.

**Its mode.**

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero. The choice is about scope, and a removal changes accepted ADRs;
both are the human's.

**From.** Ticket
`docs/plan/revision-2026-11-15/08-what-the-core-repack-added-without-a-caller-is-used-or-taken-out.md`,
moved to the backlog on the human's decision of 2026-11-15.
