# Ticket: The privileged enrolment takes what the install wrote, once

## What to build

`enrol::run(config, bootstrap, written, roots)` (`crates/fleet-agent/src/service/enrol.rs`) is
called with `bootstrap` taken from `written.bootstrap` (`crates/fleet-agent/src/main.rs`), so one
value is passed twice. `Written.copies` (`crates/fleet-agent/src/config_init.rs`) holds the path
of `answered.ca` a second time, collected apart from it, and `main.rs` reads it in four places.
The review of 3e16f944c named both and left them.

Have `enrol::run` take the bootstrap from `Written`, and derive the copies from the answers
instead of keeping them as a field.

## Acceptance criteria

- [ ] Given an install that answered the questionnaire, `--endpoint`, or neither, when it enrols,
  then it connects where ADR-0044 says, decided by the existing tests.
- [ ] Given `enrol::run` and `Written`, when the change lands, then no value is passed or kept
  twice.

## Read first

- Touches: `crates/fleet-agent/src/service/enrol.rs`, `crates/fleet-agent/src/config_init.rs`,
  `crates/fleet-agent/src/main.rs`
- ADR-0044, ADR-0027 clause 18.
- `docs/backlog/the-install-is-a-plan-the-tests-drive.md` on the branch `docs/backlog-revision`
  reshapes the same install; ticket 01 of this directory touches `enrol` and `config_init` too.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
