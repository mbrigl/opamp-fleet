# Ticket: The side-by-side case waits for its checks

## What to build

Some tests wait a fixed time for work on another thread or process, or hold that work to a real-time
budget of milliseconds, and then assert that it happened. A loaded host makes them fail. Each wait
becomes a wait for the event it stands for, under a deadline of seconds; each budget becomes a
count, a clock the test controls, or a bound only a defect reaches. This ticket covers one case of
`scripts/test-check-all.sh`.

A budget:

- `scripts/test-check-all.sh` `at_once`, the case "the checks run side by side": each stub runs 0.1
  s, so three checks must start within 100 ms of each other. Stubs that wait, under a bound of
  seconds, for three `running/` entries or for a go-file would decide it whatever the load, as the
  TERM case does.

## Acceptance criteria

- [ ] Given each case above, when it waits, then it waits for its event under a deadline of
  seconds, or its budget is a count or a clock the test controls, not milliseconds of real time
- [ ] Given the host loaded by a second build, when `scripts/check-all.sh` runs three times, then
  none of these tests fails
- [ ] Given the tests changed, when one of them is run with its mechanism broken, then it still
  fails

## Read first

- [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done), which counts a flaky
  test as a defect
- `docs/CONVENTIONS.md` on tests
- Seam: the tests themselves

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
