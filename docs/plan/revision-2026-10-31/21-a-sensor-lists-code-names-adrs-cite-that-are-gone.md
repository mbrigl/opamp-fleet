# Ticket: A sensor lists the code names an ADR in force cites that the tree no longer has

## What to build

A drafted restatement of ADR-0012 kept describing the constant `SHUTDOWN_DRAIN` after the code on
its branch had removed it; a fresh review caught it. An ADR in force that names a constant, a
function or a type the tree no longer has is either stale or worked around, and no check sees it. On
`main` today the constant exists.

Add a sensor that reads the identifiers in backticks of each ADR in force and of each draft — those
shaped like Rust constants, types or paths (`SHUTDOWN_DRAIN`, `Spawner::Channel`) — and lists every
one no file under `crates/` defines. It reports to the reviewer and fails nothing
([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)), as the other sensors.

## Acceptance criteria

- [ ] Given an ADR in force naming a constant no crate defines, when the sensor runs, then it lists
  the ADR, the line and the name.
- [ ] Given the current tree, when the sensor runs, then what it lists is reviewed once and its
  noise judged; if the noise outweighs the finds, the ticket closes without the sensor and says why.
- [ ] Given the sensor, when it is added, then check-all and `checks.yml` run it and its self-test.

## Read first

- ADR-0002, `scripts/sensor-binding-adrs.sh` and its self-test as the pattern.
- `scripts/lib/adr.sh`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. Whether the noise is acceptable is the human's to judge.
