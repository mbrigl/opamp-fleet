# Ticket: A repeated offer of the same configuration is applied once

## What to build

After a rollout, a heartbeat that left the Client before the Server's push arrived still names the
old hash, and the Server answers it with the offer again, so the same `remote_config` reaches the
Client twice. The Client applies every offer it receives: the second apply waits behind the first
and restarts the Managed Process once more after the first has reported `APPLIED`. A production
host suffers the same needless restart. The Client, for a Supervisor's Agent and for its own,
ignores an offer whose hash it is applying or has applied, and reports its status again, as
[ADR-0027](../../adr/0027-packages-signatures-and-delivery.md) clause 49 does for packages; a
deliberate restart stays the `Restart` command. The clause is drafted through `propose-adr` as a
restatement superseding
[ADR-0017](../../adr/0017-supervisor-mode-and-its-kinds.md) before the code lands, and the human
accepts it.

## Acceptance criteria

- [ ] Given a Supervisor's Agent applying a configuration, when the same hash is offered again,
  then the Managed Process is started once and the Client reports the status it holds
- [ ] Given a configuration applied and reported `APPLIED`, when the same hash is offered again,
  then nothing is restarted and `APPLIED` is reported again
- [ ] Given the Client's own Agent, when the same hash is offered twice, then it is applied once
- [ ] Given a different hash offered while one is applying, when it arrives, then it is applied
  after the first, as today
- [ ] Given `a_config_change_reaches_both_supervised_agents_over_one_connection`, when the Server's
  push is delayed past a heartbeat, then it passes

## Read first

- [ADR-0017](../../adr/0017-supervisor-mode-and-its-kinds.md) for the handling of a received remote
  configuration; [ADR-0027](../../adr/0027-packages-signatures-and-delivery.md) clause 49 for the
  pattern
- The hand-off of the timing-tests ticket, `tests-wait-for-an-event`, for the proof with a delayed
  push
- Seam: the Supervisor's apply in `reconfigure.rs` and the e2e test with a delayed push

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It needs a superseding ADR the human accepts.
