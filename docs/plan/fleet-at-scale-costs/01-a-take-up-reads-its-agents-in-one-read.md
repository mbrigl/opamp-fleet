# Ticket: A take-up reads its Agents in one read

## What to build

A take-up after a rollout reads each held Agent with its own `agent_store.get`: `take_up_agents`
(`crates/fleet-server/src/fleet/held.rs`, near line 424) calls it once per Agent (near lines 435 to
438). That is about 3 300 sequential round trips per instance for a rollout to 10 000 Agents on
three instances. The take-up reads the Agents with one `get_many` instead, before the fleet lock as
today.

Two unit tests in `crates/fleet-server/src/fleet.rs` depend on one `get` per Agent, and change with
it:

- `a_take_up_reads_again_what_differs_in_one_read` (near lines 10606 to 10643) asserts three `get`s
  before the lock. It counts batched reads instead: one before the lock and one under it.
- `a_take_up_never_undoes_a_newer_act_of_its_own_instance` (near lines 10538 to 10563) injects its
  race through the `after_get` hook, which only `Hooked::get` fires (near lines 9030 to 9045). A
  hook is added to the first `get_many`, so the race of ADR-0045 clauses 8 and 9 stays exercised.
  `after_get` stays on `get`: the report path still reads with `agent_store.get` (`fleet.rs` near
  line 3452), and `a_label_set_while_a_report_reads_the_record_is_not_undone` depends on it.

The doc comment of `take_up_agents` (`held.rs`, near lines 404 to 416) is updated to say the first
read is one `get_many`.

ADR-0052 clause 2 says `get_many` reads under the fleet lock. A `get_many` before the lock widens
the deviation that the backlog entry
`docs/backlog/the-next-normalization-restates-the-audit-record-and-the-enforcement.md` records
(near lines 26 to 28); the change adds this caller to that entry.

**Effort.** S.

## Acceptance criteria

- [ ] Given a rollout to many Agents held by one instance, when the instance takes the rollout up,
  then it reads them in one `get_many` before the lock, not one read per Agent.
- [ ] Given `a_take_up_reads_again_what_differs_in_one_read`, when it runs, then it counts one
  batched read before the lock, one under it, and no `get`.
- [ ] Given `a_take_up_never_undoes_a_newer_act_of_its_own_instance`, when its act fires after the
  first `get_many`, then the instance's own act stays.
- [ ] Given the other coordination tests, when they run, then they pass unchanged.

## Read first

- Touches: `crates/fleet-server/src/fleet/held.rs`, `crates/fleet-server/src/fleet.rs` (its tests
  and `Hooked`),
  `docs/backlog/the-next-normalization-restates-the-audit-record-and-the-enforcement.md`
- ADR-0045 clauses 8 and 9; ADR-0052 clause 2 (`get_many`).
- Seam: the coordination tests in `crates/fleet-server/tests/instances.rs`.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
