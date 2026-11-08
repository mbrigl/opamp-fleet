# Nothing is pruned before its audit.pruned is written deadlocks once in a while

**Kind:** defect (a flaky test).

**Observed.** The test
`postgres::audit_prune::tests::nothing_is_pruned_before_its_audit_pruned_is_written` in
`crates/fleet-server/src/postgres/audit_prune.rs` failed once in a full
`scripts/check-store.sh postgres` run during check-all on `feat/multi-instance-stage-3`
(2026-11-08, at the commit of stage 3, ticket 16). It panicked in `TestDatabase::admin`
(`crates/fleet-server/src/postgres/mod.rs`, "run on the test database"), on a statement of the
test's own:

```text
ERROR: deadlock detected (40P01): Process 2869092 waits for AccessExclusiveLock on relation
50026527 of database 16384; blocked by process 2869082.
Process 2869082 waits for AccessShareLock on relation 50026111 of database 16384; blocked by
process 2869092.
```

The next check-all on the same tree passed. Ticket 16 touches neither this test nor
`TestDatabase`; the failure is unrelated to it.

**Where to look.** The test adds a trigger on `audit_entries` with `TestDatabase::refuse` and
drops it with `mend` (`DROP TRIGGER`, an AccessExclusiveLock), while the instance's audit writer
and its lease renewals still run on their own connections. Whether `mend` races a write of the
writer, and whether the test should stop the writer, or retry the statement on 40P01, before it
drops the trigger.

**From.** The check-all of stage 3, ticket 16 (2026-11-08).
