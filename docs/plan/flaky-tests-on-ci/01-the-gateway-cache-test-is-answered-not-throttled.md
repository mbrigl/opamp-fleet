# Ticket: The Gateway cache test is answered, not throttled

## What to build

`an_artifact_that_fails_its_hash_is_neither_stored_nor_served` in
`crates/fleet-agent/tests/gateway_package_cache.rs` failed once on the Windows job of the `Checks`
workflow on `main` (run 41449020197): its one download through the Gateway was answered `429`
where the test expects `404`. The test sends one report and one download from one host, so no
bound a host reaches by its own requests explains a `429`. Find which limit answered it, the
Gateway's rate limit per downstream host, the Server's admission throttle in the test's upstream,
or a limit the tests of this binary share because they run side by side from one address, and make
the test decide what it asserts whatever runs beside it. A limit that refuses a host for requests
it did not send is a defect of that limit, not of the test.

## Acceptance criteria

- [ ] Given the cause, when it is named, then the hand-off shows the limit that answered `429` and
  why this test's requests reached it
- [ ] Given the tests of `gateway_package_cache.rs` run side by side on a loaded host, when they run
  thirty times, then none is answered `429` it did not provoke
- [ ] Given a fix in a limit rather than in the test, when that limit is checked with its mechanism
  broken, then a test fails

## Read first

- [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done), which counts a flaky
  test as a defect
- ADR-0035 on the Gateway's rate limit per downstream host, and ADR-0045 on the Server's admission
  throttle
- Seam: the test's upstream and Gateway in `crates/fleet-agent/tests/gateway_package_cache.rs`

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
