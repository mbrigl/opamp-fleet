# ADR-draft-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under: The audit record is one hash chain per lease of a Server instance, and each entry names the lease its decision was taken under

- **Status:** 🟡 proposed
- **Date:** 2026-11-04
- **Deciders:** Markus Brigl
- **Applies to:** the audit port in `crates/fleet-server/src/audit.rs`, the writer and `verify` in `crates/fleet-server/src/audit_log.rs`, its filesystem adapter in `crates/fleet-server/src/fs/audit.rs` and every database adapter beside it, `server audit-verify` and the shutdown in `crates/fleet-server/src/main.rs`, the take of a lease in `crates/fleet-server/src/postgres/lease.rs` where it closes the chain of a lease it takes over, the `[audit]` section of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, and every document that tells an operator how to check the record
- **Supersedes:** [ADR-0042](0042-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)

## Context
The Server keeps an append-only audit record of every security decision, as
[ADR-draft-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again](draft-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again.md)
decides. Each entry carries `seq`, a counter that never repeats, and `prev`, the SHA-256 of the
entry before it, so the record is one chain. One writer thread appends to it in the order the
decisions were taken, and at startup the Server reads the tail of the newest file and chains on
from it (clause 3). The files live under `data_dir/audit/` and rotate by size: a file reaching
`max_file_bytes` is closed, `keep_files` are kept, and the deleted file's last hash is recorded
first
([ADR-0044](0044-the-servers-state-and-its-stores.md) clause 12).
The port, `AuditStore` in `crates/fleet-server/src/audit_log.rs`, speaks in files:
`tail`, `append`, `current_bytes`, `rotate`.

The Server's records may move to PostgreSQL, with `[database] store = "fs"` or `"postgres"`, and
the target architecture recorded outside the ADRs, the working note "server split" (D9), plans
several Server processes writing to one database. One chain cannot be extended by two writers
without serialising every security decision of the fleet on one lock across processes, and a
chain that a process takes up from another's tail mixes two processes' orders into one sequence
that neither wrote. The working note gives each writing instance a chain of its own, signs the
heads of all chains in an HSM, and closes a chain when its instance's lease lapses. Every Server
on PostgreSQL holds a lease, and takes a new one, under a new instance id, after it could not
renew the old
([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
clauses 1 and 3). A chain drawn with each lease therefore ends where the lease ends, and an
instance that takes a new lease does not chain on from entries it wrote under a lease another
instance may already have closed. The signing is a decision of its own; the shape of the record
is needed first, because the database schema and `server audit-verify` are built on it. This
ADR uses the leases of the instances ADR.

Size is the wrong bound for a database. A table has no file to close, and an operator asked how
long the record goes back answers in time, not in megabytes.

A reader without this conversation would expect one global chain and would take several chains
for a gap in the record.

An instance also decides while it holds no lease. Its listeners are closed then and every answer
is refused, but its background work goes on: an enrolment window runs out, a session ends, a
change another instance made is taken up. Such an entry is written into the chain of a later
lease, or of the lease that just lapsed, and without a mark of its own it reads there as a
decision of that lease. An instance that had to be alone and whose process is gone holds a lease
that has not lapsed, and the instance that takes it over closes its chain all the same; a reason
that says the lease lapsed misnames that close.

This ADR restates
[ADR-0042](0042-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)
in full. Its clause 3 marks an entry decided under no lease, names the close of a lease whose
holder is gone, reads back a closing write whose answer was lost, and ends the record at an
orderly shutdown with the close.

## Decision

We will chain the audit record per lease: on PostgreSQL each Server instance opens a chain of its
own with each lease it takes, named by an identifier it draws with the lease, on the filesystem
store the record stays one chain, and `server audit-verify` walks every chain the store holds.

1. **The record's own ADR,
   [ADR-draft-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again](draft-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again.md)
   clause 2, decides it: What an entry holds.**

2. **The record's own ADR,
   [ADR-draft-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again](draft-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again.md)
   clause 3, decides it: The chains.** On the PostgreSQL store each lease (the instances ADR,
   clauses 1 and 3) opens a chain of its own, which ends with the lease.

3. **A chain that ended is told apart from one that was cut.**
   - A closed chain ends with `audit.chain_closed`, and its record holds that entry's hash. The
     instance writes it into its own chain at an orderly shutdown, with `reason` `shutdown`. For a
     lease that lapsed, the instance that takes it over writes it into the chain the lease names,
     with `reason` `lease_lapsed` and `closed_by` the identifier of its own chain
     ([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
     clause 18). For the lease of an instance that had to be alone and whose process is gone, which
     the next instance to take a lease takes over before it lapses (that ADR's clause 2), it writes
     it with `reason` `holder_gone` and `closed_by` the identifier of its own chain. No other entry
     is written into a chain by an instance that did not open it. Entries missing from its end, a
     last entry that is not the recorded one, or an entry after it fail verification.
   - A closing write at shutdown whose answer was lost may have committed. The instance reads its
     chain back, and when the chain holds its closing entry, the close was written and the lease
     is released. Once its chain is closed, the instance records nothing more: a decision taken
     after the close is refused for want of a record, and the instance logs at `error`, before it
     exits, how many went unrecorded so.
   - A chain without a closing entry is reported as open, with its last entry. It is the chain of
     a lease that is held, or of one that lapsed and that no instance has taken over yet.
   - Entries an instance queued under a lease that lapsed before they were written go into the
     chain of its next lease, first after its `audit.chain_opened`, each carrying `taken_under`,
     the identifier of the chain whose lease it was decided under. The chain they are written into
     covers them, and the field says under which lease they were decided; none is counted as lost,
     and none is written into a chain the instance did not open.
   - An entry whose decision was taken while the instance held no lease carries `taken_under`
     `none`, in whichever chain it is written into, that of the lease that just lapsed included.
     The instance holds no lease from the moment its lease is lapsed by the clock it measures it
     on (the instances ADR, clause 3), and after it released it, until it holds a new one. The
     entries the writer adds of its own, the aggregates, `audit.resumed`, `audit.chain_broken` and
     the closing `audit.chain_closed`, carry no `taken_under`.
   - Removing a whole chain together with its record is not detected by the store itself.
     Checkpoints of every chain's head, signed with a key the store does not hold, detect it;
     they are a decision of their own, and this ADR lands without them.
   - Pruning leaves one more cut that the store itself does not detect. Whoever can write to the
     database can place one `audit.pruned` with a correct hash in a chain that then holds — at the
     end of an open chain, for example, or of a closed one whose record they rewrite — naming any
     chain and any of its entries as the last removed, and then delete that chain's entries up to
     it, younger ones included: `server audit-verify` accepts the link, because it cannot tell the
     age of what is gone. It takes links and a carried predecessor only from an `audit.pruned` in a
     chain whose own walk holds, so an `audit.pruned` in a chain that fails vouches for nothing.
     The signed chain heads detect the cut; until then a record that holds shows that what is left
     is linked, not that no entry younger than `retain_days` was removed.

4. **[ADR-0044](0044-the-servers-state-and-its-stores.md) clause
   12 decides it: How long the record goes back.**

5. **`server audit-verify` walks every chain.** `server audit-verify <data_dir>/audit` walks the
   files of the filesystem store. `server audit-verify --config <server.toml>` reads the
   PostgreSQL store with the Server's own credentials and walks each chain in `seq` order: every
   `prev` matches the entry before; the first retained entry is `audit.chain_opened`, or its `prev`
   is the `last_hash`, and its `seq` the one after the `last_seq`, of a retained `audit.pruned`
   that names that chain as its `pruned_chain`; a closed chain ends on its recorded hash, and one
   with no entries left has as its closing hash the `last_hash` a retained `audit.pruned` names
   for it. An `audit.pruned` links only when the chain it stands in holds: a chain whose walk needs
   no link from another, or only links from chains that hold, holds, and the walk is repeated
   until no further chain does. The predecessor is read from the first chain's
   `audit.chain_opened` or, once that is pruned, from a retained `audit.pruned` that carries it on
   in a chain that holds, by the same rule as the links. It names the chain and the first entry
   that fails, lists every open chain, and exits non-zero on any failure.

6. **A record moved from files to the database stays linked.** The files under `data_dir/audit/`
   that a Server wrote on the filesystem store stay where they are as a closed archive. The Server
   neither moves them nor writes to them again, `server audit-verify <dir>` verifies them as
   before, and deleting them is the operator's. `server migrate-to-postgres`
   ([ADR-0044](0044-the-servers-state-and-its-stores.md)
   clause 27) records the `seq` and the hash of the archive's last entry in the database, and the
   first chain the database store opens names them in its `audit.chain_opened` as its
   `predecessor`. `server audit-verify --config <server.toml> --archive <dir>` checks that the
   archive ends on the entry the predecessor names; without `--archive` it reports the
   predecessor as not checked.

**Out of scope:** checkpoints of the chain heads, signed with a key the store does not hold, a
decision of its own;
the leases themselves and how an instance finds one that lapsed
([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md));
exporting the record to a system outside the Server; a REST route to read the record; copying the
archived filesystem record into the database, which clause 6 links instead. Reading the record on
the Operator plane is not decided here: without a rights model every operator who passes the guard
would read it, where a rights model could keep it from those who only read or prepare. A chain for
each run of a local command that changes state, such as `server revoke <serial>` or `server
enrolment close`, is not decided here either; such a run holds no lease, and its authentication is
shell access on a Server host.

## Alternatives considered

- **One global chain, serialised in the database.** It keeps one sequence, but every security
  decision of every instance waits for one lock, and the order it records is the database's
  commit order rather than any instance's. Rejected.
- **One chain per host, taken up again after a restart.** It gives a chain a stable name, but a
  process taking up another's tail would chain on from an entry it did not write, which is what a
  restart after a crash makes fragile on the filesystem. A chain per lease opens and closes with
  the lease. Rejected.
- **No mark on an entry decided under no lease.** It is what an entry decided under the lease
  of its own chain looks like, so a decision of background work after a lapse reads as one of
  the next lease. Rejected.
- **A field of its own naming the lease held last before.** It says between which leases the
  decision fell, but the writer would have to keep that lease beyond the moment the instance
  loses it, and the times of the entries and of the next `audit.chain_opened` say as much.
  Rejected; `taken_under` `none` keeps one field for the question under which lease.
- **Aggregates keyed by the lease of the refusals they count.** An aggregate placed after the
  writer took up a new chain counts refusals of which some may have been decided under the lease
  before, and it reads as of the new one. Keying the count by lease would split one second's
  refusals of one peer into two aggregates for that narrow case, while the listeners are closed
  and only the refusals in flight are counted. Rejected; the aggregate stays the writer's own
  entry and carries no `taken_under`.
- **`reason` `lease_lapsed` for the close of a lease whose holder is gone.** It needs no new
  value, but that lease has not lapsed, and the record would misname why its chain ended.
  Rejected.
- **One chain per process, kept across leases.** A process whose lease lapsed would go on writing
  a chain that the instance taking over its lease may already have closed. Rejected; a new lease
  opens a new chain.
- **Retention by age on the filesystem store too.** It would give one rule for both stores, but
  the filesystem store's bound is the disk it shares with the rest of `data_dir`, and size is
  what keeps that disk from filling. Rejected; each store keeps the bound that fits it.
- **Retention by count of entries in the database.** It is as far from the operator's question
  as size is. Rejected.
- **Detect a deleted chain by a sequence of chain records.** A gapless sequence of chain records
  in the same database is deleted as easily as the chains it numbers. Only a signature with a key
  outside the database detects it, which the signed chain heads give. Rejected.

## Sources / Prior art

- The working note "server split", section D9, recorded outside the ADRs.
- The code as it stands: `AuditStore`, `Limits` and `verify` in
  `crates/fleet-server/src/audit_log.rs`, the filesystem adapter in
  `crates/fleet-server/src/fs/audit.rs`, and `audit_verify` in `crates/fleet-server/src/main.rs`.
- [Certificate Transparency, RFC 9162](https://www.rfc-editor.org/rfc/rfc9162): many
  independent append-only logs, each verified on its own, with signed heads as the anchor
  outside the log.

## Consequences

- Positive: several Server instances write the record without waiting on each other, and each
  chain is verified on its own.
- Positive: the filesystem store keeps one chain in files that rotate by size, verified file by
  file.
- Positive: retention on the database is stated in the unit an operator is asked about.
- Negative / trade-offs: the record has as many chains as the Server took leases, at least one
  per restart. Reading it in the order decisions were taken across instances means merging by
  `time`, which no chain guarantees across hosts.
- Negative / trade-offs: without the signed chain heads, whoever can write to the database can
  remove a whole chain and its record without the verification noticing.
- Negative / trade-offs: without the signed chain heads, whoever can write to the database can
  also cut the head of any chain with one `audit.pruned` appended to an open chain, young entries
  included, and the verification holds.
- Negative / trade-offs: an open chain does not say whether its instance still runs; until its
  lease lapses and another instance closes it, a dead instance's chain reads as a running one's.
- Follow-ups: an export of the record to a system the Server cannot rewrite.

## Enforcement

Tests that carry `Verifies:
ADR-draft-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under`:

- On the filesystem store, the existing tests of the chain, the rotation and `audit-verify` stay
  unchanged and green.
- On the PostgreSQL store, two writers on one database each open a chain, and `audit-verify`
  passes over both; an instance that takes a new lease opens a new chain with it.
- An entry altered, removed from the middle, or removed from the end of a closed chain, or an
  entry after its `audit.chain_closed`, fails `audit-verify`, naming the chain and the entry; an
  open chain is listed, not failed.
- Entries older than `retain_days` are removed, `audit.pruned` records the last removed hash, and
  `audit-verify` still passes.
- With two instances, entries older than `retain_days` of a chain closed at a shutdown and of one
  closed for a lapsed lease are removed by one instance at a time, its `audit.pruned` entries name
  both chains, and a closed chain with no entries left keeps its record.
- When the `audit.pruned` entries themselves and the first chain's `audit.chain_opened` are
  pruned, the links still needed and the predecessor are recorded again, and `audit-verify` still
  passes.
- `retain_days` with `store = "fs"`, or `keep_files` or `max_file_bytes` with
  `store = "postgres"`, fails startup naming the key.
- With the database held back past an instance's lease, the decisions it queued under that lease
  are written into its next chain with `taken_under` naming the old one, and `audit-verify` passes
  over both chains.
- A pruned chain whose first retained entry's `prev` is not the hash its `audit.pruned` recorded,
  and a closed chain with no entries left whose closing hash differs from that hash, fail
  `audit-verify`; so does a chain linked only by an `audit.pruned` in a chain that fails, and an
  archive whose predecessor only such an `audit.pruned` carries on.
- After `server migrate-to-postgres`, the first chain's `audit.chain_opened` names the archive's
  last entry; `audit-verify --archive` passes over the untouched archive and fails, naming the
  predecessor, when the archive's last entry was changed or removed.

Tests that carry `Verifies:
ADR-draft-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under`:

- A decision recorded while the instance holds no lease, by `held` or by its clock, is written
  into the next chain with `taken_under` `none`, and `audit-verify` passes over the record.
- An instance takes over the live lease of one that had to be alone and whose lock is free, and
  closes its chain with `reason` `holder_gone`.
- A closing write at shutdown that landed and whose answer was lost counts as written; a decision
  recorded after the close is refused and counted.
