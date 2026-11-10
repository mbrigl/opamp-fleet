# One Unix module for work by directory handle

**Kind:** idea, structure.

**Observed.** The raw `*at` system calls are wrapped twice and called bare in two more places on
`main`: `crates/fleet-agent/src/nofollow.rs` has its own `openat`, `mkdirat`, `renameat` and
`unlinkat`; `storage.rs` has its own open, lstat, open-beneath and tree removal inside the
`AgentStorage` adapter; `reconfigure.rs` and `service/manager.rs` call `libc::unlinkat`,
`renameat` and `mkdirat` directly. `storage.rs` also carries a pure rule about an offered
configuration (`check_entry_names`) and re-exports `ProcessAccountGroup`, so the type has two paths
in use.

**What the old branch had.** Revision ticket 10 of `feat/revision-2026-10-31`, open, not worked: one
Unix module for work by directory handle that `nofollow`, `storage`, `reconfigure` and
`service::manager` call, `check_entry_names` moved to the core module that owns the offered
configuration, and the re-export dropped.
