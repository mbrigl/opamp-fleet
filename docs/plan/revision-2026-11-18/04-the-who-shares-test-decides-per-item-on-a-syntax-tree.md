# Ticket: The who-shares test decides per item, on a syntax tree

## What to build

ADR-0008 clause 16 says who shares an *item* chooses its crate.
`crates/fleet-util/tests/who_shares.rs` decides it per *module*, with a lexer of its own
(`without_literals`, `without_test_items`, a cfg evaluator `predicate`, line-wise matches in
`modules_of` and the re-export check). Three rounds of fixes followed it in PR #193
(9468ffa88, b709af165, 66957b200): `cfg(not(test))` read as test code, raw byte strings
misread, an alias in a group missed, a loop that did not end, a slice at a byte inside a char,
and asserts that decided nothing. And it could not see `lstat_at` and `rmdir` in
`fleet_util::nofollow::at`, used by the Client alone; PR #197 found and moved them by hand.

The test reads the sources as a syntax tree (`syn` as a dev-dependency, which is dev-only
tooling under
[`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules)
rule 2) and decides per public item of `fleet-util` which of the Client and the operator tool
names it in shipped code. The half that says the Server names no
`fleet_util` path is left to the manifest check, since the compiler refuses a path to a crate
the Server does not depend on.

## Acceptance criteria

- [ ] Given a public item of `fleet-util` that only the Client names in shipped code, when the
  test runs, then it fails and names the item.
- [ ] Given the fixtures of the current test (a `cfg(not(test))` item, a raw byte string, an
  alias in a group), when the new reader reads them, then it decides each as the current test
  does.
- [ ] Given `main`, when the test runs, then it passes, or it names the items to move, which
  the session moves as clause 16 says.

## Read first

- Touches: `crates/fleet-util/tests/who_shares.rs`, `crates/fleet-util/Cargo.toml`
- ADR-0008 clause 16, its Enforcement.
- `crates/fleet-tools/tests/depends_on_neither_end.rs`, which reads the dependency graph by
  `cargo metadata`.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
