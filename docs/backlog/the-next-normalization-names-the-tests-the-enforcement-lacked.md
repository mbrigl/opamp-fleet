# The next normalization names the tests the Enforcement lacked

**Kind:** ADR text.

**Observed.** Three accepted ADRs say in their Enforcement section that a test or a marker is
missing, and accepted text changes only by a normalization
([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules), rule 5):

- ADR-0030: "No test races a request through one instance against a delivery through another."
  `fleet.rs` `a_restart_requested_through_one_instance_while_another_delivers_one_stays_until_sent`
  now does.
- ADR-0023 clause 23: "No test of its own decides that an instance takes its lease before it first
  opens a listener." `tests/stateless_instance.rs`
  `an_instance_takes_its_lease_before_it_opens_a_listener` now does.
- ADR-0012: `plaintext_off_the_loopback_is_refused_before_connecting` "carries no marker". It now
  carries `Verifies: ADR-0012`.

ADR-0030 clause 2 also says `get_many` reads under the fleet lock. `hold_restarts` in
`crates/fleet-server/src/fleet/held.rs` calls it without that lock, as the take-up of named
Agents did before (183f49ca8).

**Idea.** When the next normalization restates these ADRs, it names the tests, drops the three
sentences and says how `get_many` reads.

**From.** Ticket adr-text-follows-the-code/01 (2026-11-12).
