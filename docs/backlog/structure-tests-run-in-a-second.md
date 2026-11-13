# The source-reading structure tests run in a second, outside the workspace build

**Kind:** idea, check.

**Observed.** `crates/fleet-core/tests/dependency_direction.rs` and
`crates/fleet-agent/tests/privileged_steps_follow_no_link.rs` read source text and use only
`std`, yet they run only inside check-all's build, test and lint step, after the whole workspace
has built. On the old branch a new module without a role in `dependency_direction.rs` was caught
only there. Each compiles with `rustc --test` alone, `CARGO_MANIFEST_DIR` set to its crate, and
runs in under a second. `main` has no check that runs them that way.

**What the old branch had.** Revision ticket 16 of `feat/revision-2026-10-31`, open, not worked: a
check script that compiles and runs the source-reading tests this way, listed in check-all and in
`checks.yml` as check 9 of `scripts/check-docs.sh` requires. The tests stay where they are, so
`cargo test` keeps running them.
