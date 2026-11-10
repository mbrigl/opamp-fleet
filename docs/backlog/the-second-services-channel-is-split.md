# The second service's channel is split, and service and channel stop calling each other

**Kind:** idea, structure.

**Observed.** `crates/fleet-agent/src/supervisor/channel.rs` is 4 754 lines on `main` and holds the
private wire protocol, the second service's server, the Client's end and the peer checks. It calls
into `service` (`RESTART_DELAY_SECS`, `agents_process`, `ignore_sighup`, `run_as`), and `service`
calls `supervisor::channel` in many places: two adapters that depend on each other, which
`crates/fleet-core/tests/dependency_direction.rs` does not see, because it reads only core
modules.

**What the old branch had.** Revision ticket 11 of `feat/revision-2026-10-31`, open, not worked:
split the module into the wire protocol, the Client's end and the peer checks, move the service's
server beside the other parts of the second service, and have the composition root hand in what
the channel takes from `service`, with the overview's Client supervision paragraph. It was blocked
by ticket 15, the flaky channel test, which `feat/revision-hardening` carries.
