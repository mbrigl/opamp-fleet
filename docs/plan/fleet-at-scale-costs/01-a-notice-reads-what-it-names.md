# Ticket: A notice reads what it names

## What to build

What a notification costs an instance grows with the fleet, not with what changed:

- a take-up after a rollout reads each held Agent with its own `agent_store.get`
  (`take_up_agents` in `fleet.rs`), about 3 300 sequential round trips per instance for a rollout
  to 10 000 Agents on three instances
- `ConfigStore::reload`, `DeploymentStore::reload` and `PackageStore::reload` read the whole
  store while they hold its write lock, on every batch of notices, and a rollout sends one
  `configuration` notice per pinned Configuration; `retain_only` collects in the database under
  the Configurations' write lock
- a host or certificate notice has every instance read the whole register again
  (`reload_ledger`), up to 100 000 entries, once per coalesced notice during a wave of renewals
- every reload read copies the whole revocation list (`fresh.revocations.clone()` in
  `revocation.rs`), refused reads included, under the state lock in the fallback

A take-up reads the held Agents with one `get_many`, a reload reads outside the lock and swaps, the
register is read by what the notice names, and the list is shared rather than copied. It blocks G-20
only if the load test of several-server-instances ticket 14 misses its target for want of it.

## Acceptance criteria

- [ ] Given a rollout to many Agents held by one instance, when it takes the rollout up, then it
  reads them in a number of round trips that does not grow with each Agent
- [ ] Given a reload of a store, when a reader asks meanwhile, then it is answered from the held
  store without waiting for the database
- [ ] Given a renewal on another instance, when its notice arrives, then the instance reads the
  entries it names, not the whole register
- [ ] Given the existing coordination tests, when they run, then they pass unchanged

## Read first

- ADR-0041 clauses 9, 10 and 12
- `fleet.rs` `take_up_agents`; `configs.rs`, `deployments.rs`, `packages.rs` `reload`;
  `instance.rs` `follow`; `revocation.rs` the reload read
- Seam: the coordination tests in `tests/instances.rs`; the load test of ticket 14 measures it

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
