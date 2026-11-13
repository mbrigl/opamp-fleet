# Ticket: The PKCS#11 primitives are written once

## What to build

The Server's token adapter (`crates/fleet-server/src/pkcs11.rs`) and the operator program's
(`crates/fleet-tools/src/token.rs`) each write the P-256 parameters (`P256_PARAMS`), the text of a
`cryptoki` error, the conversion of a raw signature to DER (`der_signature`), and the slot lookup
and login. A correction to one, such as the DER conversion, has to be made twice, and nothing
compares them. `fleet-tools` depends on `fleet-server` only for its tests, so it cannot take the
Server's today.

Write them once. Where they live is the first step and the human's: `fleet-core` as an adapter
beside `pkcs12::file`, which needs `cryptoki` there; or a sixth crate, which ADR-0008's five
crates do not allow without a superseding ADR through `propose-adr`; or `fleet-tools` depending
on `fleet-server`'s library, which ADR-0008 does not foresee for the operator tool.

## Acceptance criteria

- [ ] Given the two token adapters, when the change lands, then the P-256 parameters, the DER
  conversion, the error text and the login are written once and both programs call them.
- [ ] Given `tests/pkcs11.rs` and the operator program's token tests on SoftHSM2, when they run,
  then they pass.

## Read first

- Touches: `crates/fleet-server/src/pkcs11.rs`, `crates/fleet-tools/src/token.rs`,
  `crates/fleet-core/`, `crates/fleet-core/tests/dependency_direction.rs`
- ADR-0008, ADR-0009, ADR-0042, ADR-0043.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. Where the code goes may need a superseding ADR, which the human
decides.
