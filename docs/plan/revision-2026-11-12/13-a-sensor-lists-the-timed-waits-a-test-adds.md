# Ticket: A sensor lists the timed waits a change adds to the tests

## What to build

Seven commits of the last count replaced a sleep or a timed budget in a test by a wait for its
event (57d079f7f, 9b67c92b2, 3f8d1e1e3, 5df5434dc, a4439657b, 7e78618ec, c30f42864), and the reviews
of three found the replacement still timed. `docs/CONVENTIONS.md` now holds "A test waits for its
event, never for a time". A convention is read; a sensor shows the line.

First, the waiter: `wait_until` is the same function in six test files of
`crates/fleet-agent/tests/`
(`e2e.rs`, `packages_e2e.rs`, `collector_extension_e2e.rs`, `http_transport_e2e.rs`,
`gateway_and_supervisor_e2e.rs`, `connection_settings_e2e.rs`), with variants in others, and
`tests/common/mod.rs` exists. Move it there.

Then the sensor: on the lines a change adds under `crates/*/tests/` and in `#[cfg(test)]` code, it
lists each `thread::sleep(`, `time::sleep(`, `sleep_until(` and each `Duration` literal outside a
`timeout(` or the waiter, unless the line carries a reason the sensor reads. It reports and never
fails. Add it to `scripts/sensor-tests-kept.sh` or beside it, with its self-test.

## Acceptance criteria

- [ ] Given the Client's end-to-end tests, when the change lands, then one `wait_until` serves them.
- [ ] Given a change that adds a `thread::sleep(` to a test without a reason, when the sensor runs,
  then it lists the line; given one with a reason, or a wait through the waiter, then it does not.
- [ ] Given the sensor's self-test, when `scripts/check-all.sh` runs, then it runs and passes.

## Read first

- Touches: `crates/fleet-agent/tests/common/mod.rs`, `crates/fleet-agent/tests/`,
  `scripts/sensor-tests-kept.sh`, `scripts/test-sensor-tests-kept.sh`, `scripts/check-all.sh`
- `docs/CONVENTIONS.md`, "A test waits for its event, never for a time"; ADR-0002;
  `scripts/sensor-tests-kept.sh`, the golden path of a sensor on the lines a change adds.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
