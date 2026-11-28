# Ticket: glpi runs on the Plugin port

## What to build

`feat/glpi` is rebased onto `main`, and the `glpi` kind is ported to the Plugin port along the
path ticket 01 proved for `telegraf`. An operator runs the GLPI Agent under a Client built from
that branch as `docs/adr/CLT-glpi-agent-kind.md` proposes. The kind does no I/O of its own, and its
proposed ADR cites the record in force. The fetch and repack in the operator tool and the artifact
document are rebased unchanged. The branch lands by a pull request of its own.

## Acceptance criteria

- [ ] Given `feat/glpi` rebased onto `main`, when `scripts/check-all.sh` runs, then it is green
  with the `glpi` feature on and off.
- [ ] Given a GLPI Agent Supervisor, when the Server delivers a configuration, then the Agent
  applies it and reports it as its ADR says. The `glpi_supervisor` tests of the branch decide it,
  rewritten only where they reached into the old port.
- [ ] Given the kind's module, when the structural test runs, then the module has the role
  `Plugin` and names only the port.
- [ ] Given `docs/adr/CLT-glpi-agent-kind.md`, when it is searched, then no citation names an ADR
  the normalization of 2026-11-28 deleted. Each moved citation names the clause that now carries it.

## Read first

- Touches: `crates/fleet-agent/src/supervisor/glpi.rs`, `crates/fleet-agent/src/supervisor/mod.rs`,
  `crates/fleet-agent/tests/glpi_supervisor.rs`, `crates/fleet-core/tests/dependency_direction.rs`,
  `docs/adr/CLT-glpi-agent-kind.md`, `docs/adr/README.md`.
- The spec beside this ticket, and the hand-off of ticket 01
  (`.scratch/kinds-on-the-plugin-port-01-*.md` in the main checkout).
- The golden path: the `telegraf` kind as ticket 01 left it on `feat/telegraf`.
- The seam: `crates/fleet-agent/tests/glpi_supervisor.rs`.

## Blocked by

- docs/plan/kinds-on-the-plugin-port/01-telegraf-runs-on-the-plugin-port.md

## Mode

with the human, for the reasons and with the stops of ticket 01.
