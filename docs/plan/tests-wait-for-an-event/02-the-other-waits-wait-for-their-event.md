# Ticket: The other waits wait for their event

## What to build

More tests wait a fixed time for work on another thread or process, or hold that work to a
real-time budget of milliseconds, and then assert that it happened. A loaded host makes them fail.
Each wait becomes a wait for the event it stands for, under a deadline of seconds. Each budget
becomes a count, an injected or paused clock, or a bound only a defect reaches.

Sleeps that stand for an event:

- `crates/fleet-agent/tests/shutdown.rs:36` `sigterm_shuts_the_client_down_cleanly`: 800 ms for
  the signal handlers to be set
- `crates/fleet-agent/src/supervisor/endpoint.rs:478` `shutdown_stops_the_endpoint`: 100 ms for
  the endpoint task to end
- `crates/opamp/tests/server_endpoint.rs:288` `a_websocket_carries_replies_and_pushes`: 100 ms for
  the close callback
- `crates/opamp/tests/server_listen.rs:256` and `:278`
  `connections_past_the_cap_are_refused_while_established_ones_keep_working`: 200 ms for the
  first two connections to be accepted, and 200 ms for a slot to be freed
- `crates/opamp/tests/server_listen.rs:798`
  `a_drain_cuts_a_stuck_websocket_session_at_its_deadline`: 200 ms for the session to enter its
  handler
- `crates/fleet-server/tests/mutual_tls.rs:2598` `an_audit_that_cannot_write_refuses_admission`
  and `:2668` `an_audit_that_cannot_write_issues_no_certificate`: 150 ms for the audit write to
  fail
- `crates/fleet-agent/src/reconfigure.rs:2051` `a_start_removes_the_tombstones_again_beside_it`:
  50 ms for the reap task to ask the service
- `crates/fleet-agent/src/supervisor/channel/windows.rs:930` and `:934`
  `closing_the_connection_ends_the_whole_job`: 3 s for the program to run, and 3 s for the job to
  be torn down
- `crates/fleet-agent/src/gateway/mod.rs:1420`
  `an_enrolling_peer_relays_one_enrolment_request_and_binds_nothing` and `:1810`
  `an_unreadable_store_refuses_every_downstream_agent`: a 200 ms `select!` arm for the exchange to
  reach upstream
- `crates/fleet-agent/tests/supervisor_process.rs:1650`
  `a_failed_package_starts_its_predecessor_exactly_once`: a 2.5 s window that must contain the
  restart
- `crates/fleet-server/src/postgres/enrolment.rs:945`
  `a_take_does_not_land_in_a_window_closed_meanwhile` and
  `crates/fleet-agent/src/supervisor/channel.rs:4596`
  `output_the_client_has_not_read_holds_up_no_request`: 300 ms and 500 ms. Neither fails, but on
  a slow host neither tests its race any more.

Tight real-time budgets:

- `crates/fleet-agent/tests/supervisor_process.rs`: the apply-grace tests whose crasher must exit
  within a grace of 100 to 500 ms. They are at `:1044`, `:1197`, `:1483`, `:1508`, `:1538`,
  `:1595`, `:1641`, `:1839` (grace set at `:1759`), `:2314` and `:2787`.
- `crates/opamp/tests/server_listen.rs:312`
  `new_connections_past_the_rate_are_answered_503_with_a_spread_retry_after`: all 30 connects
  within one refill of the bucket
- `crates/opamp/tests/server_listen.rs:555` `a_slow_body_above_the_floor_is_taken_whole`: 100 ms
  per chunk against a 300 ms window
- `crates/opamp/tests/server_listen.rs:813` `a_spread_drain_closes_the_sessions_over_its_window`:
  the times the closes come at
- `crates/fleet-server/src/credentials.rs:394` `a_basic_verification_is_remembered_and_bounded`
  (`< 5 ms`) and `:376` `an_unknown_user_costs_the_same_verification` (a ratio of two timings)
- `crates/fleet-agent/src/telemetry.rs:1416`
  `a_span_this_client_writes_reaches_the_offered_destination` (200 ms of silence ends a request)
  and `:1261` `replacing_a_silent_destination_does_not_wait_for_its_flush` (`< 1 s`)
- `crates/fleet-server/src/revocation.rs:3279` `a_check_does_not_wait_for_a_reloads_read`: 1 s
- `crates/fleet-server/tests/instances.rs:462`
  `a_revocation_on_one_instance_ends_the_session_on_the_other`: 1 s
- `crates/fleet-agent/src/supervisor/process.rs:2738` (500 ms), `:2519`, `:2358`, `:2970` and
  `:3018` (1 to 2 s): budgets for something to happen "at once"
- `crates/fleet-agent/src/supervisor/channel.rs:3600`, `:4041`, `:4050` and `:4069`: 1 s each
- `scripts/test-check-all.sh` `at_once`, "the checks run side by side": stubs of 0.1 s, so three
  checks must start within 100 ms of each other

Seconds-scale budgets to check while at it: `crates/fleet-agent/tests/gateway_revocation_e2e.rs`
`:399`, `:418` and `:501`; `crates/fleet-agent/tests/certificate_renewal_e2e.rs:132`;
`crates/fleet-server/src/postgres/mod.rs:1002` and `:1083`; `crates/fleet-server/src/s3.rs:1103`;
`crates/fleet-agent/tests/gateway_tls.rs:588`.

These tests wait a time and assert that something does **not** happen. They stay as they are, and
are listed only so that nobody mistakes them for waits for an event:

- `crates/fleet-agent/tests/shutdown.rs:89`, `logging.rs:100`, `e2e.rs:590`, `:677` and `:782`,
  `gateway_e2e.rs:347`, `service_smoke.rs:309`, `:1113` and `:1952`
- `crates/fleet-agent/tests/supervisor_process.rs`: `the_apply_finds_it_away`, `stays_live`,
  `:2481` and `:2536`
- `crates/fleet-server/tests/connection_setup.rs:81`, `ws_transport.rs:104` and `:215`,
  `instances.rs:467`, `mutual_tls.rs:1524`
- `crates/opamp/tests/server_listen.rs:638` and `:709`
- `crates/fleet-agent/src/supervisor/channel.rs:3455` and `:3584`, `process.rs:2513`, `:2732` and
  `:3015`
- `crates/fleet-agent/src/connection.rs:777` and `:838`, `reconfigure.rs:2064`
- `crates/fleet-server/src/instance.rs:722` and `:731`

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
