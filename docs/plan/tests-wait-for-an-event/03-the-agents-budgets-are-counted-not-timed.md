# Ticket: The Agent's budgets are counted, not timed

## What to build

Some tests wait a fixed time for work on another thread or process, or hold that work to a real-time
budget of milliseconds, and then assert that it happened. A loaded host makes them fail. Each wait
becomes a wait for the event it stands for, under a deadline of seconds; each budget becomes a
count, a clock the test controls, or a bound only a defect reaches. This ticket covers the real-time
budgets in `fleet-agent`.

An apply grace the crasher must die within; a long grace (30 s) costs nothing where the process is
meant to fail. All in `crates/fleet-agent/tests/supervisor_process.rs`:

- `a_process_that_dies_on_the_reload_signal_is_restarted_instead` (300 ms)
- `a_process_exiting_within_the_grace_fails_the_apply_and_stays_supervised` (500 ms)
- `a_package_that_will_not_stay_up_is_rolled_back_and_fails` (500 ms)
- `a_first_install_that_will_not_start_is_kept_not_discarded` (200 ms)
- `a_program_that_keeps_crashing_is_held_not_looped` (100 ms)
- `a_failed_package_counts_towards_the_hold_of_a_predecessor_that_fails_too` (300 ms)
- `a_failed_package_starts_its_predecessor_exactly_once` (300 ms)
- `a_tree_that_will_not_stay_up_is_rolled_back_whole` (200 ms, set in `tree_harness`)
- `a_package_waits_staged_while_the_service_is_away_and_is_swapped_in_once_it_is_back` (500 ms)
- `a_failed_package_does_not_fail_the_configuration_its_predecessor_runs` (500 ms)

Other budgets:

- `crates/fleet-agent/src/telemetry.rs` `a_span_this_client_writes_reaches_the_offered_destination`:
  200 ms of silence ends a request; read `Content-Length` bytes instead
- `crates/fleet-agent/src/telemetry.rs`
  `replacing_a_silent_destination_does_not_wait_for_its_flush`: `< 1 s`
- `crates/fleet-agent/src/supervisor/process.rs` `a_shutdown_ends_a_reload_waiting_on_its_signal`
  (500 ms), `a_shutdown_ends_the_preflight_a_parked_package_waited_for` (1 s),
  `a_retry_due_at_once_does_not_say_the_service_is_away` (1 s),
  `a_program_that_went_with_the_second_service_is_no_failure_of_its_own` (2 s) and
  `a_shutdown_ends_a_start_waiting_for_the_second_service` (2 s)
- `crates/fleet-agent/src/supervisor/channel.rs` `a_client_left_behind_by_a_self_update_is_told_so`
  (1 s), `a_refused_peer_is_dropped_at_once` (1 s each, for 20 peers) and
  `a_handshake_larger_than_a_handshake_is_refused_at_once` (1 s)

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
