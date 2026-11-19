# The structural tests share one source reader

**Kind:** idea, tests.

**Observed.** Structural tests in three crates each read Rust source and cut out its test code:
- `crates/fleet-util/tests/who_shares.rs` reads a `syn` tree.
- `crates/fleet-server/tests/source/mod.rs` is a copy of that reader, which the Server's
  structural tests share (`conformance_matrix.rs`, `attribute_reads.rs`). Sharing it across
  crates would take a crate of its own (DEV-six-crates-tokio-axum §11).
- `crates/fleet-core/tests/dependency_direction.rs` (`without_test_code`) is a hand lexer that
  matches only the exact text `#[cfg(test)]`.
- `served_literals` in `crates/fleet-server/src/postgres/schema.rs` is a hand lexer too. It matches
  only the exact text `#[cfg(test)]`, and it stops looking for the item at a `;` or a `,`, so a
  test-only function with two parameters counts as served code. Both lexers err toward a false
  alarm, not a miss.

The syn reader needed 4fff44bd8, 3e7142d36 and 52033af18, and d54447ce8 took the lexer's defects
out of the capability matrix.

**Idea.** One reader for all: a test-support crate both crates take as a dev-dependency, which
amends DEV-six-crates-tokio-axum §11, or one file included by `#[path]`; or the copies stay and
`dependency_direction.rs` becomes a copy of the syn reader.

**Not now.** The design revision of 2026-11-19 filed it as a decision, and the human deferred it
the same day: the reader in `schema.rs` goes with the contract pair
(`docs/backlog/the-contract-pair-is-dropped.md`), after which `dependency_direction.rs` is the one
lexer left.

**From.** Ticket 03 of `revision-2026-11-19`.
