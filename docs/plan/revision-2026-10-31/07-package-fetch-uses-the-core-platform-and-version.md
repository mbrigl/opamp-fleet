# Ticket: `package fetch` names platforms and orders versions by the core's rules

## What to build

`opamp-fleetctl package fetch` keeps a platform alias table of its own (`normalize_os`,
`normalize_arch` in `crates/fleet-tools/src/fetch.rs`) beside `fleet_core::platform`, and the two
disagree: the core maps `osx`, `win`, `win32`, `win64`, `x86-64` and `x64`, the tool maps `i386` and
knows only `macos`. It also sorts versions by a hand-written numeric key instead of
`fleet_core::version`'s precedence. ADR-0008 puts the platform aliases and the version in
`fleet-core` because the tool makes what an end must accept.

Map upstream platform names through `fleet_core::platform` and order versions by its precedence;
where the tool needs an alias the core lacks, add it to the core.

## Acceptance criteria

- [ ] Given an upstream release named with an alias the core knows, when `package fetch` maps it,
  then it names the platform the Server and the Client name.
- [ ] Given versions with pre-release parts, when `package fetch` orders them, then the order is
  `fleet_core::version`'s.
- [ ] Given the alias `i386`, when the human has decided whether it is the core's, then both crates
  agree.

## Read first

- ADR-0008 clause 16, ADR-0028 (the version rule), ADR-0032.
- `crates/fleet-core/src/platform.rs`, `crates/fleet-core/src/version.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. Which aliases the core accepts is the human's to confirm.
