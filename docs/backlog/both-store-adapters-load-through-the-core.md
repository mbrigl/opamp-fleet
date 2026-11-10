# Both store adapters load a record through the core's rules

**Kind:** doubt, structure.

**Observed.** The filesystem and the PostgreSQL adapters each re-check the domain rules of a stored
record on load, and the two checks differ: on `main` the filesystem store parses a record in
`crates/fleet-server/src/fs/agents.rs` (near line 162) while the PostgreSQL store loads it in
`crates/fleet-server/src/postgres/agents.rs` (near line 1210) by rules of its own (ADR-0023).

**What the old branch had.** Revision ticket 05 of `feat/revision-2026-10-31`, commits 285e8b41 and
a79f629d. `Entry::load`, `Deployment::load` and `Assignment::load` in the core decided every rule a
stored package entry, Deployment and assignment is held to; both adapters mapped their rows or
files to fields and called them. An assignment's Package was read by one rule on both stores. Each
store's contract test planted a record the core refuses. It was not carried: both adapters have
been reworked since by the stages of several Server instances, and the rebase is heavy.
