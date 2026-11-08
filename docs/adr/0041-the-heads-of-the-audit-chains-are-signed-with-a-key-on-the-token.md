# ADR-0041: The heads of the audit chains are signed with a key on the token

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** Markus Brigl
- **Applies to:** the checkpoints of the audit record in `crates/fleet-server/src/audit_log.rs` and `audit.rs`, their filesystem adapter in `crates/fleet-server/src/fs/audit.rs` and every database adapter beside it, the checkpoints' column, index and head row in the PostgreSQL schema, the signer port for checkpoints and its PKCS#11 adapter, the advisory lock that elects the signer on PostgreSQL and the connection it is held on, the `[audit] key_label` key of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, `server audit-verify` and `server pki status` in `crates/fleet-server/src/main.rs`, and every document that tells an operator how to check the record or what to keep of it

## Context

The audit record is a hash chain per lease of a Server instance
([ADR-0029](0029-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under.md)),
in files or in PostgreSQL where the Server keeps its state. A chain shows an entry changed or
removed in its middle, but whoever can write the store can cut a chain's tail, remove a whole chain
with its record, or rewrite a chain from any point on with fresh hashes, and `server audit-verify`
finds nothing. That ADR names the anchor for this, signed checkpoints of every chain's head, as a
decision of its own, and this ADR decides it. With PostgreSQL the people who can write the store are
not only those who hold the Server's host: a database administrator, a stolen dump, a replica.

The append-only audit record
([ADR-0028](0028-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again.md))
rejects signing entries because the key would live on the same host as the record, and whoever can
rewrite the record could read the key and re-sign. A key on a PKCS#11 token cannot be read
([ADR-0043](0043-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md)):
whoever holds the store but not the token cannot sign. That ADR names the audit key,
`[audit] key_label`, and a signer port for checkpoints; this ADR decides the checkpoints' format,
where they are kept and which instance signs them. With several Server instances on one database
([ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md)),
one instance at a time signs, by an advisory lock any instance can take.

A signature on every entry would put the token in the path of every admission. A signature on
the heads at intervals bounds what can be rewritten unseen to one interval, off the decision path.

A signature in the store proves what was there when it was made; it cannot prove that nothing came
after it, because whoever can write the store can drop the newest checkpoints together with the
entries they cover. Certificate Transparency answers this with signed tree heads that leave the log
and are compared by others. The record already leaves the Server as a log copy
(ADR-0028 clause 6) and, where configured, as an export (the token
ADR, clause 9).

The audit chain ADR stands without this one: the chains stand on their own, and a Server without
a token keeps them as that ADR says. A reader without this conversation would expect a
key file to stand in for the token, or a signature on every entry.

## Decision

We will sign, about once a minute and at an orderly shutdown, a checkpoint that names the head of
every audit chain with the audit key on the PKCS#11 token, write it into the record as an entry of
the signing process's own chain, and let `server audit-verify` check every chain against the
newest checkpoint it finds or is given.

1. **A checkpoint names the head of every chain.** It is the audit entry `audit.checkpoint`, with
   the fields of every entry (ADR-0028 clause 2) and these:
   - `checkpoint_seq`, a counter from 1 across the whole record that never repeats, and
     `checkpoint_prev`, the SHA-256, hex, of the previous checkpoint entry as written, or 64 zeros
     for the first;
   - `heads`: for every open chain, and for every chain closed or pruned since the previous
     checkpoint, its identifier, the `seq` of its last entry and that entry's hash, and whether the
     chain is closed. A chain closed before the previous checkpoint is named in that one, and a
     verifier carries its closed head forward from it, so a checkpoint grows with the chains that
     are open, not with every lease ever taken. The filesystem store's one chain is named `fs`.
     For the signing process's own chain the head is the entry before the checkpoint;
   - `key`: the audit key's label and the SHA-256, hex, of its public half as DER
     `SubjectPublicKeyInfo`;
   - `sig`: the signature, ECDSA P-256 over the SHA-256 of the entry's line as written without
     `sig`, made with one `C_Sign` as the token ADR's clause 5 makes every signature, as a DER
     `ECDSA-Sig-Value` in base64.

   The entry is the audit entry the token ADR's clause 6 asks for every signature, and it chains
   as any other entry: its `prev` covers it, and the next entry covers its hash. A checkpoint is
   rotated and pruned with the record. An `audit.rotated` or `audit.pruned` entry that removes
   checkpoints names, besides what it names of the entries, the `checkpoint_seq` and the hash of
   the last checkpoint it removed. A prune removes no checkpoint while an older one is still held
   in a chain it does not prune with it: it ends that chain's run before such a checkpoint, so the
   checkpoints the store holds always follow each other. Nor does it remove the checkpoint that
   named a closed chain closed while that chain still holds entries it leaves, since no later
   checkpoint names the chain. A prune repeats its rounds until a round
   frees nothing more, so the chains one instance prunes, its own and the closed ones, drain in
   one prune. A chain another running instance holds is pruned by that instance at its own time of
   day: after the signer changed between running instances, entries may outlive
   `[audit] retain_days` by up to a day for each change since the oldest checkpoint left. The
   filesystem store's one chain rotates its checkpoints in order.

2. **One process signs, every 60 seconds and at an orderly shutdown.**
   - On the filesystem store the Server signs. On PostgreSQL the instance that holds a session-level
     advisory lock signs; every instance names the same `[audit] key_label`
     ([ADR-0043](0043-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md)
     clause 14) and tries to take it at every interval, and a lock that ends with its connection
     passes to the next instance that tries. The lock is held on one connection of its own per
     instance, outside the pool, which the signer checks at every interval; an instance signs only
     while its lease is live. Both ends of that connection count it dead after about 30 seconds
     without an answer, by TCP keepalives and a TCP user timeout, so the lock of a signer cut off
     from the database passes on well within a minute.
   - The signer reads the heads from the store and writes the checkpoint into its own chain. A
     checkpoint is skipped when no chain grew since the last one but by that checkpoint itself.
   - The store holds at most one checkpoint per `checkpoint_seq`, by a unique index, and inserts a
     checkpoint only while its `checkpoint_prev` names the newest one. On PostgreSQL the entry's
     `checkpoint_seq` is a column of its own under a partial unique index, and one row holds the
     newest checkpoint's `checkpoint_seq` and hash, compared and set under `FOR UPDATE` in the
     transaction that inserts it, so a prune does not lose it. A signer that lost its lock while a
     checkpoint was in flight therefore cannot fork the checkpoints: the insert of the one that came
     second fails, and its signer writes `audit.checkpoint_failed`.
   - A checkpoint is written alone, in a write of its own. One whose write fails for any reason is
     dropped, never written again, since its `sig` covers its place; `audit.checkpoint_failed`
     names the error, and the next interval signs afresh.
   - At an orderly shutdown the signer writes a last checkpoint before `audit.chain_closed`. A
     process that is not the signer closes its chain, and the signer's next checkpoint names the
     closed head.
   - Signing runs beside the writer thread and never delays a decision. A thread of its own reads
     the heads and asks the token; the writer gives the checkpoint its place and waits for the
     signature at most 2 seconds, while decisions go on queueing for it. A signature that does not
     come in time is not waited for: the checkpoint is taken back, and `audit.checkpoint_failed`
     names `timeout`. When the token does not sign, the Server logs at `error`, writes
     `audit.checkpoint_failed` with the token's error, and tries again at the next interval.
     Decisions go on: the chains stay the record, and only the anchor waits.

3. **The audit key's public half is printed and kept.** `server pki status` prints the audit key's
   label, its public half as PEM and the SHA-256 the checkpoints carry in `key`. The operator
   keeps the PEM where the Server cannot rewrite it; it is what a verifier checks against.

4. **`server audit-verify` checks every chain against the checkpoints.** With `--audit-key
   <file holding the public key as PEM>`, over the files and with `--config` alike, besides the
   checks of the audit chain ADR's clause 5:
   - only a signed removal counts: an `audit.rotated` or `audit.pruned` entry is honoured only
     where a later checkpoint that verifies names a head of its chain at or after it, and the store
     still holds that head with the hash the checkpoint names, so that the chain's links from the
     removal to the head prove the removal was there when the head was signed. One after every
     such head, in the unsigned tail or inside a run that was cut, is reported and never
     honoured, so whoever can write the store cannot excuse a removal by adding the entry that
     would explain it;
   - every checkpoint's `sig` verifies against the given key, its `key` names that key, and the
     checkpoints follow each other by `checkpoint_seq` and `checkpoint_prev`. The first checkpoint
     the store still holds follows the last removed one that a signed removal names, or is the
     first ever, with `checkpoint_seq` 1 and 64 zeros;
   - every head any checkpoint names, not only the newest one's, is in the store: its chain
     reaches at least the `seq` it names and has at that `seq` the hash it names, unless a signed
     removal recorded the removal of that part
     ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
     clause 12). A chain whose first entries are gone begins right after the last entry a signed
     removal names;
   - an entry written more than three intervals after the newest checkpoint, with no
     `audit.checkpoint_failed` between, fails as an unsigned tail: a running signer would have
     covered it.

   With `--since <file>`, a file holding a checkpoint line the operator saved from an earlier run,
   the log copy or the export, the store's checkpoints must hold that one unchanged and continue
   from it. A saved checkpoint that rotation or pruning has removed must match the last removed
   checkpoint a signed removal names, when it is that one; one older than
   that is reported as no longer held, and the check goes on from the oldest checkpoint the store
   holds. The command names the first failure by chain, `seq` and checkpoint, prints the newest
   checkpoint's line after `checkpoint: ` for the operator to keep as the next `--since`, and exits
   non-zero on any failure. Without `--audit-key` it checks the chains alone and says that it
   checked no signature.

5. **Without a token, the chains are the record.** Without `[audit] key_label` no checkpoint is
   written, and the record is the chains of the audit chain ADR alone. The Server logs at startup,
   at `warn`, that the audit record's heads are not signed. There is no key file for the audit key:
   a key that whoever can rewrite the record can also read would sign the rewrite
   (ADR-0028). `server audit-verify --audit-key` on a record without a
   checkpoint fails, naming the missing checkpoints, so a record whose checkpoints were removed
   whole does not pass as one that never had them.

**Out of scope:** where a saved checkpoint is kept and who compares it, which is the operator's;
a transparency log or a witness that cosigns checkpoints; signing individual entries; the format
of the export (the token ADR, clause 9); signing with a key outside a PKCS#11 token; a REST
route to read checkpoints; a configurable interval.

## Alternatives considered

- **Sign every audit entry.** One `C_Sign` per decision puts the token in the path of every
  admission and makes a slow token a slow fleet. A checkpoint a minute bounds what can be
  rewritten unseen to that minute. Rejected.
- **A key file for the audit key when no token is configured.** It would give every installation
  signed heads, but the key would sit beside the record it protects, which is what the append-only
  audit record rejects. Rejected; without a token the heads are unsigned and the Server says so.
- **Each process signs only the head of its own chain.** No lock is needed, but a chain removed
  whole together with its checkpoints is not missed by anyone, since no other chain names it.
  Rejected; one checkpoint names every chain.
- **Checkpoints in a table or file of their own.** It keeps them apart from the entries, but a
  separate store is one more thing to bound, back up and prune, and nothing chains it to the
  record. As an entry the checkpoint is kept, rotated and pruned with the record, and the chain
  covers it. Rejected.
- **A Merkle tree over all entries, as Certificate Transparency signs.** It allows proofs of
  inclusion for one entry without the whole record, which nobody here asks for; the verifier
  walks every chain anyway. Rejected for the linear heads the chains already have.
- **Fail verification whenever the newest checkpoint is old.** A Server that was stopped would
  fail every check of its record. Rejected; only entries after the newest checkpoint make a tail
  unsigned.

## Sources / Prior art

- [Certificate Transparency, RFC 9162](https://www.rfc-editor.org/rfc/rfc9162): signed tree heads
  as checkpoints of an append-only log, and their comparison outside the log as what detects a
  log that was cut or forked.
- PostgreSQL documentation, [advisory
  locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS): a
  session-level lock ends with its connection.
- [PKCS #11 Specification Version
  3.1](https://docs.oasis-open.org/pkcs11/pkcs11-spec/v3.1/pkcs11-spec-v3.1.html): `CKM_ECDSA`
  and its `r || s` signature.
- The code as it stands: the writer and `verify` in `crates/fleet-server/src/audit_log.rs`, and
  `audit_verify` in `crates/fleet-server/src/main.rs`.

## Consequences

- Positive: with a token, whoever can write the store but cannot use the token cannot cut,
  remove or rewrite a chain before the newest checkpoint without `audit-verify` naming it.
- Positive: the check needs only the public half and the record; a saved checkpoint extends it to
  a cut of the newest checkpoints themselves.
- Negative / trade-offs: what was written after the newest checkpoint, up to one interval, is
  protected by the chain alone. A cut of the record together with its newest checkpoints is found
  only against a checkpoint that left the store.
- Negative / trade-offs: a compromised Server can have the token sign whatever it wrote; the
  export and the log copy are what show that afterwards.
- Negative / trade-offs: a checkpoint entry a minute while the fleet is busy, and a read of every
  chain's head per checkpoint on PostgreSQL.
- Follow-ups: publishing checkpoints to a witness or transparency log; a glossary entry for the
  checkpoint.

## Enforcement

Tests that carry `Verifies:
ADR-0041`, against SoftHSM2
where a token is needed:

- With `[audit] key_label` on the filesystem store, a checkpoint is written within an interval
  and at shutdown, names the chain's head, and verifies against the key `server pki status`
  prints.
- `audit-verify --audit-key` passes over an untouched record, and fails, naming the chain and the
  checkpoint, when an entry before the newest checkpoint is changed, when a chain's tail before it
  is cut, when a chain it names is removed with its record, or when a checkpoint's signature does
  not verify.
- An entry written more than three intervals after the newest checkpoint fails as an unsigned
  tail, unless `audit.checkpoint_failed` entries lie between.
- On either store, a chain removed whole or cut at its start, with an `audit.pruned` or
  `audit.rotated` added after the newest checkpoint to explain it, still fails: the added entry is
  reported and not honoured.
- With checkpoints in two chains after a signer change, a prune of the chain that holds the newer
  ones stops before them while the other chain holds older ones, and `audit-verify --audit-key`
  passes after each prune. After several signer changes, one prune by the instance that prunes
  the closed chains frees every old checkpoint in its rounds. A prune keeps the checkpoint that
  named a closed chain closed while that chain holds entries, so the chain removed afterwards
  still fails.
- `--since` with a saved checkpoint fails when the store does not hold it and no rotation or pruning
  removed it; after a prune that removed checkpoints, `audit-verify --audit-key` passes from the
  first checkpoint left, a saved checkpoint the prune removed last is matched against the hash the
  `audit.pruned` entry names, and a first checkpoint left that does not follow that one fails.
- On PostgreSQL, with two instances, one signs at a time, each checkpoint names both chains, and
  the other takes over when the signer's connection ends.
- A signer that loses its lock while a checkpoint is in flight leaves one checkpoint per
  `checkpoint_seq`, the second insert failing as `audit.checkpoint_failed`.
- A chain closed before the previous checkpoint is not named again, and `audit-verify --audit-key`
  still fails when that chain is removed with its record.
- A token that refuses to sign, or does not sign within 2 seconds, writes `audit.checkpoint_failed`,
  and admission still answers.
- Without `[audit] key_label` no checkpoint is written, the startup warning is logged, and
  `audit-verify --audit-key` fails naming the missing checkpoints.
