# Ticket: check-all says the disk is short before it builds

## What to build

The disk filled twice during check-all, with 207 GB in `target/` and 50 GB of an old session's
target directory in `/tmp`. The build failed with a linker bus error or `ENOSPC`, which read as a
code failure at first and cost a diagnosis each time.

Before `scripts/check-rust.sh` builds, compare the free space of the file system that holds the
target directory (`CARGO_TARGET_DIR`, else `target/`) with a threshold, and fail at once with a
message that names the free space, the directory and how to reclaim it. The threshold, and whether
it fails or only warns, is the human's to set; an environment variable may override it for a smaller
machine.

## Acceptance criteria

- [ ] Given less free space than the threshold, when check-all runs, then the build check fails
  before cargo starts and says how much space is free where.
- [ ] Given enough space, when check-all runs, then nothing changes.
- [ ] Given the check's self-test, when it runs, then a fake free-space reading drives both cases.

## Read first

- `scripts/check-rust.sh`, `scripts/check-all.sh`.
- ADR-0002 on checks as bash scripts.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. The threshold is the human's.
