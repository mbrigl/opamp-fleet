# Ticket: When a certificate ends is read once, for the Server and the operator tool

## What to build

`propose-adr` is needed before this is implemented. ADR-0032 clause 6 decides that each program
reads the certificate itself, and its alternatives turned down "the whole reading of a certificate's
end in fleet-core" because it would give the internal crate its first `x509-parser` and `time`
dependencies. `fleet-core` carries both today, for `x509::read` and `csr`, so the reason has lapsed,
and the copy it allowed is verbatim: `Ending` and `read_ending` in `crates/fleet-server/src/pki.rs`
and `crates/fleet-tools/src/pki.rs`, and the status table formatted in both `main.rs` files.

Propose the ADR that supersedes ADR-0032's clause on reading the end; once the human has reviewed
it, move the reading of the end and the status line into `fleet-core` and delete the copies. The CA
loading and PEM helpers need no decision and are
docs/plan/revision-2026-10-31/25-ca-loading-and-pem-helpers-live-in-fleet-core.md.

## Acceptance criteria

- [ ] Given the proposed ADR, when the human has reviewed it, then its commits are ADRs only and the
  implementation follows on the same branch ([`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules), rules
  3 and 4).
- [ ] Given `server pki status` and `opamp-fleetctl pki status` over the same certificate, when both
  run, then they print the same verdict from one function.

## Read first

- ADR-0032 clause 6 and its Alternatives considered; ADR-0008 clause 16.
- The `propose-adr` skill.
- `crates/fleet-server/src/pki.rs`, `crates/fleet-tools/src/pki.rs`,
  `crates/fleet-core/src/ending.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It needs an ADR the human decides.
