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

**Idea.** Run the test in a loop under load. Note which error the open met and which bound it
ran into. Then either have the support wait for its database within that bound, or file the cause.

**From.** The implementation of revision-2026-11-12 on feat/bugfixes (2026-11-14).
