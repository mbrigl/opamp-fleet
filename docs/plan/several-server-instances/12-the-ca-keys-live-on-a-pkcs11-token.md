# Ticket: The CA keys live on a PKCS#11 token

## What to build

The client CA's and the bootstrap CA's keys, and the audit key, may live on one PKCS#11 token named
in `server.toml`. The token signs; the Server builds and audits every signature. SoftHSM2 is the
planned token and serves one host, so several hosts need a network HSM. An existing fleet moves to
new CA keys at its ordinary renewal. Instances that share keys name the same token labels; an
instance whose audit key label differs from a live lease's fails startup.
The manual's pages on the token and the network HSM, and on moving instances that hold a CA from a
standby to side by side, land with it.

## Acceptance criteria

- [ ] Given SoftHSM2 with the client CA key, when a host renews, then the token signs and the
  signature is audited
- [ ] Given two instances whose `[audit] key_label` differ, when the second starts, then it fails
  naming the key
- [ ] Given a file CA key and a second instance, when it starts, then it still fails as ADR-0025
  clause 2 decides (`an_instance_that_must_be_alone_is_alone`, which stays)
- [ ] Given a fleet on a file CA key, when the key moves to the token, then each host moves at its
  ordinary renewal
- [ ] Given SoftHSM2, when `opamp-fleetctl pki token-init` runs, then the client CA's, the bootstrap
  CA's and the audit key are made on the token, none extractable, and no key file is written
- [ ] Given two instances whose CA keys are named by their labels on one token, when both start,
  then both serve and both sign renewals
- [ ] Given two instances on one token, when a host enrols through B's Agent plane and is approved
  on A, then it is admitted on both; the enrolment case of \`tests/instances.rs\`, which enrols
  through \`Enrolment::subscribe\` and \`submit\` today, goes through the real WebSocket
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- The drafts of stage 3, on the branch `feat/multi-instance-stage-3` (the spec's stage 3):
  `draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md`
  clauses 1 to 7, 10 to 14;
  `draft-admission-by-client-certificate-and-revocation-with-the-ca-keys-in-a-file-or-on-a-token.md`
  clauses 8 and 17; `draft-the-operator-program-opamp-fleetctl.md` clauses 5 to 8, 12, 13 and 16
  to 18;
  ADR-0025
  clauses 2 and 16 (the lease's audit key label is a migration that follows it);
  ADR-0023 clause 22
- Golden path: the local CA and its renewal tests, and `pki init` in the operator tool
- Seam: the signer port, against SoftHSM2 in the test

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It moves the CA keys, the most sensitive material the Server holds.
