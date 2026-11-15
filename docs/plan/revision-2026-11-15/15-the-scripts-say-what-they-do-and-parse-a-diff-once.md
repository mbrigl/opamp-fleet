# Ticket: The scripts say what they do, run a self-test once, and parse a diff once

## What to build

Three small findings in the scripts, one session together:

- **Stale comments.** The comment above "Client on its platform" in `scripts/check-all.sh`, and
  `.github/workflows/checks.yml` near lines 30 to 33, say each platform check skips where the
  machine lacks Windows or macOS. `scripts/check-client-platform.sh` now lints the Client for
  Windows on Linux and skips only without cargo-xwin or its target, as its own header says.
- **A self-test run twice.** `scripts/test-check-store.sh` is a stub test, yet
  `.github/workflows/checks.yml` runs it in both the s3-store and the postgres-store job (near
  lines 293 and 338). It runs once, in a job check 9 accepts.
- **A diff parsed twice.** `scripts/sensor-tests-kept.sh` tracks the added lines of a unified diff
  (`+++`, `@@`, `+`) twice (near lines 109 and 225), once for the markers and once for the timed
  waits. One helper emits the added lines, and each section filters them.

## Acceptance criteria

- [ ] Given the change, when the comments are read, then they say what `check-client-platform.sh`
  does on Linux.
- [ ] Given a CI run, when its jobs are listed, then `test-check-store.sh` runs in one of them.
- [ ] Given `scripts/test-sensor-tests-kept.sh`, when the change lands, then it passes unchanged.

## Read first

- Touches: `scripts/check-all.sh`, `.github/workflows/checks.yml`, `scripts/sensor-tests-kept.sh`,
  `scripts/test-sensor-tests-kept.sh`
- `check-docs.sh` check 9 (the jobs and the self-tests).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
