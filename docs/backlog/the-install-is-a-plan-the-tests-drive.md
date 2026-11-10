# The install is a plan the tests drive

**Kind:** idea, structure.

**Observed.** `fn install` in `crates/fleet-agent/src/main.rs` runs from about line 314 to line 797
on `main`, and `main.rs` has no test module. It sequences the account decisions, the account
changes with their halt and resume, the layout steps, the withdrawal record, the Windows rights and
`config_init`. The account decision is spread with it: the default account is decided in `run_as`
and again in `manager::account_changes`, and reading back the registered account is split between
`manager` and `windows_config`. ADR-0021 clause 1 has the account decided once, before anything
is written.

**What the old branch had.** Revision ticket 12 of `feat/revision-2026-10-31`, open, not worked: one
value that `run_as` produces before anything is written, the Client's and the process account,
requested against registered, which `manager` renders and an install sequence in `service`
executes, so unit tests drive the decisions without a service manager.
