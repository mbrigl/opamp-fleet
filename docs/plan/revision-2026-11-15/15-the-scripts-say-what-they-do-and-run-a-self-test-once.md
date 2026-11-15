# Ticket: The scripts say what they do and run a self-test once

## What to build

Two small findings in the scripts, one session together:

- **Stale comments.** The comment above "Client on its platform" in `scripts/check-all.sh` (near
  lines 265 to 268) and `.github/workflows/checks.yml` (near lines 31 to 33) say that each platform
  check skips where the machine lacks Windows or macOS. `scripts/check-client-platform.sh` now
  lints the Client for Windows on Linux and skips only without cargo-xwin or its target, as its own
  header says.
- **A self-test run twice.** `scripts/test-check-store.sh` is a stub test, yet
  `.github/workflows/checks.yml` runs it in both the s3-store and the postgres-store job (near
  lines 294 and 339). It runs once, in a job check 9 accepts.

**Effort.** S.

## Acceptance criteria

- [ ] Given the change, when the comments are read, then they say what `check-client-platform.sh`
  does on Linux.
- [ ] Given a CI run, when its jobs are listed, then `test-check-store.sh` runs in one of them.
- [ ] Given `check-docs.sh` check 9, when the change lands, then it passes.

## Read first

- Touches: `scripts/check-all.sh`, `.github/workflows/checks.yml`
- `check-docs.sh` check 9 (the jobs and the self-tests).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
