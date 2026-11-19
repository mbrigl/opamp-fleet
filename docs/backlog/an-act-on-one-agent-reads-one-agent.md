# An act on one Agent reads one Agent

**Kind:** performance.

**Observed.** On PostgreSQL, operator acts on one Agent or one group read the whole fleet:

- `deployment_reach` (`crates/fleet-server/src/fleet.rs`, near line 2078) reads every Agent through
  `read_fleet` (near lines 1156 to 1163), on every `GET /deployments` and after every write that
  answers with its reach;
- `put_entry_record` (`packages.rs`, near lines 945 to 959) holds the Package writes of one
  instance under the `writes` mutex across an upload's check that its blob exists.

**Idea.** Each act reads the Agents it concerns, outside the fleet lock where it can, and a Package
write does not wait behind the blob check. The old ticket's first criterion cannot hold for
`GET /deployments`: it counts the reach of every Deployment, which concerns every Agent. It blocks
no stage: the G-20 load test met its target at 100 000 Agents without it.

**Not now.** The human deferred it on 2026-11-19: no criterion asks for it, and the G-20 load test
meets its target without it. It comes back when the slowest operator act of each kind, which the
G-20 load test prints since 06892c068, or another measurement shows it.

**From.** Ticket 03 of `fleet-at-scale-costs`, moved to the backlog by the human (2026-11-15).
