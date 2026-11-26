# Ticket: `collector` runs on the new port

## What to build

`collector` moves to the new port. It no longer lists its configuration directory itself: the
core hands it the written configuration entries as an input, with each entry's name, role and
digest, and the Plugin builds its process specification from them as root-relative paths. A
Collector Supervisor behaves exactly as before, the pinned Endpoint port included.

## Acceptance criteria

- [ ] Given a Collector Supervisor with configuration entries, when the Client starts, then each
      unroled entry is passed as its own `--config`, followed by the block's `args`, as before.
- [ ] Given no unroled entry, when the Client starts, then the Collector does not start and
      reports that it awaits configuration, as before.
- [ ] Given the `collector` Plugin, when it is driven by a host in memory, then a table test shows
      the process specification it requests for a given set of entries.
- [ ] Given the module of `collector`, then it names no storage, file or process module.

## Read first

- Touches:
  `crates/fleet-agent/src/supervisor/collector.rs`,
  `crates/fleet-agent/src/supervisor/ports.rs`,
  `crates/fleet-agent/src/supervisor/process.rs`,
  `crates/fleet-agent/src/storage.rs`,
  `crates/fleet-agent/tests/supervisor.rs`
- ADR: CLT-wrappers-act-through-the-core §3, §6, §10, §18.
- Golden path: `command` on the new port, from ticket 01.
- Seam: the Supervisor tests of the Client and the table test against a host in memory.

## Blocked by

- docs/plan/plugins-act-through-the-core/01-command-runs-on-the-new-port.md

## Mode

with the human: it is small, but it lands in the same stacked pull request as the tracer bullet,
and the human reviews the configuration input the tracer bullet left open.
