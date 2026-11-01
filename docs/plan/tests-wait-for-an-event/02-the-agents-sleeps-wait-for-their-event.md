# Ticket: The Agent's sleeps wait for their event

## What to build

Some tests wait a fixed time for work on another thread or process, or hold that work to a real-time
budget of milliseconds, and then assert that it happened. A loaded host makes them fail. Each wait
becomes a wait for the event it stands for, under a deadline of seconds; each budget becomes a
count, a clock the test controls, or a bound only a defect reaches. This ticket covers the sleeps in
`fleet-agent` that stand for an event, Windows-only code aside.

Sleeps that stand for an event:

- `crates/fleet-agent/tests/shutdown.rs` `sigterm_shuts_the_client_down_cleanly`: 800 ms for the
  signal handlers to be set
- `crates/fleet-agent/src/supervisor/endpoint.rs` `shutdown_stops_the_endpoint`: 100 ms for the
  endpoint task to end
- `crates/fleet-agent/src/reconfigure.rs` `a_start_removes_the_tombstones_again_beside_it`: 50 ms
  for the reap task to ask the service
- `crates/fleet-agent/src/gateway/mod.rs`
  `an_enrolling_peer_relays_one_enrolment_request_and_binds_nothing` and
  `an_unreadable_store_refuses_every_downstream_agent`: a 200 ms `select!` arm for the exchange to
  reach upstream
- `crates/fleet-agent/tests/supervisor_process.rs`
  `a_failed_package_starts_its_predecessor_exactly_once`: a 2.5 s window that must contain the
  restart
- `crates/fleet-agent/src/supervisor/channel.rs`
  `output_the_client_has_not_read_holds_up_no_request`: 500 ms for the queues to fill. It does not
  fail, but on a slow host it no longer tests its race.

These tests wait a time and assert that something does **not** happen. They stay as they are, and
are listed only so that nobody takes them for waits for an event:

- `crates/fleet-agent/tests/shutdown.rs` `sighup_leaves_the_agents_service_running`
- `crates/fleet-agent/tests/logging.rs` `a_foreground_run_writes_no_log_file`
- `crates/fleet-agent/tests/e2e.rs`
  `a_refused_supervisor_set_leaves_the_running_supervisors_untouched`,
  `a_server_offers_no_configuration_to_a_listed_supervisor` and
  `a_server_offers_no_supervisor_set_to_a_host_that_keeps_it`
- `crates/fleet-agent/tests/gateway_e2e.rs`
  `a_downstream_connection_carries_no_more_than_its_agent_cap`
- `crates/fleet-agent/tests/service_smoke.rs`
  `the_installed_service_starts_comes_back_from_a_crash_and_stays_down_after_a_stop`,
  `a_managed_process_in_the_second_services_place_is_refused` and
  `on_windows_a_pipe_the_scm_names_no_process_for_is_refused`
- `crates/fleet-agent/tests/supervisor_process.rs` the helpers `the_apply_finds_it_away` and
  `stays_live`, `a_retained_predecessor_outlasts_the_retries_while_a_package_waits` and
  `a_retry_whose_program_does_not_start_fails_the_configuration_and_respawns_nothing`
- `crates/fleet-agent/src/supervisor/channel.rs`
  `a_peer_of_another_account_ends_no_superseded_service` and the first half of
  `a_client_left_behind_by_a_self_update_is_told_so`
- `crates/fleet-agent/src/supervisor/process.rs` the pending checks of
  `a_shutdown_ends_the_preflight_a_parked_package_waited_for`,
  `a_shutdown_ends_a_reload_waiting_on_its_signal` and
  `a_shutdown_ends_a_start_waiting_for_the_second_service`
- `crates/fleet-agent/src/connection.rs` `verify_refuses_a_move_to_plaintext_on_the_loopback` and
  `verify_refuses_a_plaintext_endpoint_on_a_host_name`
- `crates/fleet-agent/src/reconfigure.rs` the "asked once" check of
  `a_start_removes_the_tombstones_again_beside_it`

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
