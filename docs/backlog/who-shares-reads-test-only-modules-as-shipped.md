# `who_shares` reads test-only modules as shipped code

**Kind:** doubt, tests.

**Observed.** `crates/fleet-util/tests/who_shares.rs` reads every file under a crate's `src/` and
leaves out only what an outer `cfg` attribute that cannot hold without `test` keeps out. So it
reads the files of modules declared `#[cfg(test)] mod x;` as shipped code:
`crates/fleet-server/src/fleet/fixtures.rs` and `crates/fleet-server/src/test_support.rs` among
them. `crates/fleet-server/tests/conformance_matrix.rs` follows the module tree from `lib.rs` and
`main.rs` and never reaches them. A test-only module of the Server that names a shared item
therefore counts the Server as one of its users, which decides the item's crate (ADR-0008
clause 16).

**Idea.** `who_shares` follows the module tree from each crate's roots, as the matrix does, so a
file that only a `#[cfg(test)] mod` declares is not read as shipped. A fixture holds a crate whose
test-only module names a shared item.

**Not taken.** One source reader for all five structural tests. The other gaps the review found,
an inner `#![cfg(test)]` and a test-only `cfg` on a field, a parameter or a macro body, occur in no
crate today. Sharing a reader across crates takes a crate of its own (ADR-0008 clause 11) or a
`#[path]` module. The human judged neither worth it on 2026-11-19.

**From.** The review of `plan/revision-2026-11-18-frontier` at 6d7b2732f (2026-11-18), cut to its
concrete part by the human (2026-11-19).
