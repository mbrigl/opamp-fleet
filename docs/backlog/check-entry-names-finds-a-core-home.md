# `check_entry_names` finds a home in the core

**Kind:** idea, structure.

**Observed.** `check_entry_names` in `crates/fleet-agent/src/storage.rs` is a pure rule about an
offered configuration: no entry may be stored under a Windows device name or a name ending in a
dot, and no two entries may share one file name compared without case. `storage` is an adapter,
yet `crates/fleet-agent/src/reconfigure.rs` calls the rule there for an offer before anything is
stored. The rule rests on `entry_file_name`, the storage's mapping from an entry to its file name,
so moving the rule moves that mapping too.

**Open.** No core module owns the offered configuration today:
`crates/fleet-agent/src/supervisor/agent/remote_config.rs` is an `impl AgentState`. Where the rule
and the naming belong is a structural choice, for the design revision or a ticket.

**From.** The rest of the backlog entry `one-directory-handle-module.md`, whose directory-handle
part landed on `refactor/archive-and-dir-handles` (2026-11-15).
