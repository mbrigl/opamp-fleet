# Ticket: telegraf runs on the Plugin port

## What to build

The tracer bullet. `feat/telegraf` is rebased onto `main`, and the `telegraf` kind is ported to
the Plugin port of CLT-wrappers-act-through-the-core. An operator runs Telegraf under a Client
built from that branch exactly as the kind's proposed ADR says:

- its block is `type` and `name` alone;
- its configuration is applied by a reload signal;
- its version is read from its probe.

The kind does no I/O of its own. Its proposed ADRs, `docs/adr/CLT-telegraf-kind.md` and
`docs/adr/SRV-kinds-name-their-ui-grammar.md`, cite the record in force.

The branch then lands by a pull request of its own. What the port asked of a wrapped kind that its
ADR did not foresee is written into the hand-off, so that `glpi` and `icinga2` follow a proven
path.

## Acceptance criteria

- [ ] Given `feat/telegraf` rebased onto `main`, when `scripts/check-all.sh` runs, then it is green
  with the `telegraf` feature on and off.
- [ ] Given a Telegraf Supervisor of two lines, when the Server delivers a configuration, then the
  stub program receives it by its reload signal, and the Agent reports it applied. The
  `telegraf_supervisor` tests of the branch decide it.
- [ ] Given the kind's module, when the structural test runs, then the module has the role
  `Plugin` and names only the port.
- [ ] Given the branch's proposed ADRs, when they are searched, then no citation names
  the deleted ADR on Supervisor Mode and its kinds or another ADR the normalization of 2026-11-28
  deleted. Each moved citation names the clause that now carries it.

## Read first

- Touches: `crates/fleet-agent/src/supervisor/telegraf.rs`,
  `crates/fleet-agent/src/supervisor/mod.rs`, `crates/fleet-agent/src/supervisor/block.rs`,
  `crates/fleet-agent/tests/telegraf_supervisor.rs`,
  `crates/fleet-core/tests/dependency_direction.rs`, `docs/adr/CLT-telegraf-kind.md`,
  `docs/adr/SRV-kinds-name-their-ui-grammar.md`, `docs/adr/README.md`.
- The spec beside this ticket.
- The clause map of the deleted ADR on Supervisor Mode and its kinds: the body of commit b4eb61fce.
  A copy is in `.scratch/supervisor-mode-and-kinds.map` of the main checkout.
- The golden path: the `command` kind on `main` (`crates/fleet-agent/src/supervisor/command.rs`)
  and the test Plugin of `crates/fleet-agent/tests/wrapped_supervisors.rs`, whose manifest names a
  reload signal.
- The seam: `crates/fleet-agent/tests/telegraf_supervisor.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human. DEV-maintainer-decides-agents-implement §5 keeps every part of the system with
the human, and the ticket rewrites two proposed ADRs. Stop and hand off if any of these happens:

- the kind needs a host function the vocabulary of CLT-wrappers-act-through-the-core §4 lacks;
- the rebase leaves a conflict outside the files listed above;
- the checks stay red after three attempts at the same failure.
