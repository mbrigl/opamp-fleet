# Ticket: Reading and issuing certificates is written once, for the Server and the operator tool

## What to build

`propose-adr` is needed before this is implemented. ADR-0032 clause 6 decides that each program
reads the certificate itself, and its alternatives turned down "the whole reading of a certificate's
end in fleet-core" because it would give the internal crate its first `x509-parser` and `time`
dependencies. `fleet-core` carries both today, for `x509::read` and `csr`, so the reason has lapsed,
and the copy it allowed is verbatim: `Ending` and `read_ending` in `crates/fleet-server/src/pki.rs`
and `crates/fleet-tools/src/pki.rs`, and the status table formatted in both `main.rs` files.

Beside it, under ADR-0008 clause 16 and without a new decision: loading a CA's certificate and key
into an issuer is written three times (`ClientCa::from_config`, `BootstrapCa::from_config`, the
tool's `load_authority`), "the first certificate of a PEM file" five times, and the client-auth leaf
settings twice, while `fleet_core::x509::issue` holds only the serial and the clock skew.

Propose the ADR that supersedes ADR-0032's clause on reading the end; once the human has reviewed
it, move the ending reading and the PEM and issuer helpers into `fleet-core` and delete the copies.

## Acceptance criteria

- [ ] Given the proposed ADR, when the human has reviewed it, then its commits are ADRs only and the
  implementation follows on the same branch ([`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules), rules
  3 and 4).
- [ ] Given `server pki status` and `opamp-fleetctl pki status` over the same certificate, when both
  run, then they print the same verdict from one function.
- [ ] Given the CA loading of the Server and the tool, when either loads a cert and key pair, then
  both go through one `fleet-core` function, tested once.

## Read first

- ADR-0032 clause 6 and its Alternatives considered; ADR-0008 clause 16.
- The `propose-adr` skill.
- `crates/fleet-server/src/ca.rs`, `crates/fleet-server/src/pki.rs`,
  `crates/fleet-tools/src/pki.rs`, `crates/fleet-core/src/x509/`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It needs an ADR the human decides.
