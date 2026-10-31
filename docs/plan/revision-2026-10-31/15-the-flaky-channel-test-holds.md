# Ticket: The channel test that checks each connection anew holds on every run

## What to build

`supervisor::channel::tests::a_peer_that_is_not_the_second_service_is_away_and_tried_anew` failed a
pre-push run on 2026-10-30 with `left: 4, right: 3` and passed 20 times in isolation. A flaky test
is a defect ([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)). The likely
race: the test runs on a paused clock, and tokio advances a paused clock to the next timer while
socket I/O is still pending. On the third connection the Client can start its 5 s handshake timeout
before the service task has accepted; the clock jumps, the timeout fires, the open fails with "no
handshake in time", and the loop opens a fourth connection, which asks the service manager a fourth
time. `a_start_in_an_outage_that_was_waited_out_tries_once` has the same exposure.

Reproduce it first, under load or with a loop of runs, then remove the race: run these tests on real
time with a short reconnect, or make the handshake timeout one the tests set, so the assertion keeps
its meaning (the impostor is refused twice, and the first matching answer connects).

## Acceptance criteria

- [ ] Given the test, when it is run 500 times under parallel load, then it passes every time, and
  the run that reproduced the failure before the fix is named in the hand-off.
- [ ] Given the assertion, when the fix lands, then it still decides that each connection is checked
  anew and is not weakened to a range.
- [ ] Given the other paused-clock tests of the channel, when they are reviewed for the same race,
  then each is fixed or shown not to have it.

## Read first

- [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done) on flaky tests.
- `crates/fleet-agent/src/supervisor/channel.rs`: the test, `HANDSHAKE`, `RECONNECT_EVERY`, and the
  open loop that retries.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. The last checks run on main was reported red too; the human confirms whether
this test is why.
