# A re-key test fails to open its database once

**Kind:** defect, a flaky test.

**Observed.** The test failed once on 2026-11-14:
`a_rekey_a_read_again_finds_before_the_report_holds_it_keeps_the_agent`
(`crates/fleet-server/src/fleet.rs`, against PostgreSQL). It ran against a local PostgreSQL 15,
and the failure was at "open the test database" in the PostgreSQL test support
(`crates/fleet-server/src/postgres/mod.rs`). Before that, nothing of the test itself had run. The
test passed in three runs on its own and in a full rerun on the same code. Nothing yet shows
whether the open hit the support's bound under load, as the PostgreSQL timeouts of check-all did
before the store steps ran last, or hit something of its own.

**Not reproduced (2026-11-14).** Against the development stack's PostgreSQL 18.6
(`max_connections` 100), the test passed 150 times in a row: 60 runs beside two runs of the whole
library's tests on the same database, then 90 beside three runs of the library's and the
`instances` integration tests at once. Every one of those suites passed too. Ruled out by
reading: a collision of the exclusive lock, whose key comes from the test's random schema
(`EXCLUSIVE_LOCK`), and a wait for a free test schema, which fails with its own message naming
`SCHEMAS_AT_ONCE`. Left: the connection itself, refused or cut (the database out of connections,
or the network to it), or the open's bound under a heavier load than this. The failure ran
against a PostgreSQL 15, not this one, and its error text was not kept.

**Idea.** On the next failure, keep the whole panic message: `open` names the error it met. Then
either have the support wait for its database within that bound, or file the cause. Until then
there is nothing to fix.

**From.** The implementation of revision-2026-11-12 on feat/bugfixes (2026-11-14).
