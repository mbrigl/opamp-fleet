# Ticket: A missed notification is caught up from one read of what the instance holds

## What to build

An instance counts a missed notification as every notification (ADR-0045 clause 10). What that
means is written by hand once per kind of state the instance keeps, each a `TakeUp::Everything`
branch of its own in `crates/fleet-server/src/fleet/held.rs`:

- `take_up_agents` compares the assignment, `restart_pending` and the labels by a hand-kept list
  of fields, and calls `place_again_for` for the groups;
- `take_up_restarts` reads each kept put-back with its own `get`, and `hold_restarts` reads again
  without a lock;
- `release_taken` reads the holders a third time and takes the lock of every Agent carried.

`instance::follow` maps `Due::EVERYTHING` onto these one by one. Each of the last count's defects
on this path patched one branch: 183f49ca8 (a restart requested during a delivery), 06efe2866 (a
group placed again), afe4fec00 and 645502fb0 (a re-key and the shutdown flush), and the backlog
entry `a-kept-restart-is-lost-across-a-rekey-through-another-instance.md` names the next. A new kind
of kept state needs a branch of its own, and nothing fails when it lacks one. In
`tests/instances.rs` only the revocation, the binding and an unknown kind are driven through a cut
listening connection; the restart, label and placement catch-ups are tested by calling the
`take_up_*` methods in the order the handler does.

Give `fleet::held` one catch-up for a missed notification. It reads every held Agent's record and
holder once, and derives from that read what each kind of kept state needs. `follow` calls it for
`Due::EVERYTHING`. The named path (`TakeUp::Agents`) stays as it is; its cost is
`docs/plan/fleet-at-scale-costs/01-a-notice-reads-what-it-names.md`. What the catch-up places
follows ADR-0045 clause 8 and does not go beyond it.

## Acceptance criteria

- [ ] Given two instances on one database and B's listening connection cut, when A changes an
  Agent B holds — an assignment, a requested restart, its labels, a Client Agent's labels its group
  inherits, a take — then once B listens again, B's memory holds each change. Each case is one
  test through the cut connection, not through the `take_up_*` methods.
- [ ] Given a missed notification, when the instance catches up, then it reads the store once for
  the records and once for the holders, however many kinds of state it keeps.
- [ ] Given `tests/instances.rs`, the `fleet.rs` tests and the store checks, when the change lands,
  then they pass.

## Read first

- Touches: `crates/fleet-server/src/fleet/held.rs`, `crates/fleet-server/src/fleet.rs`,
  `crates/fleet-server/src/instance.rs`, `crates/fleet-server/tests/instances.rs`
- ADR-0045 clauses 8 to 10; ADR-0030 clause 2 (`get_many`); `b7aa459f9`, the cut-listener test,
  the seam.
- ADR-0030 clause 2 says `get_many` reads under the fleet lock, and both reads now run under
  neither (named in the review of 183f49ca8). That is accepted text, changed only by a
  normalization; the session names it in its hand-off, it does not change the ADR.
- It decides nothing ADR-0045 does not decide.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero.
