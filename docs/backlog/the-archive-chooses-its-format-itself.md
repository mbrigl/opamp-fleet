# The archive module chooses an artifact's format itself

**Kind:** idea, structure.

**Observed.** `crates/fleet-agent/src/archive.rs` exposes `detect` and six format functions whose
parameters differ: `extract_7z(archive, member, out, key)`, `extract_tar_gz(archive, member, out)`,
`extract_zip`, `extract_tree_tar_gz(archive, program_path, kept, dest)`,
`extract_tree_7z(archive, program_path, dest, key)` and `extract_tree_zip`. Each caller matches on
the format itself: `crates/fleet-agent/src/install.rs` (near line 97) and
`crates/fleet-agent/src/supervisor/process.rs` (near line 2166). One concept, "unpack this
artifact", is spread over three files.

**Idea.** Two entry points, one for a member and one for a tree, take a request with the program
path, the kept links and the key. Each dispatches on the format inside `archive`, and the format
functions become private. The operator tool's tests call the tree entry instead of
`extract_tree_tar_gz`. The kept links are part of the request, and they stay: the
human decided so on 2026-11-15
(`docs/backlog/what-the-core-repack-added-without-a-caller-is-used-by-a-kind.md`).

**From.** The design revision of 2026-11-15, ticket 07, moved to the backlog by the human
(2026-11-15).
