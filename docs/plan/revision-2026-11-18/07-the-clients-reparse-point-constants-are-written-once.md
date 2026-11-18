# Ticket: The Client's reparse-point constants are written once

## What to build

The Windows constants for opening an entry as itself are declared three times inside
`fleet-agent`:

- `crates/fleet-agent/src/win32/files.rs`, near lines 44 to 49: `AS_ITSELF` and
  `REPARSE_POINT`;
- `crates/fleet-agent/src/supervisor/channel/windows.rs`, near lines 469 to 475:
  `OPEN_AS_ITSELF`, `REPARSE_POINT`;
- `crates/fleet-agent/src/service/run_as.rs`, near lines 1822 and 1959: `0x0200_0000`
  (`FILE_FLAG_BACKUP_SEMANTICS`) as a bare literal.

The overview names `win32` as the place where the Client declares the Windows system calls
neither the standard library nor a dependency offers. The constants are declared there once as
`pub(crate)`, and the other two modules take them from it.

`crates/fleet-util/src/nofollow.rs` (near lines 785 to 789) keeps its own copy, and this ticket
leaves it alone. Sharing it across the two crates has no cheap way:

- a public constant in `nofollow` that only the Client names is against the wording of
  ADR-0008 clause 16, which says who shares an item chooses its crate; the module test does not
  enforce that wording and would accept it, since both users name `nofollow`, so the choice is
  one of the judgements ticket 11 weighs;
- no manifest declares `windows-sys`, which reaches the lock only through other crates (0.52,
  0.59 and 0.61), so taking the constants from it would add a direct dependency.

Whether either is worth it is the human's to say in the hand-off. It is not part of this
ticket. `REPARSE_POINT` in `archive`'s private `unix_attributes` is a 7z attribute bit with the
same value and stays where it is.

## Acceptance criteria

- [ ] Given the change, when `fleet-agent`'s sources are searched for `0x0020_0000`,
  `0x0200_0000` and `0x400` as the reparse-point attribute, then each is declared once, in
  `win32`.
- [ ] Given the change, when the Windows build and tests run in CI, then they pass.

## Read first

- Touches: `crates/fleet-agent/src/win32.rs`, `crates/fleet-agent/src/win32/files.rs`,
  `crates/fleet-agent/src/supervisor/channel/windows.rs`, `crates/fleet-agent/src/service/run_as.rs`
- `docs/ARCHITECTURE.md`, the "Client supervision" bullet on `win32`; ADR-0008 clause 16.

## Blocked by

- docs/plan/revision-2026-11-18/06-the-re-own-walk-goes-through-the-clients-handle-layer.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.

Tickets 06, 07 and 08 all change `crates/fleet-agent/src/service/run_as.rs`, so they run one
after the other, in that order, not in parallel worktrees.
