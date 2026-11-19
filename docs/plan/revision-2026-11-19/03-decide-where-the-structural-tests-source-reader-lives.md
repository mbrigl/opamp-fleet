# Ticket: Decide where the structural tests' source reader lives

## What to build

A decision, not code. Four structural tests each read Rust source and cut out its test code:
- `crates/fleet-util/tests/who_shares.rs` (near lines 92 to 338) reads a `syn` tree.
- `crates/fleet-server/tests/conformance_matrix.rs` (near lines 127 to 373) is a copy of that
  reader, as its header says. Sharing it across crates would take a crate of its own
  (DEV-six-crates-tokio-axum §11).
- `crates/fleet-core/tests/dependency_direction.rs` (`without_test_code`, near line 485) is a hand
  lexer. It matches only the exact text `#[cfg(test)]`.
- `served_literals` in `crates/fleet-server/src/postgres/schema.rs` (near lines 1090 to 1180, added
  by 718455e4f) is a hand lexer too. It matches only the exact text `#[cfg(test)]`. It also stops
  looking for the item at a `;` or a `,`, so a test-only function with two parameters counts as
  served code.

The two lexers carry one or both of the defects that d54447ce8 took out of the capability matrix
the day before.
The syn reader needed 4fff44bd8 (a cut that missed attributes on expression statements),
3e7142d36 (a path with `\` on Windows) and 52033af18 (the two copies disagreed on what is
shipped).

The human chooses one of three:
- one test-support crate both crates take as a dev-dependency, which supersedes or amends
  DEV-six-crates-tokio-axum §11;
- one file included by `#[path]` into each test;
- the copies stay, and the two lexers become copies of the syn reader.

The reader in `schema.rs` goes with the contract pair
(`docs/backlog/the-contract-pair-is-dropped.md`); where release X+1 comes first, only
`dependency_direction.rs` is left to move. Then a ticket for the chosen way is written, and
`schema.rs` and `dependency_direction.rs` take the reader. Where the choice is an ADR,
propose-adr writes it.

## Acceptance criteria

- [ ] Given this ticket, when the human has chosen, then the choice and its why are recorded,
  as an ADR where it amends DEV-six-crates-tokio-axum, and one ticket for the move replaces this
  file.

## Read first

- Touches: `crates/fleet-util/tests/who_shares.rs`,
  `crates/fleet-server/tests/conformance_matrix.rs`,
  `crates/fleet-core/tests/dependency_direction.rs`, `crates/fleet-server/src/postgres/schema.rs`
- DEV-six-crates-tokio-axum §11; DEV-ports-and-adapters (its Enforcement names
  `dependency_direction.rs`); SRV-state-stores-contract-pair §22 (its Enforcement names the
  check in `schema.rs`).

## Blocked by

- None (can start immediately)

## Mode

with the human: the choice may change an ADR (NUC-decisions-are-proposed-then-accepted).
