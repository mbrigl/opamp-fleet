# Each kind of operator act is timed in the G-20 test

**Kind:** test.

**Observed.** The G-20 load test (`crates/fleet-server/tests/g20.rs`) sets labels on 100 Agents
every five seconds. Once a minute it saves a Configuration, rolls it out to a tenth of the fleet and
revokes a certificate. Every ten seconds it reads a page of the Agents. Each act goes through one of
three instances in turn. Every act was answered, and the slowest took:

| Agents | On each connection | Slowest act |
|---|---|---|
| 20 000 | 1 | 1.4 s |
| 30 000 | 10 | 3.2 s |
| 50 000 | 10 | 8.0 s |

The test records only the slowest act, not which act it was. The rollout to one Agent
(`rollout_to_agent`, `crates/fleet-server/src/api.rs` near line 1157) and the label acts are the
likely ones. No criterion sets a target of one second. The spec of several Server instances that
the old ticket cited was deleted in a4161d939.

**Idea.** The test times each kind of operator act and prints the slowest of each.

**From.** Ticket 06 of `fleet-at-scale-costs`, cut to its test by the human (2026-11-15).
