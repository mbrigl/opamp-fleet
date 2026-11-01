# Ticket: Tests wait for an event, not a fixed time

## What to build

Several tests wait a fixed time for work on another thread or process and then assert, and they
fail when the host is loaded, as during a build in a second worktree. Each such wait becomes a
wait for the event it stands for, bounded by a deadline long enough that only a defect reaches
it. The cases seen so far:

- `audit_log`'s `settle()`, a 50 ms sleep before reading what the writer thread wrote; five tests
  failed on it at a load of 33 to 47 on 24 cores;
- the 35 ms budget of a test in the Agent's packages;
- the polling in the self-update end-to-end test;
- `a_config_change_reaches_both_supervised_agents_over_one_connection`, whose unchanged
  supervisor's pid changed under load;
- the `test-check-all` case that sends TERM.

## Acceptance criteria

- [ ] Given each case above, when it waits, then it waits for its event under a deadline of
  seconds, not a sleep of milliseconds
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
