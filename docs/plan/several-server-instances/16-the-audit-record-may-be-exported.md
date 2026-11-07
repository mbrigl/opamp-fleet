# Ticket: The audit record may be exported

## What to build

`[audit.export]` names an OTLP/HTTP endpoint, off by default; the Server sends every audit entry to
it as a log record, over TLS 1.3 beyond the loopback. It does not block G-20.

## Acceptance criteria

- [ ] Given no `[audit.export]`, when the Server runs, then it opens no such connection
- [ ] Given an export endpoint, when a decision is recorded, then a log record with the entry
  reaches it

## Read first

- `draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md`
  clause 9, on the branch `feat/multi-instance-stage-3`; ADR-0045 clause 21, the open rule the
  export joins
- Golden path: the OTLP export of the Client's own telemetry

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It adds an outbound connection the Server dials.
