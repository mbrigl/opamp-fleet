# Ticket: A directory in `data` is made one way, whoever asks for it

## What to build

Two paths make a directory beneath a Supervisor's `data` root, and they make it differently where
a process account is registered:

- the directories a kind's manifest names (`Manifest::data_dirs`) reach the `Runner` as
  `ProcessSpec::ensure_dirs`, built by `data_dirs` in `crates/fleet-agent/src/supervisor/host.rs`.
  `Runner::spawn_once` in `crates/fleet-agent/src/supervisor/process.rs` makes each one through
  `storage::create_shared_dir`, which leaves a parent it creates on the way at `0710`. For
  `data_dirs = ["lib/state"]`, `data/lib` is passed through and not writable by the Managed
  Process;
- the host function `create_dir` (`Roots::create_dir` in
  `crates/fleet-agent/src/supervisor/host/files.rs`) makes every directory on the way as a kind's
  data directory, `2770` and the process account's group, since c410541db.

`process::data_root` still chooses, for each directory, "the outermost of `ensure_dirs` that holds
it, which an operator placed". No operator places one any more. Every directory a manifest names
lies beneath the Supervisor's directory, so the function always answers that directory.

After this ticket, one function makes a directory beneath `data` and every directory on the way to
it, as [CLT-accounts-service-runs-under §21](../../adr/CLT-accounts-service-runs-under.md) states
for a kind's data directories. The manifest's directories before a spawn and the host function
`create_dir` both call it. `data_root` and its "operator placed" case go.

Evidence: the first review of PR #231 (at ff170c6de) found the modes of a Plugin's data
subdirectories wrong. The second review found the parents of a nested one left unshared. c410541db
fixed the host function alone, so the manifest's path still makes the parents the old way. This
is the convention "A rule two paths apply is one function" of `docs/CONVENTIONS.md`.

## Acceptance criteria

- [ ] Given a kind whose manifest names `lib/state` and a registered process account, when the
      core spawns its Managed Process, then `data`, `data/lib` and `data/lib/state` each carry the
      process account's group and mode `2770`.
- [ ] Given the same Supervisor, when its Plugin calls `create_dir` on `data:lib/other`, then
      `data/lib/other` carries the same group and mode, and the test that decides it names the
      function both paths call.
- [ ] Given no registered process account, when either path makes `data/lib/state`, then every
      directory is the Client's alone, and none is reached through a link.
- [ ] Given the tree, when `data_root` is searched for, then it is gone, and
      `the_directories_an_agent_writes_into_are_made_before_it_runs` and
      `state_is_the_clients_alone_and_data_is_shared` pass.

## Read first

- Touches: `crates/fleet-agent/src/supervisor/host.rs`,
  `crates/fleet-agent/src/supervisor/host/files.rs`,
  `crates/fleet-agent/src/supervisor/process.rs`, `crates/fleet-agent/src/storage.rs`,
  `crates/fleet-agent/tests/supervisor_process.rs`
- ADR: CLT-accounts-service-runs-under §15 and §21; CLT-wrappers-act-through-the-core §3 and §6.
- Convention: "A rule two paths apply is one function" in `docs/CONVENTIONS.md`.
- Golden path: `Roots::create_dir` as c410541db left it.
- Seam: `state_is_the_clients_alone_and_data_is_shared` in `supervisor/host/files.rs` and
  `the_directories_an_agent_writes_into_are_made_before_it_runs` in
  `tests/supervisor_process.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human: DEV-maintainer-decides-agents-implement §5 caps open unattended agent work at
zero. The stop for the session: where the Windows grant of `storage::grant` cannot take the same
parents without a change to its ACL rules, it leaves its state in this ticket and hands off.
