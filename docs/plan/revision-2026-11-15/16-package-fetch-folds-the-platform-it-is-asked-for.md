# Ticket: `package fetch` folds the platform it is asked for through the core

## What to build

Pull request #180 has `opamp-fleetctl package fetch` fold the platforms of an upstream's assets
through `fleet_core::platform`. Its review left the `--platform` input itself unfolded:
`select_platforms` in `crates/fleet-tools/src/fetch.rs` compares the `os/arch` the operator
writes with the folded names as written. So `--platform linux/x86_64` matches none of the assets
that `linux/amd64` matches. The input is folded through the same table before it is compared.

## Acceptance criteria

- [ ] Given a release with a `linux/amd64` asset, when `package fetch --platform linux/x86_64`
  runs, then it selects that asset, as `--platform linux/amd64` does.
- [ ] Given a `--platform` the release does not publish, when the command runs, then it answers
  as today, naming what the release has.

## Read first

- Touches: `crates/fleet-tools/src/fetch.rs`, `crates/fleet-core/src/platform.rs`
- ADR-0050 clause 6 (the platform table), ADR-0053, ADR-0008 clause 17.
- Work this after pull request #180 lands.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
