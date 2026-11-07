# Ticket: The Enforcement names the tests there are

## What to build

The Enforcement sections of two accepted ADRs list tests that "will carry" their marker under
names no test has, and `scripts/check-traceability.sh` does not read those names:

- ADR-0043: `put_keeps_the_labels_the_assignments_and_a_pending_restart`,
  `set_labels_and_set_restart_pending_write_what_they_name_alone`,
  `a_restart_requested_while_another_is_delivered_stays_pending`
- ADR-0045: `connections_past_the_rate_are_answered_503_with_a_spread_retry_after`,
  `two_hundred_new_connections_a_second_are_admitted_for_thirty_seconds`

Most of the behaviours are tested under other names. A behaviour no test decides gets its test
now, under the name the ADR gives. One more has no test of its own: that an instance takes its
lease before it first opens a listener, which ADR-0050's Enforcement says no test
decides alone (clause 23). The rest of the list is a correction of accepted text, which waits
for the next normalization, together with ADR-0045 clause 21's "Today those are its two
stores", which the audit export of stage 3 makes stale. The traceability check reads the names
an Enforcement section lists, so the gap cannot open again.

## Acceptance criteria

- [ ] Given each name above, when this lands, then a test decides its behaviour, and the hand-off
  maps each listed name to the test that does
- [ ] Given an instance on PostgreSQL that starts, when it opens its listeners, then a test
  decides that it held its lease before
- [ ] Given an Enforcement section that names a test no file defines, when the traceability check
  runs, then it reports it
- [ ] Given the next normalization, when it restates ADR-0043 and ADR-0045, then it takes up the
  map and the correction listed here, and names the lease test in the restated stores ADR

## Read first

- ADR-0002 and `scripts/check-traceability.sh`; the Enforcement sections of ADR-0043 and
  ADR-0045 and of ADR-0050, which supersedes ADR-0044
- [`AGENTS.md` §3](../../../AGENTS.md#3-adr-rules), rule 5, on changing accepted text only by a
  normalization

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. The text corrections are a normalization, which only the human
orders.
