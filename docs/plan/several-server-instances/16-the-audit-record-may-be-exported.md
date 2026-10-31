# Ticket: The audit record may be exported

## What to build

`[audit.export]` names an OTLP/HTTP endpoint, off by default; the Server sends every audit entry to
it as a log record, over TLS 1.3 beyond the loopback. It does not block G-20.

## Acceptance criteria

- [ ] Given no `[audit.export]`, when the Server runs, then it opens no such connection
- [ ] Given an export endpoint, when a decision is recorded, then a log record with the entry
  reaches it

## Read first

- ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one
  clause 9; ADR-draft-tls-1-3-two-planes-and-bounded-input clause 21
- Golden path: the OTLP export of the Client's own telemetry

## Blocked by

- docs/plan/several-server-instances/08-the-audit-record-is-one-chain-per-lease.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It adds an outbound connection the Server dials.
