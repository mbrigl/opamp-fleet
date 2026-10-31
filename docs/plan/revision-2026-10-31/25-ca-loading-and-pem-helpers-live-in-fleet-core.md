# Ticket: The Server and the operator tool load a CA and read a PEM file through `fleet-core`

## What to build

Ordinary work under ADR-0008 clause 16, with no new decision: loading a CA's certificate and key
into an issuer is written three times (`ClientCa::from_config` and `BootstrapCa::from_config` in
`crates/fleet-server/src/ca.rs`, `load_authority` in `crates/fleet-tools/src/pki.rs`), "the first
certificate of a PEM file" five times, and the client-auth leaf settings (no CA, digital signature,
a random serial, not-before set back by the clock skew) twice, while `fleet_core::x509::issue` holds
only the serial and the clock skew.

Grow `fleet_core::x509::issue` by an authority loaded from a PEM pair, the first certificate of a
PEM file, and the leaf parameters by role, and have `ca.rs` and the tool's `pki.rs` use them. The
reading of a certificate's end stays where it is until
docs/plan/revision-2026-10-31/03-certificate-reading-lives-in-one-place.md is decided.

## Acceptance criteria

- [ ] Given the Server and the tool, when either loads a CA's certificate and key, then both go
  through one `fleet-core` function, tested once.
- [ ] Given a PEM file with several certificates, when any caller asks for the first, then one
  function answers.
- [ ] Given the existing CA, bootstrap and `pki init` tests, when the change lands, then they pass
  unchanged.

## Read first

- ADR-0008 clause 16, ADR-0021, ADR-0022, ADR-0032 (the Server makes no CA).
- `crates/fleet-core/src/x509/issue.rs`, `crates/fleet-server/src/ca.rs`,
  `crates/fleet-tools/src/pki.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
