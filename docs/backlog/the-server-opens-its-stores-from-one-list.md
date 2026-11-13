# The Server opens its stores from one list

**Kind:** idea, structure.

**Observed.** `crates/fleet-server/src/main.rs` opens each store inline, from `Enrolment::open_on`
to `AppState::on`, and `server migrate`'s check opens stores again from a list of its
own (`open_every_store` in `crates/fleet-server/src/migrate.rs`). The two lists are kept alike by
nothing, and they differ in what they are given: the startup passes the bootstrap CA's signer, the
authorities of the revocation list and `[blobs]`, and opens the enrolment and the bootstrap record
only with `[enrolment]`.

**What the old branch had.** Revision ticket 29 of `feat/revision-2026-10-31`, open, not worked,
written against that branch's `Records::open_every_store`. Rescoped to `main`'s store opening:
one function opens every store and returns them, taking what the startup passes as parameters, so
`main.rs` builds the Server from what it returns and `server migrate` checks through the same call.
A store added to the Server is then opened by both, or by neither.
