# Ticket: The Configurations are read in one snapshot

## What to build

On PostgreSQL, `PgConfigBackend::load` (`postgres/configs.rs`, `read_all`) reads `configurations`
and then `configuration_revisions` as two autocommit statements. A change that another instance
commits between them is read in part:
- A Configuration created and released between the two statements makes `load` fail with "a
  revision in the database names configuration …, which is not there". At a start this fails
  `ConfigStore::open`, and the Server does not start. At a take-up (`reload_catalogue`), the read
  fails and is retried, and what the instance held is kept until a retry succeeds.
- A Configuration deleted between them is held without its retained revisions. A saved revision
  replaced while a release adds a retained one is held with the old saved revision and the new
  retained one.

Either way, the change's own notification reads again and corrects what is held. Retained
revisions are addressed by their hash, and no Agent is offered a saved revision, so no Agent is
offered content no one stored. `load` reads one committed state, in one REPEATABLE READ READ ONLY
transaction as `PgAgentStore::query` does, or in one statement as `postgres/agents.rs` `read`
does. This does not block G-20: what is lost is a failed read that is retried, or a start that
is attempted again.

## Acceptance criteria

- [ ] Given a Configuration created and released between the two statements of `load`, when
  `load` answers, then it answers without an error, with or without that Configuration
- [ ] Given a Configuration deleted between them, when `load` answers, then it holds the
  Configuration with all of its retained revisions or not at all
- [ ] Given the store checks, when they run, then they pass

## Read first

- ADR-0043; ADR-0041 clause 12
- `postgres/configs.rs` `load` and `read_all`; `configs.rs` `ConfigStore::open` and `reload`
- `postgres/agents.rs` `an_agent_is_read_in_one_snapshot_whatever_commits_while_it_waits` and
  `TestDatabase::hold_reads`: the seam that holds a table against reads between two statements

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
