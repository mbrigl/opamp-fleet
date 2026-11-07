# Ticket: A key rejected in a restore stays refused beyond its window

## What to build

A database restored from before an approval no longer knows it. A request's `id` is its key's
fingerprint, so a host whose request was approved since the backup and that sends the same request
again finds it pending, and approved again it signs a certificate for that key that no revocation
names. The manual's restore procedure therefore has the operator reject those ids, named by an
`enrolment.rejected` or as the `request` of an `issuance.signed`, in every open window as long as
their hosts can still send them. The rejection does not last: `Enrolment::close` and `expire` in
`crates/fleet-server/src/enrolment.rs` drop every request the queue holds, the rejected ones
included, and `close_window` and `end_window` in `crates/fleet-server/src/postgres/enrolment.rs`
delete them from the store, so the same request sent in the next window waits again.
`Enrolment::reject` answers `DecisionError::NotFound`, a `404`, for an `id` the queue does not
hold, so no request is rejected before it arrives.

A request rejected in a restore cannot be approved again: either a rejected key stays refused
beyond its window, or `reject` accepts an `id` not yet queued and that refusal holds when the
request arrives. The shape, and how long such a refusal lasts and how it is lifted, are the
human's. With either, the manual's restore procedure keeps the window closed until those ids are
rejected, and opens it after.

ADR-0022 clause 16 says that a rejected request stays rejected for its key, and the manual's
Client chapter says what the code does, that it stays rejected until the window closes.

## Acceptance criteria

Each criterion is written as a failing test before the code that passes it.

- [ ] Given a request rejected in one window, when the window closes or runs out and the same
  request is sent in the next window, then it is answered as rejected and is not listed as
  pending, on the in-memory store and on PostgreSQL.
- [ ] Given an `id` the queue does not hold, when it is rejected, then the answer and what later
  happens to a request with that `id` are as the shape the human chose says, on both stores.
- [ ] Given two instances on one database, when one rejects and the window closes and opens
  again, then the other instance refuses the re-sent request too.
- [ ] Given the manual's database-restore procedure, when the change lands, then it keeps the
  enrolment window closed until the ids decided since the backup are rejected, and no longer has
  the operator reject them in every later window.
- [ ] Given the existing enrolment tests and the store checks, when the change lands, then they
  pass, or a change to one is named in the hand-off with its reason.

## Read first

- ADR-0022 clauses 9, 10 and 16 (the queue keyed by the key, the `404` of an unknown id, a
  rejected request staying rejected).
- The manual's database restore, `docs/manual/server.md`, from "A database restored from before a
  revocation admits the revoked certificate again", the bullet of the requests decided since the
  backup; and its enrolment window section.
- `enrolment.rs`: `submit`, `reject`, `close`, `expire`; `postgres/enrolment.rs`: `take`,
  `decide`, `close_window`, `end_window`.

## Blocked by

- None (can start immediately)

## Mode

with the human: it changes the queue behaviour ADR-0022 decides, so a `docs(adr)` commit comes
first and the human decides the shape before any code.

It blocks G-20 and the pull request of stage 2 of docs/plan/several-server-instances/spec.md: it
concerns admission, a key approved since a backup being signed again after a restore.
