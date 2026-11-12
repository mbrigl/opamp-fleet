# Tests fail on a host loaded beyond a second build

**Kind:** defect, check.

**Observed.** `scripts/check-all.sh` ran three times while a release build of the workspace looped
beside it. Other builds and test runs of the same session shared the host too. Tests that no
ticket of `tests-wait-for-an-event` named then failed:

- The TERM case of `scripts/test-check-all.sh` failed inside the normalize cycle: "a runner
  stopped by TERM returns before the watchdog fires", "exits 143", "and no check it started
  finishes after it".
- Audit tests failed:
  `audit_log::tests::a_held_place_counts_against_the_channel_until_it_is_written_or_given_back`
  ("lock: PoisonError"), and the audit export's
  `a_silent_receiver_blocks_no_decision_and_the_dropped_count_reaches_the_record` ("428 dropped in
  the record").
- PostgreSQL tests failed on "no answer within 15000 ms" while opening a test schema, among them
  audit, audit_prune and deployment tests.

The PostgreSQL failures also come from `check-all` itself. Once `FLEET_TEST_POSTGRES_*` name a
database, its "Build, test and lint" step runs the workspace's PostgreSQL tests at the same time as
its "PostgreSQL store" step, against the same database. Each process caps its own schemas at
eight, so together they open twice that, with their connections. Run alone, the store check passed
all 309 PostgreSQL tests.

**Idea.**

- Have `check-all` run the PostgreSQL tests in one step only, the store check, and keep the
  database's variables away from the workspace's test run.
- Give the TERM case's watchdog and the two audit tests a bound only a defect reaches, as
  `tests-wait-for-an-event` did for its cases.

**From.** The loaded runs of `tests-wait-for-an-event`'s acceptance 2 and the stack of
`multi-instance-small-corrections` on `feat/open-tickets` (2026-11-12).
