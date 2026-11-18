# Ticket: The who-shares test reads a syntax tree, and reports what each item's users are

## What to build

`crates/fleet-util/tests/who_shares.rs` decides ADR-0008 clause 16 module by module, as the
ADR's Context, its rejected alternatives and its Enforcement have it, with a lexer of its own
(`without_literals`, `without_test_items`, a cfg evaluator `predicate`, line-wise matches in
`modules_named` and the re-export check). Three rounds of fixes followed it in PR #193
(9468ffa88, b709af165, 66957b200): `cfg(not(test))` read as test code, raw byte strings
misread, an alias in a group missed, a loop that did not end, a slice at a byte inside a char,
and asserts that decided nothing.

The test `each_shared_module_sits_in_the_crate_its_users_choose` keeps its name and every case
ADR-0008's Enforcement lists for clause 16:

- a module of `fleet-util` that the Client or the tool does not name;
- a module of `fleet-util` that the Server names;
- a module of `fleet-core` that no Server code names;
- a `pub use` of a `fleet_util::` item, an alias of the crate included;
- a manifest of `fleet-server` that names `fleet-util`.

It reads the sources as a syntax tree (`syn` as a dev-dependency, which is dev-only tooling
under
[`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules)
rule 2) in place of its own lexer. It decides nothing new and moves nothing.

Beside the test, a report lists each public item of `fleet-util` with the crates that name it
in shipped code, and the session puts that list into its hand-off as input to ticket 11. On
`main` the tool names in shipped code only `archive::unix_mode_attributes`,
`nofollow::{read_privileged, Named}`, `password::read_password_file`,
`identity::pkcs12::open` and `tree::{KeptLink, is_kept}`, so most items would fail a per-item
rule. Some could not move at all: `read_privileged` builds on `HeldDir`. The report fails
nothing; whether items move is ticket 11's decision.

## Acceptance criteria

- [ ] Given each case the Enforcement of ADR-0008 lists for clause 16, when a fixture builds it,
  then the test fails naming the module, the re-export or the manifest, as it does today.
- [ ] Given the fixtures of the current test (a `cfg(not(test))` item, a raw byte string, an
  alias in a group), when the new reader reads them, then it decides each as the current test
  does.
- [ ] Given `main`, when the test runs, then it passes.
- [ ] Given `main`, when the report runs, then it lists each public item of `fleet-util` with
  the crates that name it in shipped code, and fails nothing.

## Read first

- Touches: `crates/fleet-util/tests/who_shares.rs`, `crates/fleet-util/Cargo.toml`
- ADR-0008 clause 16, and its Enforcement on `who_shares.rs`.
- `crates/fleet-tools/tests/depends_on_neither_end.rs`, which reads the dependency graph by
  `cargo metadata`.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
