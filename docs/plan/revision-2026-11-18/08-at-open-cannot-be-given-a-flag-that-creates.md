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
  `crates/fleet-agent/src/service/run_as.rs`
- ADR-0022 (no link is followed beneath a held root).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
