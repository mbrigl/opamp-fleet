# The structural tests share one source reader

**Kind:** idea, tests.

**Observed.** Five tests read the workspace's Rust sources to decide a structural rule, each with
a reader of its own:

- `crates/fleet-server/tests/conformance_matrix.rs` parses with `syn`, cuts what a `cfg` that
  cannot hold without `test` keeps out, and follows the module tree from `lib.rs` and `main.rs`;
- `crates/fleet-util/tests/who_shares.rs` has the same cut, but reads every file under `src/`;
- `crates/fleet-agent/tests/at_calls.rs` parses with `syn`, reads test code too, and copies
  `who_shares`' `rust_files` and its walk by path;
- `crates/fleet-core/tests/dependency_direction.rs` (near lines 374 to 540) has a hand lexer;
- `crates/fleet-core/tests/pkcs11_written_once.rs` reads the source text without its comments.

They disagree on what is shipped. `who_shares` reads the files of modules declared
`#[cfg(test)] mod x;`, such as `crates/fleet-server/src/fleet/fixtures.rs` and
`crates/fleet-server/src/test_support.rs`, as shipped code; the matrix never reaches them. And
the cut the matrix and `who_shares` share sees only outer attributes: a module file headed by an
inner `#![cfg(test)]`, a tuple-struct field, a parameter, or a macro body under a test-only `cfg`
is read as shipped. None of the crates has such a case today.

**Open.** Sharing a reader across crates takes a crate of its own, which ADR-0008 clause 11 counts
and decides, or a `#[path]` module that each test includes. Which one, and whether the
disagreement is worth either, is the human's to decide.

**From.** The review of `plan/revision-2026-11-18-frontier` at 6d7b2732f (2026-11-18).
