# Ticket: The Server's waits wait for their event

## What to build

Some tests wait a fixed time for work on another thread or process, or hold that work to a real-time
budget of milliseconds, and then assert that it happened. A loaded host makes them fail. Each wait
becomes a wait for the event it stands for, under a deadline of seconds; each budget becomes a
count, a clock the test controls, or a bound only a defect reaches. This ticket covers the
`fleet-server` crate's tests.

Sleeps that stand for an event:

- `crates/fleet-server/tests/mutual_tls.rs` `an_audit_that_cannot_write_refuses_admission` and
  `an_audit_that_cannot_write_issues_no_certificate`: 150 ms for the audit write to fail; poll
  `record().is_err()` as the same file already does elsewhere
- `crates/fleet-server/src/postgres/enrolment.rs`
  `a_take_does_not_land_in_a_window_closed_meanwhile`: 300 ms for the taker to block on the row
  lock. It does not fail, but on a slow host it no longer tests its race; `postgres/deployments.rs`
  waits on `pg_stat_activity` instead.

Budgets:

- `crates/fleet-server/src/credentials.rs` `a_basic_verification_is_remembered_and_bounded` (`< 5
  ms`) and `an_unknown_user_costs_the_same_verification` (a ratio of two timings): count the hash
  calls instead
- `crates/fleet-server/src/revocation.rs` `a_check_does_not_wait_for_a_reloads_read`: 1 s for a
  thread's answer
- `crates/fleet-server/tests/instances.rs`
  `a_revocation_on_one_instance_ends_the_session_on_the_other`: 1 s for the session on the other
  instance to close

These tests wait a time and assert that something does **not** happen. They stay as they are, and
are listed only so that nobody takes them for waits for an event:

- `crates/fleet-server/tests/connection_setup.rs` `an_established_session_outlives_the_header_bound`
- `crates/fleet-server/tests/ws_transport.rs` `a_config_change_is_pushed_without_the_agent_asking`
  and `selectors_target_a_subset_and_compose_named_entries`
- `crates/fleet-server/tests/instances.rs` the kept session of
  `a_revocation_on_one_instance_ends_the_session_on_the_other`
- `crates/fleet-server/tests/mutual_tls.rs`
  `a_revocation_closes_the_session_it_concerns_and_no_other`
- `crates/fleet-server/src/instance.rs` `the_logged_id_follows_the_lease`

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
- The PostgreSQL tests run only with the stores of the
  [README's Build, Test & Run](../../../README.md#build-test--run)
- Seam: the tests themselves

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
