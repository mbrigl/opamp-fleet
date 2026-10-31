# Ticket: Directory-handle work on Unix lives in one module, and `storage` keeps its adapter

## What to build

The raw `*at` system calls are wrapped twice and called bare in two more places: `nofollow` has its
own `openat`, `mkdirat`, `renameat` and `unlinkat`; `storage::handle`, about 570 lines inside the
`AgentStorage` adapter, has its own open, lstat, open-beneath and tree removal; `reconfigure`'s
`move_aside` and `service::manager` call `libc::unlinkat`, `renameat` and `mkdirat` directly.
`storage.rs` also carries a pure rule about an offered configuration (`check_entry_names`, called
from `reconfigure`) and re-exports `ProcessAccountGroup`, so the type has two paths in use.

Make one Unix module for work by directory handle, which `nofollow`, `storage`, `reconfigure` and
`service::manager` call, move `check_entry_names` to the core module that owns the offered
configuration, and drop the re-export.

## Acceptance criteria

- [ ] Given the crate, when it is searched for `libc::openat`, `mkdirat`, `renameat` or `unlinkat`,
  then they appear only in the one module.
- [ ] Given `dependency_direction.rs`, when it runs, then `check_entry_names` sits in a Core module
  and the new module has a role.
- [ ] Given the existing storage, reconfigure and no-link tests, when the change lands, then they
  pass unchanged.

## Read first

- ADR-0009, ADR-0037 clause 11.
- `crates/fleet-agent/src/nofollow.rs`, `storage.rs` (`mod handle`), `reconfigure.rs`,
  `service/manager.rs`; `docs/ARCHITECTURE.md`, Client supervision.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
