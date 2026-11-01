# Ticket: The opamp crate's waits wait for their event

## What to build

Some tests wait a fixed time for work on another thread or process, or hold that work to a real-time
budget of milliseconds, and then assert that it happened. A loaded host makes them fail. Each wait
becomes a wait for the event it stands for, under a deadline of seconds; each budget becomes a
count, a clock the test controls, or a bound only a defect reaches. This ticket covers the `opamp`
crate's tests.

Sleeps that stand for an event:

- `crates/opamp/tests/server_endpoint.rs` `a_websocket_carries_replies_and_pushes`: 100 ms for the
  close callback
- `crates/opamp/tests/server_listen.rs`
  `connections_past_the_cap_are_refused_while_established_ones_keep_working`: 200 ms for the first
  two connections to be accepted, and 200 ms for a slot to be freed
- `crates/opamp/tests/server_listen.rs` `a_drain_cuts_a_stuck_websocket_session_at_its_deadline`:
  200 ms for the session to enter its handler

Budgets:

- `crates/opamp/tests/server_listen.rs`
  `new_connections_past_the_rate_are_answered_503_with_a_spread_retry_after`: all 30 connects within
  one refill of the bucket
- `crates/opamp/tests/server_listen.rs` `a_slow_body_above_the_floor_is_taken_whole`: 100 ms per
  chunk against a 300 ms window
- `crates/opamp/tests/server_listen.rs` `a_spread_drain_closes_the_sessions_over_its_window`: the
  times the closes come at

These tests wait a time and assert that something does **not** happen. They stay as they are, and
are listed only so that nobody takes them for waits for an event:

- `crates/opamp/tests/server_listen.rs` `an_idle_websocket_stays_open` and
  `a_websocket_that_only_pings_stays_open`

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
