# A rejection after a held approval is answered deciding once in a while

**Kind:** defect (a flaky test).

**Observed.** The test
`postgres::enrolment::tests::a_decision_on_a_request_another_instance_holds_long_is_answered_deciding`
in `crates/fleet-server/src/postgres/enrolment.rs` failed once in a full
`scripts/check-store.sh postgres` run during check-all on `feat/multi-instance-stage-3` (2026-11-08,
after the commits of stage 3, ticket 12, none of which touches the enrolment store's code). Its last
assertion, "the approval stands" (near line 1574), got `Ok(Deciding)` from `b.reject("p", 1)` where
it expects `Ok(NotWaiting)`. That came after A's approval had returned
`Ok(Approved(…))`, and after B's `approve` and `decide` on the same request had answered
`NotWaiting`. Run alone, it passed five times in a row (about 11 s each).

**Where to look.** Whether a rejection after the approval committed can still meet a lock on the
request's row, held by something other than the approval's own transaction, and be answered
`Deciding` when its wait for the lock runs out under a loaded database.

**From.** The check-all of stage 3, ticket 12 (2026-11-08).
