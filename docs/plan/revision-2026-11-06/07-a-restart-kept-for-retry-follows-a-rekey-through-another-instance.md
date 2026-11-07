# Ticket: A restart kept for retry follows a re-key through another instance

## What to build

On PostgreSQL a restart command whose send failed is set queued again in the store
(`put_back` in `crates/fleet-server/src/fleet.rs`); where the store does not take it, the
put-back is kept in `unput_restarts`, keyed by the Agent's `instance_uid`, and `retry_restarts`
tries it again. A re-key through this instance moves the kept put-back to the new
`instance_uid` (`rekey_held`). A re-key through another instance while this one tries does not:
the next try calls `set_restart_pending` on the old `instance_uid`, whose row is gone, the store
answers `Ok(None)` (`postgres/agents.rs`), and `put_back` drops the entry with a warning naming
the Agent. The Operator's restart is lost, and draft clause 8 names this as the third case not
seen.

A kept put-back reaches the Agent's row after a re-key through another instance too, so the
retry queues the restart on the new `instance_uid`. Two ways are open, and the choice is the
human's: on `Ok(None)` the instance follows a re-key the other instance announced, as a read back
of the old identity, or the put-back hangs on the row in the store, so the re-key carries it.
Either may need a migration, which takes the next free number and names its oldest Server.

## Acceptance criteria

- [ ] Given a restart whose send failed and whose put-back the store refused, when another
  instance re-keys the Agent before the retry, then the retry queues the restart on the new
  `instance_uid` and the Agent is sent it at its next report there.
- [ ] Given the same kept put-back, when the Agent is forgotten through another instance before
  the retry, then no restart is queued and the warning is written as now.
- [ ] Given draft clause 8's third case not seen and the manual's restart route, when the change
  lands, then both no longer name it.
- [ ] Given the existing restart and re-key tests in `fleet.rs` and the store checks, when the
  change lands, then they pass unchanged.

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side
  clause 8 (the restart's put-back and the cases not seen) and clause 9 (announcements).
- `fleet.rs`: `put_back`, `retry_restarts`, `rekey_held`, `read_unsure_rekey`;
  `postgres/agents.rs` `set_restart_pending` and `rekey`.
- `crates/fleet-server/src/fleet.rs` test
  `a_kept_put_back_whose_row_another_instance_rekeyed_is_logged`, which shows the case today.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.

It does not block G-20 or the pull request of stage 2 of
docs/plan/several-server-instances/spec.md. The loss needs a put-back the store refused, a
reconnect to another instance and a re-key there while the put-back is still kept; it is written
to the log naming the Agent and named in the draft and the manual, so the Operator
sees it and requests the restart again. Closing it likely needs a migration, which stage 2 does
not need otherwise.
