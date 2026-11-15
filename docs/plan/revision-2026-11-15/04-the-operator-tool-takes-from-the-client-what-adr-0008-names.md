# Ticket: The operator tool takes from the Client only what an ADR names

## What to build

ADR-0008 clause 22 names what `fleet-tools` uses of the Client. It does not name two functions the
tool calls: `fleet_agent::nofollow::read_privileged` and
`fleet_agent::config_init::read_password_file` (`crates/fleet-tools/src/pki.rs`). No test checks
what the tool takes from the Client.

The human decides one of two:

- a superseding ADR through `propose-adr` names both functions; or
- `fleet-tools` stops using them.

Either way, a test lists the `fleet_agent::` paths that `fleet-tools` uses against an allowlist.
`fleet_agent::tls::pkcs12_identity` stays on that list, since ADR-0053 clauses 5 and 24 name it.

This must be settled before the next normalization of the ADR record.

**Effort.** S.

## Acceptance criteria

- [ ] Given the change, when `fleet-tools` is searched for `fleet_agent::`, then every path is one
  an accepted ADR names.
- [ ] Given a new `fleet_agent::` path in `fleet-tools` outside the allowlist, when the tests run,
  then a test fails and names the path.

## Read first

- Touches: `crates/fleet-tools/src/pki.rs`, `crates/fleet-core/tests/dependency_direction.rs`
- ADR-0008 clause 22; ADR-0053 clauses 5 and 24.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero. It may need a superseding ADR, which only the human decides.
