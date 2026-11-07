# Ticket: A sensor lists the modules the overview does not name

## What to build

[`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done) asks that a change to
the structure update `docs/ARCHITECTURE.md` in the same change, and nothing reports a module the
overview does not name. `dependency_direction.rs` lists every module of the checked crates with
its role: 148 module paths at 022ebd51. Of those, 38 have neither their path
nor their last segment in backticks in the overview, among them `labels`, `uid`, `proto`, `cli`,
`telemetry`, `archive`, `client::http`, `client::protocol`, `gateway::pool`,
`supervisor::collector`, `renewal`, `signing`, `fs::agents` and `test_support`. Counted by reading
the module paths of `ROLES` (empty root paths left out, `/` read as `::`) and searching the
overview for `` `<path>` `` and `` `<last segment>` ``. Many of them are small parts the overview
names only through their parent, by its rule of one level deep.

Add a sensor that lists every module `dependency_direction.rs` gives a role to whose path or last
segment the overview does not name in backticks. It reports to the reviewer and fails nothing,
as the other sensors do
([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)).

## Acceptance criteria

- [ ] Given a module with a role that the overview does not name, when the sensor runs, then it
  lists the module.
- [ ] Given the current tree, when the sensor runs, then the 38 modules it lists today are judged
  once, each as a part the overview should name or as a part its one-level rule leaves to its
  parent; if the second kind outweighs the first and no rule (an allow-list, a depth) tells them
  apart, the ticket closes without the sensor and says why.
- [ ] Given the sensor, when it is added, then check-all and `checks.yml` run it and its
  self-test, as check 9 of `check-docs.sh` requires.

## Read first

- ADR-0002; ADR-0009 (Enforcement).
- `crates/fleet-core/tests/dependency_direction.rs`; `scripts/sensor-binding-adrs.sh` and its
  self-test as the pattern.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. Whether the noise is acceptable is the human's to judge.
