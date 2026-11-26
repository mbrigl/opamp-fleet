# Ticket: The old port is gone

## What to build

The contraction. The old Plugin factory, `SupervisorContext` and every path a kind could take
past the core are removed. The boundary becomes a test: Plugin modules get a role of their own in
the dependency-direction test, and the port's types are checked for host resources. Every
citation of CLT-supervisor-mode-and-kinds in code, tests, scripts and other ADRs moves to the
clause that now carries it, by the map in the commit that proposed the successor ADRs.

## Acceptance criteria

- [ ] Given the workspace, when the dependency-direction test runs, then
      `a_plugin_names_only_the_port` passes, and it fails for a Plugin module that names
      `std::fs`, `std::process`, `std::net`, `tokio`, `libc`, `nix` or the storage module.
- [ ] Given the port module, when `the_port_carries_no_host_resource` runs, then it passes, and it
      fails for a port type holding a path, a handle, a channel, a span or a secret.
- [ ] Given the tree, when it is searched for `CLT-supervisor-mode-and-kinds`, then only the
      superseded ADR, the index and the two successors' headers and Context name it.
- [ ] Given the full test suite and `scripts/check-docs.sh`, then both pass.

## Read first

- Touches:
  `crates/fleet-agent/src/supervisor/ports.rs`,
  `crates/fleet-agent/src/supervisor/mod.rs`,
  `crates/fleet-agent/src/supervisor/process.rs`,
  `crates/fleet-core/tests/dependency_direction.rs`,
  `docs/adr/`
- ADR: CLT-wrappers-act-through-the-core, Enforcement; DEV-ports-and-adapters;
  NUC-decisions-are-proposed-then-accepted §4, §5, §6, for what a commit that changes an ADR's
  text may carry.
- Golden path: the existing roles in the dependency-direction test.
- Seam: the dependency-direction test and a source-reading test of the port module.

## Blocked by

- docs/plan/plugins-act-through-the-core/02-collector-runs-on-the-new-port.md
- docs/plan/plugins-act-through-the-core/03-the-host-functions-a-wrapped-kind-needs.md

## Mode

with the human: moving citations inside accepted ADRs is a change to the record the human
decides, and the human flips the two successor ADRs to accepted with it.
