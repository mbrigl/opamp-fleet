# Ticket: The archive module chooses an artifact's format itself

## What to build

`crates/fleet-agent/src/archive.rs` exposes `detect` and six format functions whose parameters
differ: `extract_7z(archive, member, out, key)`, `extract_tar_gz(archive, member, out)`,
`extract_zip`, `extract_tree_tar_gz(archive, program_path, kept, dest)`,
`extract_tree_7z(archive, program_path, dest, key)` and `extract_tree_zip`. Each caller matches
on the format itself: `crates/fleet-agent/src/install.rs` (near line 97) and
`crates/fleet-agent/src/supervisor/process.rs` (near line 2166). A wide, shallow surface spreads
one concept, "unpack this artifact", over three files.

Two entry points, one for a member and one for a tree, take a request with the program path, the
kept links and the key, and dispatch on the format inside `archive`; the format functions become
private. The operator tool's use of `extract_tree_tar_gz` in its tests moves to the tree entry.

## Acceptance criteria

- [ ] Given an artifact in each format, when the install and the Supervisor unpack it, then they
  call one entry point and do not name the format.
- [ ] Given the existing archive, install and fuzz tests, when the change lands, then they pass,
  `fuzz/fuzz_targets/archive_tree.rs` included.

## Read first

- Touches: `crates/fleet-agent/src/archive.rs`, `crates/fleet-agent/src/install.rs`,
  `crates/fleet-agent/src/supervisor/process.rs`, `crates/fleet-tools/src/fetch.rs`, `fuzz/`
- ADR-0050 (package formats), ADR-0008 clause 22, Q-2.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
