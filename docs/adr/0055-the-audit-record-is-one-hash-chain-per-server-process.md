# ADR-0055: The audit record is one hash chain per Server process

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the audit port in `crates/fleet-server/src/audit.rs`, the writer and `verify` in `crates/fleet-server/src/audit_log.rs`, its filesystem adapter in `crates/fleet-server/src/fs/audit.rs` and every database adapter beside it, `server audit-verify` in `crates/fleet-server/src/main.rs`, the `[audit]` section of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, and every document that tells an operator how to check the record
- **Supersedes:** [ADR-0022](0022-an-append-only-audit-record-chained-by-hash.md) clauses 2 and 3; [ADR-0037](0037-the-servers-state-lives-under-one-data-directory-one-store-per-domain.md) clause 12

## Context

The Server keeps an append-only audit record of every security decision
([ADR-0022](0022-an-append-only-audit-record-chained-by-hash.md)). Each entry carries `seq`, a
counter that never repeats, and `prev`, the SHA-256 of the entry before it, so the record is one
chain. One writer thread appends to it in the order the decisions were taken, and at startup the
Server reads the tail of the newest file and chains on from it (clause 3). The files live under
`data_dir/audit/` and rotate by size: a file reaching `max_file_bytes` is closed, `keep_files` are
kept, and the deleted file's last hash is recorded first
([ADR-0037](0037-the-servers-state-lives-under-one-data-directory-one-store-per-domain.md)
clause 12). The port, `AuditStore` in `crates/fleet-server/src/audit_log.rs`, speaks in files:
`tail`, `append`, `current_bytes`, `rotate`.

The Server's records may move to PostgreSQL, with `[database] store = "fs"` or `"postgres"`, and
the target architecture recorded outside the ADRs, the working note "server split" (D9), plans
several Server processes writing to one database. One chain cannot be extended by two writers
without serialising every security decision of the fleet on one lock across processes, and a
chain that a process takes up from another's tail mixes two processes' orders into one sequence
that neither wrote. The working note gives each writing instance a chain of its own, signs the
heads of all chains in an HSM, and closes a chain when its instance's lease lapses. The signing
and the leases are decisions of their own; the shape of the record is needed first, because the
database schema and `server audit-verify` are built on it.

Size is the wrong bound for a database. A table has no file to close, and an operator asked how
long the record goes back answers in time, not in megabytes.

A reader without this conversation would expect one global chain and would take several chains
for a gap in the record.

## Decision

We will chain the audit record per Server process: each process that writes the record keeps a
chain of its own, named by an identifier it draws at startup, and `server audit-verify` walks
every chain the store holds.

1. **Restates ADR-0022 clause 2. What an entry holds.** `seq` (a counter from 1 within its chain
   that never repeats there), `time` (RFC 3339, UTC), `event` (a fixed name such as
   `admission.refused`), `outcome`, the peer address, the Agent's `instance_uid` where one is
   known, the presented certificate's CA role and serial where one is presented, the operator
   where one acted, event-specific fields, and `prev` — the SHA-256, hex, of the previous entry
   of the same chain as written. On the PostgreSQL store every entry also names its `chain`. A
   certificate appears only by role and serial, a CSR only by its key fingerprint. No entry holds
   a secret in any form: an `Authorization` value is never written, not even as a hash, whether an
   operator presented it or an Agent sent it unasked. A text field is cut at 256 bytes.

2. **Restates ADR-0022 clause 3. The chains.**
   - On the filesystem store there is one chain, as before: the first entry of a file carries the
     hash of the last entry of the file before it, or 64 zeros for the first file ever. At startup
     the Server reads the end of the newest file that holds a line, takes `seq` from the last whole
     entry, and chains on from the last line as it stands. A last line that is not a whole entry
     is kept and closed with a newline, and the next entry records `audit.chain_broken`.
   - On the PostgreSQL store each Server process draws a random chain identifier at startup and
     opens a chain of its own: a chain record with the identifier, the host name and the time it
     was opened, and a first entry `audit.chain_opened` whose `prev` is 64 zeros. Every entry the
     process writes belongs to that chain. On an orderly shutdown it writes
     `audit.chain_closed` as the chain's last entry, and the chain record takes the time and that
     entry's hash. No process appends to a chain it did not open.
   - Entries are written as clause 4 and clause 6 of ADR-0022 describe: one writer of their own per
     process, in the order the decisions were taken. On the PostgreSQL store an entry is written
     once its transaction is committed.

3. **A chain that ended is told apart from one that was cut.**
   - A closed chain ends with `audit.chain_closed`, and its record holds that entry's hash.
     Entries missing from its end, or a last entry that is not the recorded one, fail
     verification.
   - A chain without a closing entry is reported as open, with its last entry. It is the chain of
     a running process or of one that stopped without an orderly shutdown; telling the two apart
     follows with several instances and their leases.
   - Removing a whole chain together with its record is not detected by the store itself. An
     anchor outside the database, such as signed checkpoints of every chain's head, is a decision
     of its own.

4. **Restates ADR-0037 clause 12. How long the record goes back.**
   - On the filesystem store the audit is written to `data_dir/audit/audit-<first seq>.jsonl`.
     The directory is `0700` and each file `0600` on Unix, under the data root's restricted ACL on
     Windows. Each entry is one line, written with a single append and synced to disk; a write that
     fails partway is cut back to the last whole line. A file reaching `[audit] max_file_bytes`
     (default 64 MiB) is closed and a new one opened. The Server keeps `[audit] keep_files` files
     (default 16) and deletes the oldest. Before it does, it records `audit.rotated` with the
     deleted file's last hash.
   - On the PostgreSQL store entries older than `[audit] retain_days` (default 365, at least 1)
     are deleted, chain by chain, once a day by the process that holds the chain. Before it
     deletes, it records `audit.pruned` in its own chain, naming each chain it pruned, the last
     removed `seq` and that entry's hash. A closed chain with no entries left keeps its record.
   - `max_file_bytes` and `keep_files` apply to the filesystem store alone and `retain_days` to
     the PostgreSQL store alone. A key that does not apply to the configured store fails startup,
     naming it.

5. **`server audit-verify` walks every chain.** `server audit-verify <data_dir>/audit` walks the
   files of the filesystem store as before. `server audit-verify --config <server.toml>` reads the
   PostgreSQL store with the Server's own credentials and walks each chain in `seq` order: every
   `prev` matches the entry before; the first retained entry matches the hash an `audit.pruned`
   entry recorded for it, or is `audit.chain_opened`; a closed chain ends on its recorded hash. It
   names the chain and the first entry that fails, lists every open chain, and exits non-zero on
   any failure.

**Out of scope:** signing the heads of the chains with a key; closing the chain of a process that
stopped without an orderly shutdown, which follows with several instances and their leases;
exporting the record to a system outside the Server; a REST route to read the record; moving an
existing filesystem record into the database.

## Alternatives considered

- **One global chain, serialised in the database.** It keeps one sequence, but every security
  decision of every process waits for one lock, and the order it records is the database's
  commit order rather than any process's. Rejected.
- **One chain per host, taken up again after a restart.** It gives a chain a stable name, but a
  process taking up another's tail would chain on from an entry it did not write, which is what a
  restart after a crash already makes fragile on the filesystem. A chain per process opens and
  closes with the process. Rejected.
- **Retention by age on the filesystem store too.** It would give one rule for both stores, but
  the filesystem store's bound is the disk it shares with the rest of `data_dir`, and size is
  what keeps that disk from filling. Rejected; each store keeps the bound that fits it.
- **Retention by count of entries in the database.** It is as far from the operator's question
  as size is. Rejected.
- **Detect a deleted chain by a sequence of chain records.** A gapless sequence of chain records
  in the same database is deleted as easily as the chains it numbers. Only an anchor outside the
  database detects it. Rejected for now.

## Sources / Prior art

- The working note "server split", section D9, recorded outside the ADRs.
- The code as it stands: `AuditStore`, `Limits` and `verify` in
  `crates/fleet-server/src/audit_log.rs`, the filesystem adapter in
  `crates/fleet-server/src/fs/audit.rs`, and `audit_verify` in `crates/fleet-server/src/main.rs`.
- [Certificate Transparency, RFC 9162](https://www.rfc-editor.org/rfc/rfc9162): many
  independent append-only logs, each verified on its own, with signed heads as the anchor
  outside the log.

## Consequences

- Positive: several Server processes write the record without waiting on each other, and each
  chain is verified on its own.
- Positive: the filesystem store keeps its files, its rotation and its verification as they are.
- Positive: retention on the database is stated in the unit an operator is asked about.
- Negative / trade-offs: the record has as many chains as the Server had processes, one per
  restart. Reading it in the order decisions were taken across processes means merging by `time`,
  which no chain guarantees across hosts.
- Negative / trade-offs: until an anchor outside the database exists, whoever can write to the
  database can remove a whole chain and its record without the verification noticing.
- Negative / trade-offs: an open chain does not say whether its process still runs.
- Follow-ups: signed checkpoints of every chain's head with a key outside the database; closing
  the chain of a process whose lease lapsed; an export of the record to a system the Server
  cannot rewrite.

## Enforcement

Tests that will carry `Verifies: ADR-0055`:

- On the filesystem store, the existing tests of the chain, the rotation and `audit-verify` stay
  unchanged and green.
- On the PostgreSQL store, two writers on one database each open a chain, and `audit-verify`
  passes over both.
- An entry altered, removed from the middle, or removed from the end of a closed chain fails
  `audit-verify`, naming the chain and the entry; an open chain is listed, not failed.
- Entries older than `retain_days` are removed, `audit.pruned` records the last removed hash, and
  `audit-verify` still passes.
- `retain_days` with `store = "fs"`, or `keep_files` with `store = "postgres"`, fails startup
  naming the key.
