# Ticket: A token key signs for rcgen through one adapter

## What to build

`TokenKey` in `crates/fleet-server/src/pkcs11.rs` (near line 338) and `TokenKey<'_>` in
`crates/fleet-tools/src/token.rs` (near line 180) both implement `rcgen::PublicKeyData` (the
P-256 point, `PKCS_ECDSA_P256_SHA256`) and `SigningKey` (SHA-256, `Mechanism::Ecdsa`, DER).
ADR-0008 clause 18 says a later measurement that finds such code written twice moves it, and the
`RESTATED` list of `crates/fleet-core/tests/pkcs11_written_once.rs` does not catch this one.
The adapter moves into `fleet_core::pkcs11`; how each finds or makes its key stays in each end.

## Acceptance criteria

- [ ] Given the change, when the Server and the tool sign a certificate with a key on SoftHSM2,
  then both go through the one adapter of `fleet_core::pkcs11`.
- [ ] Given `pkcs11_written_once.rs`, when a third copy of the adapter is written, then it fails.

## Read first

- Touches: `crates/fleet-core/src/pkcs11.rs`, `crates/fleet-server/src/pkcs11.rs`,
  `crates/fleet-tools/src/token.rs`, `crates/fleet-core/tests/pkcs11_written_once.rs`
- ADR-0008 clauses 17 and 18, ADR-0043 clause 5.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
