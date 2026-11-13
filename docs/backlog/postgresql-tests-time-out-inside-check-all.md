# PostgreSQL tests time out inside check-all

**Kind:** defect, check.

**Observed.** With `FLEET_TEST_POSTGRES_*` naming a database, the "PostgreSQL store" step of
`scripts/check-all.sh` still fails on about nine tests with "no answer within 15000 ms" while
opening a test schema, all in tests that open several databases (lease, ledger, enrolment). The
workspace's test run no longer runs them at the same time. Run alone,
`scripts/check-store.sh postgres` passes them, also beside 48 CPU burners and disk-fsync load. The
load average was 47 during the failing run; whether another session used the host or the database
at that time was not checked. The cause is not known.

Seen beside it, not failing yet:

- With `FLEET_TEST_S3_*` set, the S3 tests run three times at once: in the workspace's test run
  (`scripts/check-rust.sh`) and in both store steps.
- Other `audit_log` tests that fill the channel while holding the store lock, such as
  `a_throttle_refusal_that_finds_the_channel_full_is_counted_unrecorded`, assume the writer has
  already taken its entry, as
  `a_held_place_counts_against_the_channel_until_it_is_written_or_given_back` did before it
  waited for that.
- The audit export's
  `a_silent_receiver_blocks_no_decision_and_the_dropped_count_reaches_the_record` sends its SIGTERM
  once the record holds every entry; a SIGTERM between the last batch's write and its hand-off to
  the export would put that batch's drops after the count.

**Idea.** Find what the store step shares with the steps beside it: run it in check-all with the
other steps stubbed, then with each one back, and log the database's connections and locks at a
timeout. Keep the S3 variables away from the workspace run as the PostgreSQL ones are, by their
prefix rather than by a list of names, so a variable added later is kept away too, with a case of
`scripts/test-check-all.sh` that says so (the design revision of 2026-11-12).

**From.** The fix of tests-fail-on-a-host-loaded-beyond-a-second-build on `feat/stabilize` and its
review (2026-11-12).
