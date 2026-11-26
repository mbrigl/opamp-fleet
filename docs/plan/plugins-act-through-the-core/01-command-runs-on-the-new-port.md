# Ticket: `command` runs on the new port

## What to build

The tracer bullet. The new port exists beside the old one: manifest, inputs, the host trait and
the asynchronous handler, with an executor in the core that carries out the minimum host
functions over the existing `Runner`: start, restart and reload the Managed Process, report
health, answer an apply, an install and an uninstall. `command` moves to the new port. An
operator's `command` Supervisor behaves exactly as before; `collector` still runs on the old port.

## Acceptance criteria

- [ ] Given a `command` block, when the Client starts, then the Managed Process starts, is
      watchdog-restarted, stopped, health-gated and swapped as before, and every existing test of
      `command` passes unchanged in its assertion.
- [ ] Given the `command` Plugin, when it is driven by a host in memory, then a table test shows
      the host functions it calls for start, apply, restart, install and uninstall.
- [ ] Given a handler call in progress, when a second input arrives, then the core holds it until
      the first call returns.
- [ ] Given the Client shuts down during a handler call, when the shutdown fires, then the core
      cancels the handler and stops the Managed Process within the stop timeout.
- [ ] Given the port's types, then none of them is a path, handle, channel, span or secret.

## Read first

- Touches:
  `crates/fleet-agent/src/supervisor/ports.rs`,
  `crates/fleet-agent/src/supervisor/process.rs`,
  `crates/fleet-agent/src/supervisor/command.rs`,
  `crates/fleet-agent/src/supervisor/mod.rs`,
  `crates/fleet-agent/tests/supervisor_process.rs`,
  `crates/fleet-agent/tests/supervisor.rs`
- ADR: CLT-wrappers-act-through-the-core §1, §2, §4, §5, §6, §7, and the Enforcement.
- Glossary: Plugin, Supervisor, Managed Process, Second service.
- Golden path: the `Runner` as `command` wires it today, and `start_supervisor` in the supervisor
  module.
- Seam: the process tests (commands in, events out) and a new table test against a host in memory.

## Blocked by

- None (can start immediately)

## Mode

with the human: the tracer bullet decides the shape of the port and of the bridge to the
`Runner`, which every later ticket and every wrapped kind builds on.
