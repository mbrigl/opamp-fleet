# Ticket: `at::open` cannot be given a flag that creates

## What to build

`fleet_util::nofollow::at::open` (`crates/fleet-util/src/nofollow.rs`, near line 497) takes raw
`c_int` flags and refuses `O_CREAT` and `O_TMPFILE` at run time, since `openat` would read
a mode the call does not pass. The guard came with a fix (1c9a61858) after the function became
public. The interface relies on a check where its type could rule the mistake out.

`open` takes a type that cannot carry a flag that creates (a small set of named opening modes,
or flags built without the creating ones), and `create` stays the one way to create. Every
caller in `fleet-util` and `fleet-agent` moves to it.

## Acceptance criteria

- [ ] Given the change, when a caller passes a flag that creates to `at::open`, then it does
  not compile.
- [ ] Given the change, when the tests of `nofollow` and `storage::handle` run, then they pass;
  the run-time refusal and its test go, with the reason in the hand-off.

## Read first

- Touches: `crates/fleet-util/src/nofollow.rs`, `crates/fleet-agent/src/storage.rs`,
  `crates/fleet-agent/src/service/run_as.rs`, and the other callers of `at::open`:
  `crates/fleet-agent/src/service/manager.rs` (near lines 520, 546 and 1829),
  `crates/fleet-agent/src/supervisor/channel.rs` (near lines 1130 to 1136 and 1170), beside
  the callers inside `nofollow.rs` itself and the imports in `storage.rs` and `run_as.rs`; and
  those that pass
  raw flags through `storage::handle::open_beneath`:
  `crates/fleet-agent/src/supervisor/process.rs` (near line 2242) and
  `crates/fleet-agent/src/reconfigure.rs` (near lines 337 and 469)
- ADR-0022 (no link is followed beneath a held root).

## Blocked by

- docs/plan/revision-2026-11-18/06-the-re-own-walk-goes-through-the-clients-handle-layer.md
- docs/plan/revision-2026-11-18/07-the-clients-reparse-point-constants-are-written-once.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.

Tickets 06, 07 and 08 all change `crates/fleet-agent/src/service/run_as.rs`, and 06 and 08
`crates/fleet-agent/src/storage.rs`, so they run one after the other, not in parallel
worktrees.
