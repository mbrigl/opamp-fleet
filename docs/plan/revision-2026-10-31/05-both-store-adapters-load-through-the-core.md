# Ticket: The filesystem and PostgreSQL adapters load a record through the core's rules

## What to build

Each Server store adapter re-checks the domain's rules while it decodes: the package entry's
identifier, platform, hash and signature length in `fs/packages.rs` and `postgres/packages.rs`, and
the Deployment's name, Selector, Package and Configuration names in `fs/deployments.rs` and
`postgres/deployments.rs`. The agent adapters already differ (`PackageId::parse` in one,
`PackageId::load` in the other). A third adapter would copy the rules a third time.

Build core constructors (an entry, a Deployment and an assignment loaded from their stored fields)
in the core modules, which both adapters call after they have mapped their rows or JSON, so a rule
changes in one place.

## Acceptance criteria

- [ ] Given a stored record that breaks a domain rule, when the filesystem and the PostgreSQL
  adapter load it, then both refuse it with the same message, decided by one core constructor.
- [ ] Given the `contract` tests, when the change lands, then each store's contract test covers a
  record the core refuses, on both stores.
- [ ] Given the agents adapters, when an assignment's package is read, then both use the same parse.

## Read first

- ADR-0009, ADR-0029 (header, Decision, Enforcement).
- The golden path in `docs/ARCHITECTURE.md`: `FloorStore`, `fs::floors`, `postgres::floors` and its
  `contract` test.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
