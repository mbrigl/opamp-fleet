# ADR-draft-an-append-only-audit-record: The Server keeps an append-only audit record of every security decision, each entry chained to the one before by its hash

- **Status:** 🟡 proposed
- **Date:** 2026-10-27
- **Deciders:** Markus Brigl
- **Applies to:** the audit port in `crates/fleet-server/src/audit.rs`, its writer in `crates/fleet-server/src/audit_log.rs`, its filesystem adapter in `crates/fleet-server/src/fs/audit.rs` and every database adapter beside it, every place in `crates/fleet-server/src/` that admits or refuses a peer, issues or revokes a certificate, takes an operator's act or records a package outcome, the `server audit-verify` command, and the `[audit]` section of `server.toml`

## Context

Every security measure of the Server decides something about a peer: admission and its refusal,
the throttle, an enrolment request and its approval, a certificate issued or renewed, a
revocation and the sessions it ended
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md),
[ADR-0022](0022-bootstrap-certificates-and-enrolment.md)), a rollout
([ADR-0026](0026-deployments-and-rollouts.md)) and a package outcome an Agent reports
([ADR-0027](0027-packages-signatures-and-delivery.md)). Without a record of their own these
leave log lines at best, mixed with everything else, kept as long as whatever collects the log
keeps it, and changeable by whoever can write the log. None of them is demonstrable afterwards:
an investigation cannot say who was admitted when, who approved a host, or whether a record was
removed. Measure H15 of [`HARDENING.md`](../HARDENING.md) asks for exactly one record per
decision, refusals included: the half that is easy to omit and the half an investigation needs.

The Agent plane admits by client certificate alone
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)), so there is no fleet
credential to offer, rotate or revoke and none for the record to cover. Certificate issuance is
a security decision like any other and waits for its record as admission does.

The specification puts security before convenience and holds that no vulnerability can be ruled
out (Strategy *Security before convenience*). A record that an intruder on the Server host can
quietly edit is worth little against exactly that intruder; a record whose entries each carry the
hash of the one before at least makes an edit or a removal visible to whoever checks it against a
copy taken earlier.

Two forces bound the design. A refused admission is the cheapest thing a remote peer can cause,
so a record per refusal must not let a flood fill the disk. And the record must not leak what it
records: no secret, no key, no CSR body. The Agent plane reads no `Authorization` header, but a
Client may still send one unasked, and an operator signing in to the Operator plane presents a
password; neither may reach the record.

Where the record is kept, in files or in PostgreSQL, its permissions, how it is written and how
long it goes back are part of the Server's state and its stores
([ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)).

This ADR restates [ADR-0023](0023-an-append-only-audit-record.md) in full. Its clauses 2, 3 and
6 take up the chains per lease of
[ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance](draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md),
and its clause 3 the lease of
[ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it](draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md).
Its clause 3 also changes the recovery from a torn line: the repair records the hash of the last
whole entry before the torn line, and the verifier checks it, so the anchor at the end of a record
binds every entry before a linked repair as well.

## Decision

We will append one JSON object per security decision to the Server's audit record, each carrying
the SHA-256 of the entry before it, emit the same entry as a `tracing` event with the target
`audit`, and bound the refusals by aggregation.

1. **One entry per decision, refusals included.** An entry is written for:
   - admission on `/v1/opamp` and on the download route: admitted, refused (which check refused,
     never the presented value), throttled. A WebSocket session and a download are recorded
     every time; a plain-HTTP poll once per peer address and certificate per hour, since a fleet
     polling every few seconds would otherwise crowd everything else out of the record;
   - enrolment: a request queued, approved, rejected; the window opened, or closed by an
     operator or run out, with the number of requests that expired with it;
   - issuance: a certificate signed on enrolment or renewal, with its CA role and serial, the
     predecessor, and the `instance_uid` of the request; a CSR refused, with the reason;
   - revocation: a certificate revoked or lifted, and each session it ended; a member refused
     the Gateways' revocation list because its host is not marked as a Gateway, with that host;
   - operator acts: every mutating route of the REST API, with the operator's user name and the
     route, once before it runs and once with its status after; what a revocation, an approval,
     a rejection or a window act did; and every refused or throttled operator sign-in;
   - packages: a rollout, and each package outcome an Agent reports, installed or failed.

2. **What an entry holds.** `seq` (a counter from 1 within its chain that never repeats there),
   `time` (RFC 3339, UTC), `event` (a fixed name such as `admission.refused`), `outcome`, the peer
   address, the Agent's `instance_uid` where one is known, the presented certificate's CA role and
   serial where one is presented, the operator where one acted, event-specific fields, and `prev` —
   the SHA-256, hex, of the previous entry of the same chain as written. On the PostgreSQL store
   every entry also names its `chain`. A certificate appears only by role and serial, a CSR only by
   its key fingerprint. No entry holds a secret in any form: an `Authorization` value is never
   written, not even as a hash, whether an operator presented it or an Agent sent it unasked. A text
   field is cut at 256 bytes: a user name or a path can come from a peer that was not admitted, and
   must not be able to fill the record.

3. **The chains.**
   - Each Server process writes its entries through one writer thread of its own, which takes
     them from the channel of clause 5 in the order the decisions were taken.
     [ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)
     clause 12 says where the entries are kept.
   - On the filesystem store the record is one chain. The first entry of a file carries the hash
     of the last entry of the file before it, or 64 zeros for the first file ever. At startup the
     Server reads the end of the newest file that holds a line, takes `seq` from the last whole
     entry, and chains on from the last line as it stands. A last line that is not a whole entry
     (a crash mid-write) is kept and closed with a newline, and the next entry records
     `audit.chain_broken`, so a truncation is itself on the record and `seq` never repeats.
   - That `audit.chain_broken` chains on the torn line by its `prev`, and names as `last_whole`
     the hash of the last whole entry before the torn line, or 64 zeros when there is none. A
     torn piece holds no `prev` of its own, so `last_whole` is what binds the entries after the
     repair to those before it. `server audit-verify` holds a line that is not an entry only
     when the next entry is that `audit.chain_broken`, and requires its `last_whole` to be the
     hash of the entry before the torn line wherever the files hold one; a repair can therefore
     not restart the chain anywhere. An `audit.chain_broken` after a torn line that names no
     `last_whole` is kept as written, since an entry is never rewritten: `server audit-verify`
     holds it and names it as a repair without a link, because the entries before it are not
     bound by those after it.
   - A crash during the repair itself leaves a run of torn lines: the torn piece, then a torn
     `audit.chain_broken`. The next start keeps the run, closes it with a newline, and writes one
     `audit.chain_broken` that chains on the last torn line by its `prev` and names as
     `last_whole` the last whole entry before the run, wherever the files hold it, in the same
     file or an earlier one. `server audit-verify` holds a run of torn lines closed by one
     `audit.chain_broken` whose `last_whole` links to the last whole entry before the run; a run
     closed by an `audit.chain_broken` that names no `last_whole`, or a run nothing closes, fails.
   - On the PostgreSQL store a Server instance draws a random chain identifier with each lease it
     takes and opens a chain of its own: a chain record with the identifier, the host name and the
     time it was opened, and a first entry `audit.chain_opened` whose `prev` is 64 zeros. The
     first chain the store ever opens names, as its `predecessor`, the last entry of the file
     chain a migration left as an archive
     ([ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance](draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)
     clause 6). Every
     entry the instance writes under that lease belongs to that chain, and the lease names it
     ([ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it](draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
     clauses 1 and 3). How a chain ends, and how one that ended is told apart from one that was
     cut, is clause 3 of
     [ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance](draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md).
   - On the PostgreSQL store an entry is written when the transaction that inserts it commits. A
     transaction that does not commit is a failed write (clause 5).

4. **Refusals are aggregated, never dropped.** Within one second, refusals of the same `event`
   from the same peer address (an IPv6 one by its /64, as the throttle counts it) after the
   tenth are counted rather than written, and one `…aggregated` entry with the count follows
   when the second ends. A flood of refusals therefore costs at most ten lines and a count per
   address and second, and every refusal is still accounted for.

5. **Writing never blocks admission on a slow disk, and never silently fails.** Entries go
   through a bounded channel of 10 000 to the writer thread, and a decision goes ahead once its
   entry is queued. When the channel is full, admission and every operator act are refused with
   `503` and no certificate is issued until it drains. When a write fails, the error is logged
   at `error`, and from then on nothing is admitted, no certificate is issued and no operator
   act runs until a write succeeds; the writer retries every second. A CSR held back for want of
   a record is answered `ServerErrorResponse` `Unavailable` with `retry_info`, since the Server,
   not the request, is at fault and the Agent asks again. Decisions that were queued when the
   write failed, or refused for want of a record, are counted, and the next entry that lands
   carries the count as `unrecorded_before`. A security decision is never taken without its
   record queued, and a record that is lost is never lost silently.

6. **The log carries it too.** Every entry is also emitted as a `tracing` event with the target
   `audit`, the same fields, at `info`, so a log collector receives it without reading the store.
   The record is the chains of clause 3, one per lease on PostgreSQL, stored in files or
   in PostgreSQL where the Server keeps its state; the log is a copy.

**Out of scope:** an audit record on the Client; shipping the record to a remote store or
signing entries with a key; a REST route to read the record; retention by age on the filesystem
store. Where the record is kept, its permissions, its writing and how long it goes back are
[ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)'s.

## Alternatives considered

- **Only the normal log, under a target of its own** — retention and integrity would depend on
  whatever collects the log, and a host without a collector keeps nothing an investigation can
  rely on.
- **OTLP logs to a collector** — central, but a Server without a reachable collector loses
  exactly the record an outage investigation needs; it can be added from the `tracing` copy.
- **Entries without a chain** — simpler, but an edit or a removal is invisible.
- **Signing each entry with a key** — proves more than a chain, but the key would live on the
  same host as the record, so an intruder who can rewrite the record can re-sign it; a chain
  checked against an earlier copy gives most of the value without key management.
- **Dropping refusals under load** — cheaper, but an attack is exactly when refusals matter;
  aggregation keeps the count.
- **An audit record on the Client as well** — a second format to maintain; the Client reports
  its package outcomes to the Server, which records them.

## Sources / Prior art

- [`HARDENING.md`](../HARDENING.md), measure H15 and the observable it names.
- [OWASP Logging Cheat
  Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html)
  — which events to record, and never to record secrets.
- [JSON Lines](https://jsonlines.org/); [RFC 3339](https://www.rfc-editor.org/rfc/rfc3339).
- Hash-chained logs as in [RFC 6962](https://www.rfc-editor.org/rfc/rfc6962) (Certificate
  Transparency), in their simplest linear form.

## Consequences

- Positive: an incident becomes an investigation: who was admitted, refused, enrolled, approved,
  issued, revoked and rolled out to, when and by whom, with an edit or a removal visible against
  an earlier copy.
- Negative / trade-offs: a write per decision on the admission path; a disk that stops writing
  stops admission and issuance (clause 5), which is the intended order of failure. The record
  grows with the fleet's traffic and is bounded by size in files, so a large fleet keeps less time
  there than a small one, and by age in PostgreSQL. An entry is never rewritten: a file holding
  `rotation.offered` entries or credential hashes cut to eight hex digits keeps them in the chain
  as written.
- Follow-ups: shipping the record off the host; reading it through the REST API; an audit record
  of the Client's own decisions.

## Enforcement

- `crates/fleet-server/src/audit_log.rs` tests: `each_entry_carries_the_hash_of_the_one_before`,
  `the_chain_survives_a_restart_and_a_rotation`, `a_truncated_file_is_recorded_as_a_broken_chain`,
  `refusals_beyond_ten_a_second_are_aggregated_with_their_count` (clauses 2, 3, 4),
  `a_write_that_fails_refuses_until_one_succeeds` (clause 5).
- `crates/fleet-server/src/fs/audit.rs` test:
  `the_record_is_owner_only_and_continues_where_it_ended` (clause 3; the file's permissions are
  [ADR-0029](0029-the-servers-state-and-its-stores.md)'s).
- The repair's link (clause 3): `crates/fleet-server/src/audit_log.rs` tests
  `a_truncated_file_is_recorded_as_a_broken_chain`,
  `a_torn_line_holds_only_when_the_next_entry_records_it_as_broken` and
  `a_repair_without_a_link_holds_and_is_named`; `crates/fleet-server/src/fs/audit.rs` test
  `a_line_torn_inside_a_character_verifies_as_the_store_hashed_it`,
  `a_line_with_an_invalid_byte_inside_is_refused_naming_it`,
  `a_torn_line_that_opens_a_file_is_linked_to_the_file_before` and
  `a_crash_during_the_repair_is_closed_by_one_linked_repair`;
  `crates/fleet-server/src/postgres/audit.rs` tests
  `an_archive_the_file_store_repaired_after_a_crash_is_linked` and
  `an_archive_whose_entries_before_a_repair_were_replaced_fails_audit_verify`.
- `crates/fleet-server/tests/mutual_tls.rs` tests:
  `an_admission_and_its_refusal_each_leave_one_entry`, which refuses a revoked certificate,
  `an_issued_certificate_is_recorded_with_its_predecessor`,
  `a_revocation_records_the_sessions_it_ended`, `an_operator_act_names_the_operator`,
  `an_audit_that_cannot_write_refuses_admission` (clauses 1, 5). That no entry holds the
  operator's password is asserted by `an_operator_act_names_the_operator`.
- `crates/fleet-server/tests/mutual_tls.rs`:
  `an_authorization_an_older_client_sends_is_never_written`: an Agent admitted on its
  certificate while sending an `Authorization` header leaves an admission entry that holds
  neither the value nor any hash of it (clause 2);
  `an_audit_that_cannot_write_issues_no_certificate`: with the record unavailable, a CSR on a
  member connection gets no certificate and is answered `Unavailable` with `retry_info`
  (clause 5); `an_unmarked_member_asking_for_the_list_leaves_a_refusal` (clause 1).
- `crates/fleet-server/src/fleet.rs`: `unavailable_tells_the_agent_when_to_retry` (clause 5).
- Clauses 2 and 3 as they stand here, the chains per lease on the PostgreSQL store included, are
  verified by the tests
  [ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance](draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)
  names.
