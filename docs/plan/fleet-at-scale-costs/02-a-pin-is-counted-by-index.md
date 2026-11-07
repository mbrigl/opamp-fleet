# Ticket: A pin is counted by index

## What to build

The pin counts under a Package's, Deployment's or Configuration's row lock count the Agents with
no index to serve them (`hold_unassigned` in `postgres/packages.rs` and its siblings; no index on
`agents.deployment`, the package columns or `agent_assignment_configurations(configuration)`).

The pin counts are served by indexes, added by a migration that follows the expand-then-contract
rule. The bound on the Agents a fleet stores is read from the counter row of migration 14, not
counted. It blocks no stage: the G-20 load test met its target at 100 000 Agents without it.

## Acceptance criteria

- [ ] Given 100 000 stored Agents, when a Package entry is written, then its pin count reads by
  index (`EXPLAIN` shows no sequential scan of `agents`)
- [ ] Given the store checks, when they run, then they pass unchanged

## Read first

- ADR-0023 clauses 2 and 22 on the schema and its migrations; ADR-0025 clause 19
- `postgres/packages.rs` `hold_unassigned`; `postgres/schema.rs`

## Blocked by

- docs/plan/fleet-at-scale-costs/01-a-notice-reads-what-it-names.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
