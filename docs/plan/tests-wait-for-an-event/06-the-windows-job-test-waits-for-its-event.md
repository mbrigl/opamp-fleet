# Ticket: The Windows job test waits for its event

## What to build

Some tests wait a fixed time for work on another thread or process, or hold that work to a real-time
budget of milliseconds, and then assert that it happened. A loaded host makes them fail. Each wait
becomes a wait for the event it stands for, under a deadline of seconds; each budget becomes a
count, a clock the test controls, or a bound only a defect reaches. This ticket covers one
Windows-only test.

Sleeps that stand for an event:

- `crates/fleet-agent/src/supervisor/channel/windows.rs`
  `closing_the_connection_ends_the_whole_job`: 3 s for the program left behind to run, then 3 s for
  the job to be torn down before the marker's size is read. Its last wait, the size staying the
  same, asserts an absence and stays.

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
- The code compiles only for Windows: typecheck it with `cargo xwin clippy` as the
  [README's Build, Test & Run](../../../README.md#build-test--run) shows, and run it in CI,
  since it cannot run here
- Seam: the tests themselves

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
