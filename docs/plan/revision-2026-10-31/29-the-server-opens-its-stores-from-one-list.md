# Ticket: The Server's startup and `server migrate` open the stores from one list

## What to build

`main.rs` opens each store inline, and `Records::open_every_store` opens the same stores again for
`server migrate`'s check. The two lists are kept alike only by a sentence in the architecture
overview, and they already differ in what they are given: startup passes the bootstrap CA's
signer, the authorities of the revocation list and `[blobs]`, and opens the enrolment and the
bootstrap record only with `[enrolment]`.

Have one function open every store and return them, taking what startup passes as parameters, so
`main.rs` builds the Server from what it returns and `server migrate` checks through the same call.
A store added to the Server is then opened by both, or by neither.

## Acceptance criteria

- [ ] Given the Server's startup, when it opens its stores, then it does so through the function
  `server migrate`'s check calls, and `main.rs` names no store constructor of its own.
- [ ] Given a Server with `[blobs.s3]`, when the function opens the artifacts, then it opens the
  bucket `[blobs]` names, not `data_dir/blobs`.
- [ ] Given the existing startup, migrate and store tests, when the change lands, then they pass
  unchanged.

## Read first

- ADR-0029 (header, Decision, Enforcement), clause 26 on what the migration carries.
- `crates/fleet-server/src/main.rs`, the opening of the stores; `crates/fleet-server/src/lib.rs`
  `Records::open_every_store`; `crates/fleet-server/src/migrate.rs` `check_opens`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It changes the Server's startup.
