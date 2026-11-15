# Ticket: The fleet search reads the platform the offer reads

## What to build

`listing::derive` (`crates/fleet-server/src/listing.rs`, near lines 270 to 276) builds the platform
an operator searches by (`q`) raw, as `format!("{os}-{arch}")`. It reads the literal keys `os.type`
and `host.arch` from the non-identifying attributes only, and it does not fold them through the
core's table. `packages::Platform::reported` folds both and falls back to the identifying
attributes. A search can therefore show `linux-x86_64` where the offer reads `linux-amd64`.

The platform becomes `Platform::reported(..).map(|p| p.tag())`, whose tag reads `linux-amd64`.

**Effort.** S.

## Acceptance criteria

- [ ] Given an Agent that reports `os.type` `linux` and `host.arch` `x86_64`, when the fleet is
  searched for `linux-amd64`, then the Agent is found.
- [ ] Given an Agent that reports its platform only in identifying attributes, when the fleet is
  listed, then its platform is shown.

## Read first

- Touches: `crates/fleet-server/src/listing.rs`, `crates/fleet-server/src/packages.rs`
- ADR-0031 (what the search reads), ADR-0050 clause 6 (the platform table).
- Pull request #180 moves the Server's platform rule into `fleet_core::platform`; work this after
  it lands.

## Blocked by

- None as a ticket. Work it after pull request #180 merges, which changes the same code.

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
