# Ticket: `supervisor.toml` is read and written one way, and the no-link test sees reads

## What to build

ADR-0037 clause 11 covers every read a privileged step makes in the data root, the state directory
and the layout root. `service install` and `service config` read `supervisor.toml` through
`ClientConfig::load`, which reads by path (`config_file.rs`), and `config_init.rs` reads the
existing file and the verification key file by path too; the test `privileged_steps_follow_no_link`
lists only calls that change a file, so it cannot see this.

The file also has two writers with rules that conflict on purpose: `reconfigure::write_replacement`
replaces it by path, keeps the operator's existing mode (0600 for a new file) and does not sync
before the rename, as ADR-0017 clause 30 decides for a Server-driven apply; `config_init` writes it
through a held directory, always 0600, synced.

Route the privileged reads through `nofollow`, add the path-based read calls to the test's banned
list with their justified exceptions, and give `supervisor.toml` one replacement. Which mode rule it
applies is the human's: both kept, each named by its caller, or one rule, and `propose-adr` is
needed for one that departs from ADR-0017 clause 30.

## Acceptance criteria

- [ ] Given a link planted at `supervisor.toml`'s name by the service's account, when `service
  config` runs privileged, then it neither reads through the link nor writes through it.
- [ ] Given a path-based read added to a privileged module, when the no-link test runs, then it
  fails and names the call.
- [ ] Given a Server-delivered `[[supervisor]]` change and a `service config`, when each rewrites
  `supervisor.toml`, then both go through one function, the file is synced before its rename, and
  the mode each gives is the one the human chose (with an ADR where it departs from ADR-0017 clause
  30).

## Read first

- ADR-0037 clause 11 (header, Decision, Enforcement); ADR-0017 clause 30.
- `crates/fleet-agent/tests/privileged_steps_follow_no_link.rs`,
  `crates/fleet-agent/src/nofollow.rs`, `config_file.rs`, `config_init.rs`, `reconfigure.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It changes the privileged install's file handling.
