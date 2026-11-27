# Ticket: The overview names every module the dependency-direction test gives a role

## What to build

A change to the structure updates `docs/ARCHITECTURE.md`
([NUC-documents-are-fixed §4](../../adr/NUC-documents-are-fixed.md)), and no check holds it.
`crates/fleet-core/tests/dependency_direction.rs` lists every module of the workspace's library
crates with its role, and fails on a module without one. The overview is read beside that list, by
hand.

After this ticket, a check fails when a module that has a role in `ROLES` is named nowhere in
`docs/ARCHITECTURE.md`. A module counts as named where its path, or its last segment, stands in
backticks there. A module the overview leaves out on purpose stands in a list of the check's own,
each with the reason. A new module then reaches the overview, or that list, in the change that
adds it.

On `main` at 04c6301ec, 20 of the 167 modules with a role are named nowhere in the overview:
`bin::stub_agent`, `bin::stub_crasher`, `changed`, `client::http`, `client::protocol`,
`client::ws`, `clock`, `ending`, `frame`, `fs::agents`, `logging`, `naming`, `platform`,
`product`, `proto`, `random`, `service::windows`, `span`, `test_support` and `version`. The session
names each in the overview, or puts it on the list with its reason.

Evidence: the first review of PR #231 (at ff170c6de) found the overview stale: it named neither
`supervisor::port` nor `supervisor::host` nor the role `Plugin`. 16a4a2e13 fixed it by hand.

## Acceptance criteria

- [ ] Given a module with a role in `ROLES` that `docs/ARCHITECTURE.md` does not name and the
      check's list does not hold, when the check runs, then it fails and names the module.
- [ ] Given the tree after this ticket, when the check runs, then it passes, and each entry of its
      list carries a reason.
- [ ] Given an entry of the list that names a module without a role, when the check runs, then it
      fails, so the list does not outlive the module.
- [ ] Given `scripts/check-all.sh`, then it runs the check.

## Read first

- Touches: `crates/fleet-core/tests/dependency_direction.rs` or `scripts/check-project-docs.sh`,
  `docs/ARCHITECTURE.md`
- ADR: NUC-documents-are-fixed §4; DEV-ports-and-adapters, whose Enforcement names the
  dependency-direction test; NUC-checks-are-bash, where the check is a script.
- Conventions: "A structural test reads the syntax, never words" and "A test runs with the crate
  it decides" in `docs/CONVENTIONS.md`.
- Golden path: `a_role_without_a_source_file_is_reported` in `dependency_direction.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human: the human decides for each of the 20 modules whether the overview names it or
the list exempts it. DEV-maintainer-decides-agents-implement §5 caps open unattended agent work at
zero, too.
