# Ticket: The reparse-point constants are written once

## What to build

The Windows constants for opening a link as itself are written twice:
`crates/fleet-util/src/nofollow.rs` (near lines 785 to 789: `FILE_SHARE_READ`,
`FILE_SHARE_WRITE`, `FILE_FLAG_BACKUP_SEMANTICS`, `FILE_FLAG_OPEN_REPARSE_POINT`,
`FILE_ATTRIBUTE_REPARSE_POINT`) and `crates/fleet-agent/src/win32/files.rs` (near lines 44 to
49: `AS_ITSELF`, `FILE_SHARE_*`, `REPARSE_POINT`). The Unix side was brought to one place by
dd24b1a02 and 419a433df; the Windows side was not.

`win32::files` takes the constants `nofollow` already declares, made public there, or both
take them from the `windows-sys` feature the workspace already enables, if it carries them;
the session checks which before it chooses, and adds no dependency.

## Acceptance criteria

- [ ] Given the change, when the two files are read, then each constant is declared once.
- [ ] Given the change, when the Windows build and tests run in CI, then they pass.

## Read first

- Touches: `crates/fleet-util/src/nofollow.rs`, `crates/fleet-agent/src/win32/files.rs`
- ADR-0008 clause 16 (an item the Client and the tool share lives in `fleet-util`).

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
