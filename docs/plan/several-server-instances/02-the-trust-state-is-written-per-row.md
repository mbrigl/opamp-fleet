# Ticket: The trust state is written per row

## What to build

Certificates, revocations, Hosts, the enrolment queue and the package floors stop being saved whole.
Each act is one transaction on the rows it names; an act that decides from what is stored reads it
under a row lock in the same transaction. Two operators revoking through two instances at once keep
both revocations; an instance with a stale view can no longer lower a floor or admit the renewal of
a revoked certificate. A unique constraint holds one `instance_uid` to one Host, and the register's
and the queue's bounds count the whole fleet. On the filesystem store nothing changes for the
operator.

## Acceptance criteria

- [ ] Given two instances, when each commits a different revocation at the same moment, then the
  list holds both
- [ ] Given a floor stored on the database and instance B holding an older one in memory, when B is
  asked to store a floor below the stored one, then it answers `409`
- [ ] Given a certificate revoked through A, when its successor is presented for renewal on B, then
  B follows it back to the revoked predecessor and refuses it
- [ ] Given one `instance_uid` reported by two Hosts on two instances, when each first report would
  bind it, then one is refused naming the binding
- [ ] Given the register at its bound across the fleet, when either instance would add to it, then
  the request is answered as the bound says
- [ ] Given the port contracts of the ledger, the enrolment and the floors, when they run on both
  stores, then they pass, the whole-or-nothing cases included
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clause
  19; ADR-draft-the-servers-state-and-its-stores clauses 2, 9, 10, 11 and 21
- ADR-draft-admission-by-client-certificate-and-revocation clauses 27, 28 and 30; ADR-0028 clause 15
- Golden path: the port contract tests of the ledger and the enrolment, which run one case set
  against both stores
- Seam: the port contracts, plus a two-instance case that holds a notification back

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It rewrites the stores that answer security decisions: revocation,
renewal, enrolment and the floors.

## Where it stands

Worked with the human on `ticket/several-server-instances/02-the-trust-state-is-written-per-row`;
the session stopped where the rest needs the human's decisions.

Done:

- The revocation list is written per row: the ledger port adds and removes one entry
  (`add_revocation`, `remove_revocation`), the store decides from what it holds whether the entry
  is present and whether the list is at its bound, and on PostgreSQL that is one transaction that
  holds the table against every other adder and carries the notification. The bound of the list
  is so counted for the fleet. Criterion 1 is decided by
  `two_revocations_committed_at_once_on_two_instances_are_both_kept` in `tests/instances.rs`.
- The package floors are decided from the stored row: the floor port answers `Put::Lower` with
  the floor it found, on PostgreSQL under `FOR UPDATE` in one transaction with the notification,
  and the instance takes up what it found. Criterion 2 is decided by
  `an_instance_with_an_older_floor_in_memory_cannot_lower_the_stored_one`.
- The port contracts of the ledger and the floors run on both stores with the new calls.

Done in part two:

- Criterion 3: the ledger port's `register_issued` walks the predecessor chain and reads the list
  in its own transaction (one recursive query on PostgreSQL, the register held against every other
  writer and the list against every adder) and refuses a renewal whose chain holds a revoked
  certificate. Decided by `a_stale_instance_cannot_register_the_renewal_of_a_revoked_certificate`.
- Criterion 5: the same write counts the register's size and the entries below the chain's root.
  Decided by `the_register_write_decides_from_the_filesystem` and `..._from_postgres`, and by
  `two_writers_at_the_registers_bound_write_one_entry_between_them`, which fails without the lock.
- Bindings: `host_bindings` (migration 4), one row per `instance_uid`, filled from the Hosts'
  arrays; a report that would bind an `instance_uid` another Host holds is refused naming it.
  Decided by `a_stale_instance_cannot_bind_an_agent_another_host_holds`,
  `two_binders_of_one_instance_uid_bind_it_once` and `the_bindings_move_out_of_the_hosts_arrays`.

- Review findings on part two: a Gateway's Host is bound none, in migration 4, an import and the
  binding itself, and marking a Host as a Gateway releases its bindings; a release and a re-key
  act on the binding the store holds, whatever the instance holds in memory, a re-key in one
  transaction, and a release takes the `instance_uid` out of the Hosts' arrays too; every register
  write, binding and write of the Hosts carries a `certificate` or `host` notification, on which
  an instance reads the register and the Hosts again. Decided by
  `a_release_a_rekey_and_a_gateway_mark_hold_for_every_instance`,
  `a_binding_another_instance_changes_reaches_an_instance_that_follows` and the ledger contract.
- Re-review findings on part two: on PostgreSQL every act on a Host is one transaction on its row,
  written under the lock a reload reads under, each field decided from what is stored (a valid
  until only moves forward, an approval and a subject are written only into a Host with no row
  yet, a Host is forgotten only while its stored valid until has passed), so no instance writes
  another's change back from a stale copy and no reload drops a write not yet committed; marking
  a Gateway and releasing its bindings is one write, and a binding refuses a Host stored as a
  Gateway; what an instance hears while it reads the ledger is answered by one read after it, a
  failed read is tried again, and a revocation is read with the whole ledger. Decided by
  `an_approval_survives_a_first_report_on_an_instance_that_never_heard_of_it`,
  `a_stale_instance_does_not_write_a_gateways_mark_back`,
  `a_stale_instance_binds_nothing_to_a_gateways_host`,
  `a_renewal_another_instance_registers_is_revoked_with_its_root_on_an_instance_that_follows`,
  `an_instance_uid_only_an_array_lists_is_that_hosts`,
  `the_bindings_leave_a_gateways_array_behind` and `an_import_binds_nothing_to_a_gateways_host`.

Decided by the human:

- An `instance_uid` is bound by a Host's first report, as ADR-0024 and its restatement decide, not
  by the approval of an enrolment; criterion 4 says so. Built.
- An `instance_uid` two Hosts' arrays hold fails migration 4, naming it and the Hosts, and changes
  nothing; the operator releases it from all but one. Built.

Open:

- The enrolment queue per row with its bound counted in the write: not built yet.
- The bootstrap entries' bound, which clause 19 of
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it
  names, is not counted in the write; it is counted in memory per instance.
- During a rolling update a release before the bindings' table binds through the Hosts' arrays
  only. This release honours those arrays when it binds, lists each binding it makes in its Host's
  array in the same write, and takes a released `instance_uid` out of them; ticket 11 decides the
  rest.
- Criteria 6 and 7 follow once the rest is built.
