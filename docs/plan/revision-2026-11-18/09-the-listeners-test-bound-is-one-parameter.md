# Ticket: The listener's test bound is one parameter, named for what it bounds

## What to build

Since axum-server 0.9 and the silent-peer fix, one timeout bounds both the time to a
connection's first request and an HTTP/1 request's headers (`bounded` in
`crates/opamp/src/server/listen.rs`, near line 411). The Gateway still offers three entry points
for it (`run_on`, `run_on_bounded`, `run_on_timed` in
`crates/fleet-agent/src/gateway/mod.rs`, near lines 225 to 260), of which `run_on_bounded` is
a narrower `run_on_timed` with two callers. The field doc of `Endpoint::header_read_timeout`
(`crates/fleet-agent/src/supervisor/endpoint.rs`, near line 37) still says it bounds the
request headers alone, while its setter was corrected in 3bc4489c9.

`run_on_bounded` goes, its callers call `run_on_timed`, and the field doc says what the bound
covers.

## Acceptance criteria

- [ ] Given the change, when `gateway/mod.rs` is read, then the Gateway has `run_on` and
  `run_on_timed` and nothing between them.
- [ ] Given the change, when the Gateway's and the Supervisor Endpoint's suites run, then they
  pass unchanged in number and name.

## Read first

- Touches: `crates/fleet-agent/src/gateway/mod.rs`, `crates/fleet-agent/src/supervisor/endpoint.rs`
- ADR-0012 clause 15; ADR-0016.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
