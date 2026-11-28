# Ticket: icinga2 runs on the Plugin port, under a registered process account too

## What to build

`feat/icinga2` is rebased onto `main`, and the `icinga2` kind is rewritten against the Plugin
port. Today the kind acts past the core in four ways:

- it reads, writes, copies, renames and removes files itself;
- it starts its subcommands (`pki`, `node setup`, the configuration check) itself;
- it asks the resolver for the host's name and `id` for its account;
- it intercepts the command channel in tasks of its own.

On the port, each becomes one of three things:

- a call of a host function within the kind's roots: `program`, `config`, `data` and `state`;
- a program run from its `program` root through the core, and so through the second service where
  a process account is registered;
- a host fact the core answers.

The kind's refusal of a registered process account goes, in the code and in
`docs/adr/CLT-icinga2-repacked-kind.md`. An operator runs Icinga 2 in its Agent role, enrolled with
its master, with a process account or without one. The supervisor-set handling the branch added and
the repacking in the operator tool are rebased. The branch lands by a pull request of its own.

## Acceptance criteria

- [ ] Given `feat/icinga2` rebased onto `main`, when `scripts/check-all.sh` runs, then it is green
  with the `icinga2` feature on and off.
- [ ] Given an Icinga 2 Supervisor without a process account, when it starts and the Server
  delivers a configuration, then the stub program enrols, checks and runs the configuration as the
  branch's `icinga2_supervisor` tests decide. Those tests are rewritten only where they reached into
  the old port.
- [ ] Given a registered process account, when an Icinga 2 Supervisor starts, then the block is not
  refused. Every program the kind runs, its subcommands included, is started by the second service
  under that account.
- [ ] Given the kind's module, when the structural test runs, then the module has the role
  `Plugin` and names only the port: no `std::fs`, no `std::process`, no `libc`, no `crate::storage`.
- [ ] Given `docs/adr/CLT-icinga2-repacked-kind.md`, when it is read, then it no longer refuses a
  process account. No citation names an ADR the normalization of 2026-11-28 deleted.

## Read first

- Touches: `crates/fleet-agent/src/supervisor/icinga2.rs`,
  `crates/fleet-agent/src/supervisor/mod.rs`, `crates/fleet-agent/src/reconfigure.rs`,
  `crates/fleet-agent/src/bin/stub_icinga2.rs`, `crates/fleet-agent/tests/icinga2_supervisor.rs`,
  `crates/fleet-core/tests/dependency_direction.rs`, `docs/adr/CLT-icinga2-repacked-kind.md`,
  `docs/adr/README.md`.
- The spec beside this ticket, and the hand-offs of tickets 01 and 02.
- CLT-wrappers-act-through-the-core §1, §3, §4 and §13, and its Context on `icinga2`;
  CLT-accounts-service-runs-under §18.
- The golden path: `a_program_a_plugin_runs_goes_through_the_second_service` in
  `crates/fleet-agent/src/supervisor/host.rs`. For steps in a row, the fake host the tests of a
  Plugin on the port use.
- The seam: `crates/fleet-agent/tests/icinga2_supervisor.rs`.

## Blocked by

- docs/plan/kinds-on-the-plugin-port/02-glpi-runs-on-the-plugin-port.md

## Mode

with the human, with the stops of ticket 01. One stop is expected here: a step of the enrolment or
the take-over that no host function carries. The session then writes into this ticket which step
it is, what it needs and why, commits it on the branch and hands off. The superseding ADR goes
through `propose-adr` with the human.
