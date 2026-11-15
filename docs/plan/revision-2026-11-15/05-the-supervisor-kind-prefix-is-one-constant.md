# Ticket: The `supervisor.kind.` prefix is one constant of the protocol crate

## What to build

The attribute prefix by which the Server recognises a Client Agent is spelled twice:
`CLIENT_KIND_PREFIX` in `crates/fleet-server/src/labels.rs` (near line 35) and
`format!("supervisor.kind.{}", ..)` in `crates/fleet-agent/src/supervisor/mod.rs` (near line
117). Its siblings `SUPERVISOR_PREFIX` and `SUPERVISOR_PACKAGE_VERSION` already live in
`crates/opamp/src/attributes.rs`, which both ends use. A typo on one side silently breaks
placement. The prefix moves there, and both ends use it.

## Acceptance criteria

- [ ] Given the change, when the non-test code is searched for `"supervisor.kind.`, then it is
  found once, in `opamp::attributes`.
- [ ] Given the existing placement and Supervisor tests, when the change lands, then they pass.

## Read first

- Touches: `crates/opamp/src/attributes.rs`, `crates/fleet-server/src/labels.rs`,
  `crates/fleet-agent/src/supervisor/mod.rs`
- ADR-0054 (what an Agent reports), ADR-0052 clause 19.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
