# Ticket: The certificate's subject values are one module, and the Host register leaves `revocation`

## What to build

The subject a certificate carries is spread over three files of `fleet-server`: `SubjectValues` and
its `fleet.host.*` keys in `revocation.rs`, `Attribute` with its own OID table, `Subject` and
`carried_attributes` in `bootstrap.rs`, and `subject_values` with a second OID table written as
literals, `carried_subject` and `distinguished_name` in `ca.rs`. `revocation.rs` also holds the Host
register (`Host`, `may_report`, `speaks_for`, `rebind`, `set_gateway`, `placement`), which its name
does not say.

Make one module for the subject values, with one OID table and the reading and writing of an X.509
name, and move the Host register into a module of its own behind the same `LedgerStore`. Update the
overview's Server admission paragraph.

## Acceptance criteria

- [ ] Given the crate, when it is searched for the attribute OIDs, then each is written once.
- [ ] Given the existing revocation, bootstrap and CA tests, when the change lands, then they pass
  unchanged.
- [ ] Given `docs/ARCHITECTURE.md`, when the change lands, then it names the Host register where it
  lives.

## Read first

- ADR-0021, ADR-0022, ADR-0024.
- `crates/fleet-server/src/revocation.rs`, `bootstrap.rs`, `ca.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
