# A rejection after a failed approval is answered deciding for a moment

**Kind:** defect, accepted for now.

**Observed.** On PostgreSQL, `PgEnrolmentStore::approve` in
`crates/fleet-server/src/postgres/enrolment.rs` holds the request's row `FOR UPDATE` on a
connection kept in `Held`. Where a statement after that fails with `?`, the error path drops
`Held`, whose `Drop` takes the connection out of the pool and closes it without waiting for the
database to end the transaction. A rejection of the same request right after takes the row with
`FOR UPDATE NOWAIT`; until the database has noticed the closed connection it finds the row held,
and the REST API answers `409`, "another Server instance is deciding enrolment request …",
although nothing decides it. A retry succeeds. Not seen; found by reading.

**Idea.** End the transaction on each error path before answering, as `decide` and `reject` do
through `rolled_back`: a `ROLLBACK` sent and its answer awaited, the connection closed only where
that fails.

**From.** Review of the fix to the enrolment test that answered `Deciding` (feat/stabilize,
2026-11-12).
