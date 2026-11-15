# A pin is counted by index

**Kind:** performance, schema.

**Observed.** The pin counts under a Package's, Deployment's or Configuration's row lock count the
Agents with no index to serve them (`hold_unassigned` in
`crates/fleet-server/src/postgres/packages.rs` and its siblings). `postgres/schema.rs` (near lines
650 to 652) indexes `agents` by `host` and by Client Agent, but not `agents.deployment`, the package
columns or `agent_assignment_configurations(configuration)`. The other half of the old ticket is
done: the bound on the Agents a fleet stores is read from the counter row (`postgres/agents.rs`,
line 1639).

**Idea.** A migration adds the three indexes. ADR-0023 clause 22 allows a migration that adds
indexes. The work is S. It blocks no stage: the G-20 load test met its target at 100 000 Agents
without it.

**From.** Ticket 02 of `fleet-at-scale-costs`, moved to the backlog by the human (2026-11-15).
