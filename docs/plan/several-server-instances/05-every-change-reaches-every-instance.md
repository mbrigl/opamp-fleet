# Ticket: Every change reaches every instance

## What to build

Every change that concerns a live session is one `NOTIFY` on one channel in the transaction that
commits it: an assignment, a Package or Deployment published, a restart, a revocation added or
lifted, an enrolment decided, the window opened or closed, labels, an Agent forgotten or taken, a
floor set or removed. Ticket 02 sends `certificate` for a register entry written or removed and
`host` for a Host written or an `instance_uid` bound or released, and an instance reads the register
and the Hosts again on each; `floor` is sent and not yet followed. Each instance acts on the
sessions it holds. A lost listening connection, or a kind it does not know, counts as every
notification: it drops its caches, re-reads the revocation list, the assignments, the holders, the
labels, the enrolment state and the floors. The revocation list has one version, so a Gateway's
`ETag` answered by one instance is answered `304` by any. Each notification names the instance that
sent it, and an instance does not act on its own: it already holds what it committed, and reading
the ledger again for its own acts would cross every reload it makes during an enrolment wave.

## Acceptance criteria

- [ ] Given a rollout committed on A, when an Agent is carried by B, then B offers it the new
  configuration within one second
- [ ] Given an enrolment request waiting on B, when it is approved on A, then B hands the
  certificate to the connection that asked
- [ ] Given an instance whose listening connection is cut and restored, when a revocation was
  committed in between, then the instance ends the session it concerns once it is back
- [ ] Given a notification of a kind the instance does not know, when it arrives, then the instance
  re-reads as after a lost connection
- [ ] Given a Gateway holding the revocation list's `ETag` from A, when it asks B, then B answers
  `304`
- [ ] Given an enrolment wave on A, when A's own notifications arrive, then A reads nothing again
  for them, and B still takes each of them up
- [ ] Given a floor set on A, when an Agent asks B for the floors, then B serves the new floor
- [ ] Given a single Server on the filesystem store and one on PostgreSQL, when the existing
  integration suite and the store checks run, then both pass unchanged

## Read first

- ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it clauses
  9, 10, 11 and 12; ADR-draft-the-servers-state-and-its-stores clause 10;
  ADR-draft-admission-by-client-certificate-and-revocation clauses 32 and 34
- ADR-0026 for what a rollout offers, ADR-0022 for enrolment
- Golden path: the in-process push channel and the revocation list's subscription, which this ticket
  feeds from the database
- Seam: two instances in one test process, the listening connection cut by the test

## Blocked by

- docs/plan/several-server-instances/01-two-instances-share-one-database.md
- docs/plan/several-server-instances/02-the-trust-state-is-written-per-row.md
- docs/plan/several-server-instances/03-revisions-entries-and-members-are-written-per-row.md

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap on
open unattended work at zero. It decides when a revoked session ends and when an approved
certificate is handed out.

## Where it stands

Worked with the human on
`ticket/several-server-instances/05-every-change-reaches-every-instance`; the session stopped at a
coherent point, with what is left either more work of the same kind or a decision for the human.

Done:

- The notification names its kind, its identifier and the instance that sent it, as JSON on the one
  channel. Every connection of an instance carries a random origin as the session setting
  `opamp_fleet.origin`, so every store's `NOTIFY` names its sender without passing it on, and the
  listener hands on none its own instance sent. A payload that does not read counts as a kind
  nobody knows.
- `instance::follow(database, state)` replaces `follow_revocations`: the ledger for `revocation`,
  `certificate` and `host`; the floors for `floor`; the enrolment window and queue for `enrolment`
  and `window`, waking the connections waiting on them; the Configurations for `configuration`, the
  Deployments for `deployment`, the Packages for `package`, each read again whole; and the record
  of each Agent an `assignment` names, its assignment taken up and the WebSocket loops woken. A
  connection made again, and a kind not known, read all of these, every held Agent's record
  included.
- Sent now: every enrolment act on PostgreSQL (a request taken in, decided, approved, withdrawn; the
  window opened, closed, ended, saved), a Package created, its entry or signature written or
  removed, or deleted, a Configuration saved, collected or deleted, a Deployment
  written, amended, losing a member or deleted, and a rollout's release (each assignment by its
  `instance_uid`, each pinned Configuration by name).
- Criterion 8 is `scripts/check-all.sh`, green with the stores env.
- Criteria 1 to 7 are tests in `tests/instances.rs`, each failing without its mechanism (checked by
  mutation): `a_rollout_on_one_instance_is_offered_by_the_instance_that_carries_the_agent`,
  `an_enrolment_approved_on_one_instance_is_handed_out_by_the_other`,
  `a_revocation_committed_while_the_listening_connection_was_cut_ends_its_session_once_back`,
  `a_notification_of_an_unknown_kind_has_everything_read_again`,
  `an_etag_one_instance_answered_is_answered_304_by_another`,
  `a_package_published_on_one_instance_is_listed_by_the_other`,
  `an_instance_reads_nothing_again_for_its_own_notifications`,
  `a_floor_set_on_one_instance_is_served_by_the_other`.

Open, each a decision for the human before it is built:

- The revocation list's version. The Gateways' `ETag` is the SHA-256 of the list's body today; two
  instances that hold the same list answer the same one, so criterion 5 holds. The stores draft's
  clause 10 decides a counter in the database that every revocation, lift and list-moving register
  change increments, which is also what clause 10 of the multi-instance draft compares at each lease
  renewal. Options: (a) add the counter (a migration), serve it as the `ETag` on PostgreSQL, and
  compare it at each renewal; (b) keep the hash as the `ETag`, add the counter only for the renewal
  check, and revise the stores draft's clause 10; (c) keep the hash and drop the renewal check from
  the draft. Recommended: (a), as the draft decides, since the renewal check needs a value read
  without the list.
- Labels set, a restart requested and an Agent forgotten are not announced. They are written today
  by the whole-record `put` every report also uses, so announcing them needs either a port call per
  act now, or the per-column writes of ticket 04. Options: (a) ticket 04 announces its column writes
  through this ticket's envelope, and the take-up reads those columns as it reads the assignment;
  (b) a `put` with an announcement here, rewritten in ticket 04. Recommended: (a).
