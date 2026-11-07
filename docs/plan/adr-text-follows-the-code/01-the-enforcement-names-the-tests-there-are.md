# Ticket: The Enforcement names the tests there are

## What to build

The Enforcement sections of three accepted ADRs list tests that "will carry" their marker under
names no test has, and `scripts/check-traceability.sh` does not read those names:

- ADR-0044: `two_servers_of_a_new_version_started_together_apply_each_migration_once`,
  `a_revocation_beside_another_writers_keeps_both`,
  `a_host_written_beside_another_writers_keeps_both`,
  `the_revocation_version_moves_with_every_revocation_and_lift`,
  `two_writers_queue_two_requests_and_the_bound_counts_both`,
  `two_writers_saving_one_configuration_keep_both_revisions`,
  `two_writers_adding_two_entries_keep_both`, `two_writers_adding_two_members_keep_both`,
  `a_floor_below_the_stored_one_is_refused_in_the_transaction`,
  `a_server_holds_a_lease_before_it_opens_a_listener`,
  `an_instance_that_cannot_renew_its_lease_closes_its_listeners_until_it_holds_a_new_one`,
  `a_second_instance_beside_one_that_must_be_alone_fails_startup_naming_the_key`,
  `an_instance_that_must_be_alone_fails_startup_beside_a_live_lease`,
  `an_instance_that_loses_its_exclusive_lock_logs_it_and_refuses_every_call`,
  `migrate_to_postgres_leaves_the_audit_as_an_archive_and_records_its_last_entry`; it calls
  `a_server_that_loses_its_lock_logs_it_and_writes_nothing_more` replaced while it still exists,
  credits `a_data_dir_beside_the_database_holds_the_blobs_alone` with a refusal another test
  decides, and leaves out `a_data_directory_read_in_place_is_not_written`,
  `a_store_opened_before_its_lease_takes_no_upload_until_the_lease_is_named` and
  `a_bucket_that_fails_startup_leaves_no_lease_behind`
- ADR-0043: `put_keeps_the_labels_the_assignments_and_a_pending_restart`,
  `set_labels_and_set_restart_pending_write_what_they_name_alone`,
  `a_restart_requested_while_another_is_delivered_stays_pending`
- ADR-0045: `connections_past_the_rate_are_answered_503_with_a_spread_retry_after`,
  `two_hundred_new_connections_a_second_are_admitted_for_thirty_seconds`

Most of the behaviours are tested under other names. A behaviour no test decides gets its test
now, under the name the ADR gives; the rest of the list is a correction of accepted text, which
waits for the next normalization, together with the other places where accepted text and code
differ: ADR-0044 clause 12's "until no entry is that old" and "rolled back whole", clause 22's
"A test that will carry" before two tests, and clause 3 reading through `get` and `query` beside
the paging draft's `query_page`; ADR-0045 clause 21's "Today those are its two stores", which the
audit export of stage 3 makes stale. The traceability check reads the names an Enforcement
section lists, so the gap cannot open again.

## Acceptance criteria

- [ ] Given each name above, when this lands, then a test decides its behaviour, and the hand-off
  maps each listed name to the test that does
- [ ] Given an Enforcement section that names a test no file defines, when the traceability check
  runs, then it reports it
- [ ] Given the next normalization, when it restates ADR-0043, ADR-0044 and ADR-0045, then it
  takes up the map and the corrections listed here

## Read first

- ADR-0002 and `scripts/check-traceability.sh`; the Enforcement sections of ADR-0043, ADR-0044
  and ADR-0045
- [`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules), rule 5, on changing accepted text only by a
  normalization

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. The text corrections are a normalization, which only the human
orders.
