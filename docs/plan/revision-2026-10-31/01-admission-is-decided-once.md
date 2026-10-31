# Ticket: The Agent plane admits a certificate by one rule, for OpAMP and the download alike

## What to build

The Agent plane decides admission twice: once for `/v1/opamp` and once in the guard of the package
download. Each reads the certificate's facts, checks its end and its revocation, classifies its
issuer and builds the proofs by hand, and the two copies already differ: the OpAMP path refuses a
certificate whose facts do not parse, while the download path refuses one only when a revocation
list is configured, and the two ask about revocation through different calls. The download guard
also carries a field-by-field copy of the admission state.

Build one admission of a presented certificate that both middlewares call, so a host is admitted or
refused by the same admission check on both routes (each route keeps its own message), and reduce
the download guard to the shared admission plus what only the download needs (its rate bucket).

## Acceptance criteria

- [ ] Given a client certificate whose facts cannot be read, when it is presented to `/v1/opamp` and
  to the package download route, then both refuse it, by the same admission check, whether or not a
  revocation list is configured.
- [ ] Given a revoked certificate, when it is presented to either route, then both refuse it through
  the same revocation check.
- [ ] Given the existing admission tests in `crates/fleet-server/tests/mutual_tls.rs` and the
  download tests, when the change lands, then they pass unchanged, and a new test pins the case the
  two copies decided differently.

## Read first

- ADR-0021 and ADR-0012 (header, Decision, Enforcement).
- `crates/fleet-server/src/transport.rs`: `admit` and `admit_download`, `Admission`,
  `DownloadGuard`.
- [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done) on pinning behaviour before
  changing what exists.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It changes a security decision's code path.
