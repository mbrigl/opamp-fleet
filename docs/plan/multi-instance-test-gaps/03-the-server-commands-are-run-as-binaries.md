# Ticket: The Server's commands are run as binaries

## What to build

The Server's commands for several instances are tested in the library, not as the operator runs
them:

- `server blobs-to-s3` has no test of its arguments through the binary
- `server audit-verify` with no schema named, and its exit code (`i32::from(failed)` in
  `main.rs`), are not run
- the daily prune's schedule in `main.rs`, its 24-hour cutoff across a change of daylight saving
  time and the exact `retain_days` boundary have no test; the prune tests drive one round through
  `Writing::prune`, so only the rounds test drives the loop
- the image is never run read-only: the stateless instance's checks cover the working directory
  and the directory of the files it reads, not `$TMPDIR` or `$HOME`, and the release workflow
  runs the image only for `--version` and a shell probe

## Acceptance criteria

- [ ] Given the binary, when `server blobs-to-s3` runs with missing or wrong arguments, then it
  fails naming them, and with right ones it copies and checks
- [ ] Given a record that does not hold, when `server audit-verify` runs, then it exits non-zero,
  and with no schema named it reads the default
- [ ] Given an entry exactly `retain_days` old, when the prune runs, then the boundary the manual
  states holds, across a change of daylight saving time too
- [ ] Given the image with `--read-only` and no volume, when it serves on PostgreSQL and S3, then
  it starts, admits an Agent and writes nothing

## Read first

- ADR-0044 clauses 1 and 12; ADR-0042 clause 5
- Golden path: `tests/stateless_instance.rs`; the service smoke test for running an image
- `main.rs`, the commands and the prune's schedule; `postgres/audit_prune.rs`

## Blocked by

- docs/plan/multi-instance-test-gaps/02-the-stores-races-are-run.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
