# Ticket: The structural tests run in a second, before the build

## What to build

A new module, opamp's `random`, was added on a ticket branch without a role in
`dependency_direction.rs` (commit bb820bf8). The test caught it, but only inside check-all's "Build,
test and lint", after the whole workspace had built and every check had finished, because check-all
prints its results once all have run. `dependency_direction.rs` and
`privileged_steps_follow_no_link.rs` read source text and use only `std`: each compiles with `rustc
--test` alone, `CARGO_MANIFEST_DIR` set to its crate, and runs in under a second (tried during this
revision).

Add a check script that compiles and runs the source-reading tests this way, list it in check-all
and in `checks.yml` as check 9 of `check-docs.sh` requires, and let the hooks of
docs/plan/revision-2026-10-31/17-a-rebase-reports-the-fast-checks.md run it. The tests stay where
they are, so `cargo test` keeps running them too.

## Acceptance criteria

- [ ] Given a module file without a role, when the new check runs, then it fails within seconds and
  names the file, without a cargo build.
- [ ] Given a source-reading test that gains a dependency beyond `std`, when the check runs, then it
  fails and says why, so the fast path cannot rot silently.
- [ ] Given check 9 of `check-docs.sh`, when the script is added, then check-all and `checks.yml`
  list it.

## Read first

- ADR-0002, ADR-0009 (Enforcement).
- `scripts/check-all.sh`, `scripts/check-rust.sh`, check 9 in the header of `scripts/check-docs.sh`.
- `crates/fleet-core/tests/dependency_direction.rs`,
  `crates/fleet-agent/tests/privileged_steps_follow_no_link.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
