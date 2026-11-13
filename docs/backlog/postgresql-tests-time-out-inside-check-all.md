# Two audit tests race their writer

**Kind:** defect, test; latent, neither has failed yet.

**Observed.** Two tests of the audit record assume an order the writer does not promise:

- Other `audit_log` tests that fill the channel while holding the store lock, such as
  `a_throttle_refusal_that_finds_the_channel_full_is_counted_unrecorded` in
  `crates/fleet-server/src/audit_log.rs`, assume the writer has already taken its entry, as
  `a_held_place_counts_against_the_channel_until_it_is_written_or_given_back` did before it
  waited for that.
- The audit export's
  `a_silent_receiver_blocks_no_decision_and_the_dropped_count_reaches_the_record` in
  `crates/fleet-server/tests/audit_export.rs` sends its SIGTERM once the record holds every entry;
  a SIGTERM between the last batch's write and its hand-off to the export would put that batch's
  drops after the count.

**Idea.** Have each test wait for the state it assumes, as the held-place test waits for the
writer to take its entry, instead of assuming it.

**From.** Seen beside the PostgreSQL timeouts inside `scripts/check-all.sh` on `feat/stabilize`
and in its review (2026-11-12). The timeouts themselves were fixed on `feat/bugfixes`
(2026-11-13): the store steps run last, one after the other, and the workspace's test run drops
every `FLEET_TEST_S3_*` variable as it drops the PostgreSQL ones.
