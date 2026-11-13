# Ticket: Every script is committed executable, and a check says so

## What to build

Six scripts under `scripts/` are committed with mode 100644: `sensor-binding-adrs.sh`,
`test-sensor-binding-adrs.sh`, `check-build-hardening.sh`, `test-check-build-hardening.sh`,
`test-normalize-remap.sh` and `test-sensor-plan-frontier.sh`. `check-all.sh` and CI run every
script through `bash`, so neither notices.
[`AGENTS.md` §2](../../../AGENTS.md#2-start-here) and the skills tell a session to run
`scripts/sensor-binding-adrs.sh --sections`, which then fails with "Permission denied"; this
revision hit it. They came with the take-up of NUC v0.8.1 (2dd201a26) and earlier take-ups.

Set the six executable, and add to `scripts/check-shell.sh` a check that every `scripts/*.sh` is
committed with mode 100755, with a case in its self-test. Tell the template's maintainer too, since
the modes came from it.

## Acceptance criteria

- [ ] Given a script under `scripts/` committed with mode 100644, when `scripts/check-shell.sh`
  runs, then it fails, naming the script.
- [ ] Given the tree after the change, when `scripts/check-all.sh` runs, then the check passes.

## Read first

- Touches: `scripts/check-shell.sh`, `scripts/test-check-shell.sh`, the six scripts named above
- ADR-0005 (what the template carries), ADR-0002.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
