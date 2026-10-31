# Ticket: The heads of the audit chains are signed

## What to build

One process signs a checkpoint of every open chain's head every 60 seconds and at an orderly
shutdown, with the audit key on the token, one checkpoint per sequence number. `server audit-verify`
checks every chain against the checkpoints, so removing a whole chain is detected. Without a token,
the chains are the record. It does not block G-20.

## Acceptance criteria

- [ ] Given a token with the audit key, when 60 seconds pass, then one checkpoint is written naming
  every open chain's head
- [ ] Given a chain removed with its record, when `audit-verify` runs against the checkpoints, then
  it fails naming the chain
- [ ] Given two instances racing to sign, when both try, then one checkpoint per sequence number
  lands

## Read first

- ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token clauses 1 to 5;
  ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one
  clause 8
- Golden path: ticket 08's chains and `audit-verify`

## Blocked by

- docs/plan/several-server-instances/08-the-audit-record-is-one-chain-per-lease.md
- docs/plan/several-server-instances/12-the-ca-keys-live-on-a-pkcs11-token.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It changes how the audit record is proven.
