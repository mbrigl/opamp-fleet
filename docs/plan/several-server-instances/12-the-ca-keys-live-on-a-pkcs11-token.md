# Ticket: The CA keys live on a PKCS#11 token

## What to build

The client CA's and the bootstrap CA's keys, and the audit key, may live on one PKCS#11 token named
in `server.toml`. The token signs; the Server builds and audits every signature. SoftHSM2 is the
planned token and serves one host, so several hosts need a network HSM. An existing fleet moves to
new CA keys at its ordinary renewal. Instances that share keys name the same token labels; an
instance whose audit key label differs from a live lease's fails startup.

## Acceptance criteria

- [ ] Given SoftHSM2 with the client CA key, when a host renews, then the token signs and the
  signature is audited
- [ ] Given two instances whose `[audit] key_label` differ, when the second starts, then it fails
  naming the key
- [ ] Given a file CA key and a second instance, when it starts, then it fails as ticket 01 decides
- [ ] Given a fleet on a file CA key, when the key moves to the token, then each host moves at its
  ordinary renewal
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one
  clauses 1 to 7 and 10 to 13;
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clause
  2
- ADR-0021 clauses 8 and 17, ADR-0032 for what the operator tool makes
- Golden path: the local CA and its renewal tests
- Seam: the signer port, against SoftHSM2 in the test

## Blocked by

- docs/plan/several-server-instances/01-two-instances-share-one-database.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It moves the CA keys, the most sensitive material the Server holds.
