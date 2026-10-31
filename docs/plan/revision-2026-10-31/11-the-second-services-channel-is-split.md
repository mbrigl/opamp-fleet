# Ticket: The second service's channel is split, and `service` and `supervisor` stop the cycle

## What to build

`supervisor::channel` holds, in 2800 production lines, the private wire protocol, the second
service's server, the Client's end, and the peer checks. It calls into `service`
(`service::manager::RESTART_DELAY_SECS`, `agents_process`, `service::runtime::ignore_sighup`,
`service::run_as`), and `service` calls `supervisor::channel` in fourteen places: two adapters that
depend on each other, which `dependency_direction.rs` does not see because it reads only core
modules.

Split the module into the wire protocol, the Client's end and the peer checks, move the service's
server beside the other parts of the second service, and have the composition root hand in what the
channel now takes from `service`. Update the overview's Client supervision paragraph in the same
change.

## Acceptance criteria

- [ ] Given the channel's modules, when they are read, then none names `crate::service`, and
  `service` names only the wire protocol and the server.
- [ ] Given the existing channel tests, when the change lands, then they pass unchanged on Linux,
  macOS and Windows.
- [ ] Given `docs/ARCHITECTURE.md`, when the change lands, then it names the new modules.

## Read first

- ADR-0009, ADR-0036, ADR-0037 clauses 17 to 23.
- `crates/fleet-agent/src/supervisor/channel.rs` and `channel/windows.rs`.

## Blocked by

- docs/plan/revision-2026-10-31/15-the-flaky-channel-test-holds.md

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It moves the Windows code paths, which only CI and cargo-xwin compile.
