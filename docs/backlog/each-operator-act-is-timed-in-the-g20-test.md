# Each kind of operator act is timed in the G-20 test

**Kind:** test.

**Observed.** The G-20 load test (`crates/fleet-server/tests/g20.rs`) records only the slowest
operator act, not which act it was. At 50 000 Agents across three instances the slowest took 8.0 s.
The rollout to one Agent (`rollout_to_agent`, `crates/fleet-server/src/api.rs` near line 1157) and
the label acts are the likely ones. No criterion sets a target of one second. The spec of several
Server instances that the old ticket cited was deleted in a4161d939.

**Idea.** The test times each kind of operator act and prints the slowest of each.

**From.** Ticket 06 of `fleet-at-scale-costs`, cut to its test by the human (2026-11-15).
