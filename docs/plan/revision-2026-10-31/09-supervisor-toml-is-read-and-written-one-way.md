# Ticket: `supervisor.toml` is read and written one way, and the no-link test sees reads

## What to build

ADR-0037 clause 11 covers every read a privileged step makes in the data root, the state directory
and the layout root. `service install` and `service config` read `supervisor.toml` through
`ClientConfig::load`, which reads by path (`config_file.rs`), and `config_init.rs` reads the
existing file and the verification key file by path too; the test `privileged_steps_follow_no_link`
lists only calls that change a file, so it cannot see this.

The file also has two writers with different rules: `reconfigure` replaces it by path, keeps the old
file's mode and does not sync before the rename; `config_init` writes it through a held directory,
always 0600, synced. One file, one way to replace it.

Route the privileged reads through `nofollow`, add the path-based read calls to the test's banned
list with their justified exceptions, and give `supervisor.toml` one replacement whose mode rule is
written down once.

## Acceptance criteria

- [ ] Given a link planted at `supervisor.toml`'s name by the service's account, when `service
  config` runs privileged, then it neither reads through the link nor writes through it.
- [ ] Given a path-based read added to a privileged module, when the no-link test runs, then it
  fails and names the call.
- [ ] Given a Server-delivered `[[supervisor]]` change and a `service config`, when each rewrites
  `supervisor.toml`, then both go through one function with one mode rule, and the file is synced
  before its rename.

## Read first

- ADR-0037 clause 11 (header, Decision, Enforcement).
- `crates/fleet-agent/tests/privileged_steps_follow_no_link.rs`,
  `crates/fleet-agent/src/nofollow.rs`, `config_file.rs`, `config_init.rs`, `reconfigure.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It changes the privileged install's file handling.
