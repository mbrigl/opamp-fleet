# The next normalization restates the audit record and the Enforcement

**Kind:** ADR text.

**Observed.** Accepted text changes only by a normalization
([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules), rule 5), and two changes to accepted ADRs wait for
one:

- **The audit record is one decision in four ADRs.** ADR-0028 (an append-only record whose failed
  writes on PostgreSQL are kept and tried again), ADR-0029 (one hash chain per lease, naming the
  lease each decision was taken under), ADR-0041 (the heads of the chains signed with a key on the
  token) and ADR-0043 clause 9 (what the export drops and counts). Each is true on its own; a
  reader has to assemble what the record holds, how it is chained, signed and exported from four
  records.
- **Three Enforcement sections name a test or marker as missing that now exists:**
  - ADR-0030: "No test races a request through one instance against a delivery through another."
    `fleet.rs`
    `a_restart_requested_through_one_instance_while_another_delivers_one_stays_until_sent` now
    does.
  - ADR-0023 clause 23: "No test of its own decides that an instance takes its lease before it
    first opens a listener." `tests/stateless_instance.rs`
    `an_instance_takes_its_lease_before_it_opens_a_listener` now does.
  - ADR-0012: `plaintext_off_the_loopback_is_refused_before_connecting` "carries no marker". It
    now carries `Verifies: ADR-0012`.

  ADR-0030 clause 2 also says `get_many` reads under the fleet lock. `hold_restarts` in
  `crates/fleet-server/src/fleet/held.rs` calls it without that lock, as the take-up of named
  Agents did before (183f49ca8).

**Idea.** The next normalization merges the four audit ADRs into one new ADR, names the three tests,
drops the three sentences and says how `get_many` reads. A superseding ADR through `propose-adr`
could take the audit record earlier; not during the freeze of 2026-11-12.

**From.** The design revision of 2026-11-12 (step 4) and the review of its branch; ticket
adr-text-follows-the-code/01 (2026-11-12). Merged into one entry from two on `feat/bugfixes`
(2026-11-14).
