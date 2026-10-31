# Ticket: `service install` decides its accounts once as a plan, and the tests drive that plan

## What to build

`fn install` in `crates/fleet-agent/src/main.rs` runs for about 490 lines and `main.rs` has no test
module: it sequences the account decisions, the account changes and their halt and resume, the
layout steps, the withdrawal record, the Windows rights and `config_init`. The account decision is
spread with it: the default account is decided in `run_as` and again in `manager::account_changes`,
`decide_process_account` takes two booleans one of which only returns `None`, three variants of the
process-account check exist, and reading back the registered account is split between `manager` and
`windows_config`. ADR-0037 clause 1 says the account is decided once, before anything is written.

Build one value that `run_as` produces before anything is written — the Client's and the process
account, requested against registered — which `manager` renders and an install sequence in `service`
executes, so unit tests drive the decisions without a service manager.

## Acceptance criteria

- [ ] Given a registration and a requested account change, when the plan is made, then it names
  every step the install will take, and a unit test asserts it without touching the system.
- [ ] Given `manager`, when it needs the default account, then it reads it from the plan and decides
  nothing.
- [ ] Given the service smoke test, when it runs on Linux, macOS and Windows, then it passes
  unchanged.

## Read first

- ADR-0019, ADR-0037 (clause 1 first).
- `crates/fleet-agent/src/main.rs` `install`, `service/run_as.rs`, `service/manager.rs`,
  `service/windows_config.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It reshapes the privileged install on three platforms.
