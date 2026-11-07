# Ticket: An operator act answers within a second at scale

## What to build

The G-20 load test of ticket 14 sets labels on 100 Agents every five seconds, saves a
Configuration and rolls it out to a tenth of the fleet and revokes a certificate once a minute, and
reads a page of the Agents every ten seconds, each through one of three instances in turn. Every
act was answered, and the slowest took:

| Agents | On each connection | Slowest act |
|---|---|---|
| 20 000 | 1 | 1.4 s |
| 30 000 | 10 | 3.2 s |
| 50 000 | 10 | 8.0 s |

The test records only the slowest, not which act it was. The rollout, which releases to every Agent
of its Deployment, and the label acts, which take the fleet lock (several-server-instances ticket
34 left them under it), are the likely ones.

Each act names its time in the test's output, the slow ones are found and an act on one Agent
answers within a second at G-20's fleet. It does not block G-20: the load test's target asks that
every act is answered, and it is.

## Acceptance criteria

- [ ] Given the G-20 load test, when it ends, then it prints the slowest time of each kind of act
- [ ] Given 50 000 Agents across three instances, when labels are set on one Agent, then the answer
  comes within a second
- [ ] Given the integration suite and the store checks, when they run, then they pass unchanged

## Read first

- `crates/fleet-server/tests/g20.rs`, the operator acts of the load test
- `fleet.rs` `set_labels`, `rollout_to_agent` and the Deployment's rollout;
  docs/plan/fleet-at-scale-costs/03-an-act-on-one-agent-reads-one-agent.md

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
