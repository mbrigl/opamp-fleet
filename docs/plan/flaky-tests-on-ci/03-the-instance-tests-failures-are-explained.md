# Ticket: The instance tests' single failures are explained

## What to build

Four tests failed once each while stage one was worked, and nobody ran them in a loop since:

- `postgres::lease::tests::instances_that_share_the_database_each_hold_a_lease`
- `postgres::tests::a_database_that_stops_answering_fails_the_call_within_the_bound`
- `postgres::agents::tests::a_label_change_committed_during_a_rekey_moves_with_the_row`
- `a_gateway_keeps_its_agents_on_one_instance_and_moves_those_of_a_stopped_one_once` in
  `tests/instances.rs`, once at 12.95 s; it then passed twenty times in a row

They most likely came from contention on the test database, which the caps on test schemas
address. Each is run in a loop on a loaded host; a test that still fails gets its cause named and
fixed, and the others are recorded as explained in the hand-off.

Beside them, two hazards of the test harness: the caps on test schemas (`Slot` and `HELD` in
`postgres/mod.rs`, `Slot` in `tests/instances.rs`) wait without a deadline, and the first is
thread-local, so a test that opens a schema from a spawned thread while it holds one waits forever
instead of failing; and the regression test of the release order parks a session under the five
second `lock_timeout`, which a heavily loaded host can exceed. A schema opened that way fails
naming the cause, and the parked session waits without the timeout.

## Acceptance criteria

- [ ] Given each of the four tests on a loaded host, when it runs fifty times, then it passes every
  time, or its cause is named and fixed
- [ ] Given a test that opens a second schema from a spawned thread, when it runs, then it fails
  within seconds naming the cap
- [ ] Given the release-order regression test on a loaded host, when it runs thirty times, then it
  passes every time

## Read first

- [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done), which counts a flaky
  test as a defect
- `TestDatabase` and the schema caps in `postgres/mod.rs` and `tests/instances.rs`

## Blocked by

- docs/plan/flaky-tests-on-ci/02-the-cut-listener-test-keeps-its-lease.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
