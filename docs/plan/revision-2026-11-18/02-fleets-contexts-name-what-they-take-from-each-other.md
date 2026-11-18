# Ticket: The contexts of `fleet` name what they take from each other

## What to build

Every production submodule of `crates/fleet-server/src/fleet/` starts with `use super::*`. The
module root imports about 45 names it does not use itself (among them `Presented`,
`Recorder`, `Registrar`, `DeploymentError`, `Refusal`, `ServerToAgent`) and the items of
`fleet::held` and `fleet::placement` the other contexts call (`Taking`, `Took`, `Settled`,
`PendingRekey`, `UnsureRekey`, `Carried`, `Elsewhere`), so that its children get them by
the glob. A reader cannot see from a context's file which other context or store it depends on.
The review of PR #202 found this for the siblings' items it moved; the crate-level imports and
those of `held` and `placement` stayed.

Each context imports by name what it uses from the root, from its siblings and from the crate.
The root imports only what it uses. A check keeps it so, such as `clippy::wildcard_imports`
denied for `fleet`'s production code, which leaves `use super::*` in test modules alone; the
session verifies that the lint behaves so before it relies on it.

The module comments say what each module is, not what it was: the seven that end "Behind the
same `AppState` entry points as before" lose "as before", and the root's comment no longer
names the OpAMP control loop, which is `fleet::session` now.

## Acceptance criteria

- [ ] Given the change, when `fleet.rs`'s imports are read, then each name is used in the
  root's own code.
- [ ] Given a production submodule of `fleet`, when a `use super::*` is added to it, then
  `scripts/check-all.sh` fails.
- [ ] Given the change, when the fleet's unit tests run, then the same 155 tests pass.

## Read first

- Touches: `crates/fleet-server/src/fleet.rs`, `crates/fleet-server/src/fleet/`
- `docs/ARCHITECTURE.md`, the paragraphs on `fleet::held` to `fleet::session`.
- `docs/CONVENTIONS.md`: "An item lives in the context it changes".

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
