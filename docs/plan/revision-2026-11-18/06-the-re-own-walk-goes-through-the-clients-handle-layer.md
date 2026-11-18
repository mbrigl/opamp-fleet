# Ticket: The re-own walk goes through the Client's handle layer

## What to build

The Client walks a tree by directory handle in two places. `storage::handle`
(`crates/fleet-agent/src/storage.rs`, from near line 643) holds `remove_tree_at`,
`share_tree` and the walk of `shared_for_writing`. `service::run_as`
(`crates/fleet-agent/src/service/run_as.rs`, near lines 1315 to 1490) has a re-own walk of its
own, takes `entries`, `fstat` and `mode` from `storage::handle`, and calls
`libc::fchownat` itself (near line 1486), the one `*at` system call of the Client outside
`storage::handle` and `fleet_util::nofollow::at`.

The `fchownat` moves into `storage::handle` beside the calls only the Client makes, and the
re-own walk uses it there. Whether the re-own walk and `share_tree` share one walker is the
session's judgement, said in its hand-off. The overview's sentence on `storage::handle` loses
its exception for `run_as`.

## Acceptance criteria

- [ ] Given the change, when the Client's sources are searched for `libc::` `*at` calls, then
  each is in `storage::handle` or `fleet_util::nofollow::at`.
- [ ] Given the change, when the re-own tests of `service::run_as` and the test
  `privileged_steps_follow_no_link` run on Linux and macOS, then they pass unchanged.

## Read first

- Touches: `crates/fleet-agent/src/storage.rs`, `crates/fleet-agent/src/service/run_as.rs`,
  `docs/ARCHITECTURE.md` (the `fleet-util` bullet)
- ADR-0022 (the accounts the service runs under).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.

Tickets 07 and 08 change `crates/fleet-agent/src/service/run_as.rs` too, and 08
`crates/fleet-agent/src/storage.rs`; both wait for this one, so they do not run in parallel
worktrees beside it.
