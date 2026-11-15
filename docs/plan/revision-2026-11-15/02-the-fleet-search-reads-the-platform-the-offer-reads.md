# Ticket: The fleet search reads the platform the offer reads

## What to build

`listing::derive` (`crates/fleet-server/src/listing.rs`, near line 270) builds the platform an
operator searches by (`q`) as `format!("{os}-{arch}")` from the non-identifying attributes,
without folding it through `fleet_core::platform`. `packages::Platform::reported`
(`crates/fleet-server/src/packages.rs`, near line 67) folds it and falls back to the identifying
attributes. A search can therefore show `linux-x86_64` where the offer reads `linux/amd64`.

`derive` and the build of `AgentView` in `crates/fleet-server/src/fleet.rs` also derive the
same display values twice: the attribute split, the remote configuration status, the service
version and the OS description fallback. One of them takes the values from the other, and the
platform is the one `Platform::reported` reads.

## Acceptance criteria

- [ ] Given an Agent that reports `os.type` `linux` and `host.arch` `x86_64`, when the fleet
  is searched for `linux/amd64` (or the token the core writes), then the Agent is found.
- [ ] Given an Agent that reports its platform only in identifying attributes, when the fleet is
  listed, then its platform is shown.
- [ ] Given the change, when `listing.rs` and `fleet.rs` are read, then the display values are
  derived in one place.

## Read first

- Touches: `crates/fleet-server/src/listing.rs`, `crates/fleet-server/src/fleet.rs`,
  `crates/fleet-server/src/packages.rs`
- ADR-0031 (what the search reads), ADR-0050 clause 6 (the platform table).
- Pull request #180 moves the Server's platform rule into `fleet_core::platform`; work this
  after it lands.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
