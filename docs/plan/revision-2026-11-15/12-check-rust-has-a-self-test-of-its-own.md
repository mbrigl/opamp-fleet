# Ticket: `check-rust.sh` has a self-test of its own

## What to build

`scripts/test-check-all.sh` now holds a case for `scripts/check-rust.sh` (the block beginning
"The workspace's test run in check-rust.sh sees no variable"), with `check-rust.sh` among its
inputs. The runner's self-test thereby tests another script, and `--changed` re-runs the whole
slow runner self-test for an edit to `check-rust.sh`. The `sed` in `check-rust.sh` that reads the
wrapped kinds from `crates/fleet-agent/Cargo.toml`'s `[features]` table has no test and falls
back silently to an empty list (`|| true`).

The case moves into a new `scripts/test-check-rust.sh`, with a case for the feature parse that
fails when it finds no kind, and joins `check-all.sh` and the CI job check 9 maps it to.

## Acceptance criteria

- [ ] Given a fixture `Cargo.toml` with two wrapped kinds, when `test-check-rust.sh` runs, then it
  asserts that `check-rust.sh` finds both; given none, then `check-rust.sh` says so rather than
  running with an empty list.
- [ ] Given the change, when `test-check-all.sh` is read, then it tests only the runner.
- [ ] Given `check-docs.sh` check 9, when the change lands, then it passes.

## Read first

- Touches: `scripts/check-rust.sh`, `scripts/test-check-all.sh`, `scripts/test-check-rust.sh`,
  `scripts/check-all.sh`, `.github/workflows/checks.yml`
- CONVENTIONS: "A self-test asserts what it builds".

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
