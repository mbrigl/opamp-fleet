# SEC-append-only-audit-record: An append-only audit record of every security decision, one hash chain per lease, its heads signed with the audit key on the token, exportable

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** Markus Brigl
- **Applies to:** the audit port in `crates/fleet-server/src/audit.rs`, its writer and `verify` in `crates/fleet-server/src/audit_log.rs`, its filesystem adapter in `crates/fleet-server/src/fs/audit.rs` and every database adapter beside it, the checkpoints in `crates/fleet-server/src/checkpoint.rs` and `crates/fleet-server/src/postgres/checkpoint.rs`, their column, index and head row in the PostgreSQL schema, the signer port for checkpoints and its PKCS#11 adapter, the advisory lock that elects the signer on PostgreSQL and the connection it is held on, the audit key label each lease records and its startup check in the PostgreSQL adapters, the export in `crates/fleet-server/src/audit_export.rs` and the OpenTelemetry crates it adds to the Server in `crates/fleet-server/Cargo.toml`, every place in `crates/fleet-server/src/` that admits or refuses a peer, issues or revokes a certificate, takes an operator's act or records a package outcome, `server audit-verify`, `server pki status` and the shutdown in `crates/fleet-server/src/main.rs`, the take of a lease in `crates/fleet-server/src/postgres/lease.rs` where it opens and closes chains, the `[audit]` and `[audit.export]` sections of `server.toml` and their parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, and every document that tells an operator how to check the record or what to keep of it

## Context

Every security measure of the Server decides something about a peer:

- admission and its refusal, and the throttle;
- an enrolment request and its approval;
- a certificate issued or renewed;
- a revocation and the sessions it ended
  ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md),
  [SEC-bootstrap-certificates-and-enrolment](SEC-bootstrap-certificates-and-enrolment.md));
- a rollout ([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md));
- a package outcome an Agent reports
  ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)).

Without a record of their own these leave log lines at best. Those are mixed with everything else,
kept as long as whatever collects the log keeps them, and changeable by whoever can write the log.
None of the decisions is then demonstrable afterwards. An investigation cannot say who was admitted
when, who approved a host, or whether a record was removed. It needs exactly one record per
decision, refusals included, and the refusals are the half that is easy to omit.

The Agent plane admits by client certificate alone
([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)). There is no fleet
credential to offer, rotate or revoke, and none for the record to cover. Certificate issuance is a
security decision like any other and waits for its record as admission does.

The specification puts security before convenience and holds that no vulnerability can be ruled
out (Strategy *Security before convenience*). Its *Threats & Forbidden Actions* name the Server's
audit record among what the project protects. A record that an intruder on the Server host can
quietly edit is worth little against exactly that intruder. A record whose entries each carry the
hash of the one before at least makes an edit or a removal visible to whoever checks it against a
copy taken earlier.

Two forces bound the design:

- A refused admission is the cheapest thing a remote peer can cause. A record per refusal must not
  let a flood fill the disk.
- The record must not leak what it records: no secret, no key, no CSR body. The Agent plane reads
  no `Authorization` header, but a Client may still send one unasked. Beyond the loopback a request
  on the Operator plane carries the operator's credential, which the specification's *Threats &
  Forbidden Actions* name for protection. Neither may reach the record.

Where the record is kept, in files or in PostgreSQL, its permissions, how it is written and how
long it goes back are part of the Server's state and its stores
([SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md)). On the filesystem store the
files live under `data_dir/audit/` and rotate by size
([SRV-state-stores-contract-pair §12](SRV-state-stores-contract-pair.md)): a file that reaches
`max_file_bytes` is closed, `keep_files` are kept, and the last hash of a deleted file is recorded
first. Size is the wrong bound for a database. A table has no file to close, and an operator asked
how long the record goes back answers in time, not in megabytes.

On PostgreSQL several Server instances write to one database
([SRV-instances-share-one-postgresql](SRV-instances-share-one-postgresql.md)). Two writers cannot
extend one chain without serialising every security decision of the fleet on one lock across
processes. A chain that one process takes up from another's tail mixes two processes' orders into
one sequence that neither wrote. Every Server on PostgreSQL holds a lease, and takes a new one,
under a new instance id, after it could not renew the old
([SRV-instances-share-one-postgresql §1 and §3](SRV-instances-share-one-postgresql.md)). A chain
drawn with each lease therefore ends where the lease ends. An instance that takes a new lease does
not chain on from entries it wrote under a lease another instance may already have closed. A chain
of a lease opens and ends with the lease. An instance that took a lease without its chain open, or
removed a lapsed lease and left its chain open, would leave a record that contradicts the leases. A
reader expects one global chain and would take several chains for a gap in the record.

An instance also decides while it holds no lease. Its listeners are closed then, and it refuses
every answer, but its background work goes on: an enrolment window runs out, a session ends, a
change another instance made is taken up. Such an entry lands in the chain of a later lease, or of
the lease that just lapsed. Without a mark of its own it reads there as a decision of that lease.
An instance that had to be alone and whose process is gone holds a lease that has not lapsed. The
instance that takes it over closes its chain all the same, and a reason that says the lease lapsed
misnames that close.

On PostgreSQL a write is a transaction, and the answer to its commit can be lost while the commit
holds. The writer cannot tell such a write from one that failed, so it asks the chain what it
holds.

A chain shows an entry changed or removed in its middle. Whoever can write the store can still cut
a chain's tail, remove a whole chain with its record, or rewrite a chain from any point on with
fresh hashes, and `server audit-verify` finds nothing. With PostgreSQL the people who can write the
store are not only those who hold the Server's host: a database administrator, a stolen dump, a
standby copy of the database. A key that signs the record is no help when it lives on the same
host as the record, since whoever can rewrite the record can read the key and re-sign. A key on a
PKCS#11 token cannot be read
([SEC-admission-by-client-certificate §35 and §37](SEC-admission-by-client-certificate.md)), so
whoever holds the store but not the token cannot sign. A signature on every entry would put the
token in the path of every admission. A signature on the heads at intervals bounds what can be
rewritten unseen to one interval, off the decision path. With several Server instances on one
database, one instance at a time signs, elected by an advisory lock any instance can take.

A signature in the store proves what was there when it was made. It cannot prove that nothing came
after it, because whoever can write the store can drop the newest checkpoints together with the
entries they cover. Certificate Transparency answers this with signed tree heads that leave the log
and are compared by others. The record leaves the Server as a log copy (clause 6) and, where
configured, as an export to a system the Server cannot rewrite (clause 15). That export is the only
anchor that holds against a compromised Server.

The chains stand without the checkpoints: a Server without a token keeps them, and they are the
record. A reader would expect a key file to stand in for the token, or a signature on every entry;
neither does (clause 14, *Alternatives considered*).

## Decision

We will append one JSON object per security decision to the Server's audit record, each carrying
the SHA-256 of the entry before it in one chain per lease on PostgreSQL or the one chain of the
filesystem store, emit it as a `tracing` event with the target `audit` and export it where
configured, bound the refusals by aggregation, let `server audit-verify` walk every chain, and
sign a checkpoint of every chain's head about once a minute with the audit key on the PKCS#11
token.

1. **One entry per decision, refusals included.** The Server writes an entry for:
   - admission on `/v1/opamp` and on the download route: admitted, refused, throttled. A refusal
     names which check refused, never the presented value. The Server records a WebSocket session
     and a download every time. It records a plain-HTTP poll once per peer address and certificate
     per hour, since a fleet polling every few seconds would otherwise crowd everything else out of
     the record;
   - enrolment: a request queued, approved, rejected; the window opened, or closed by an operator,
     or run out, with the number of requests that expired with it;
   - issuance: a certificate signed on enrolment or renewal, with its CA role and serial, the
     predecessor, and the `instance_uid` of the request; a CSR refused, with the reason;
   - revocation: a certificate revoked or lifted, and each session it ended; a member refused the
     Gateways' revocation list because its host is not marked as a Gateway, with that host;
   - operator acts:
     - every mutating route of the REST API, with the operator's user name and the route, once
       before it runs and once with its status after;
     - what a revocation, an approval, a rejection, a window act, a Gateway mark or a floor did;
     - a revocation's entry names its certificate by `authority` and `serial`, a Gateway mark's
       entry its host and whether the mark is set, and a floor's entry its Agent type and version;
     - every refused or throttled operator sign-in;
   - packages: a rollout, and each package outcome an Agent reports, installed or failed.

2. **What an entry holds.**
   - `seq`: a counter from 1 within its chain that never repeats there.
   - `time`: RFC 3339, UTC.
   - `event`: a fixed name such as `admission.refused`.
   - `outcome`.
   - The peer address.
   - The Agent's `instance_uid`, where one is known.
   - The presented certificate's CA role and serial, where one is presented.
   - The operator, where one acted.
   - Event-specific fields.
   - `prev`: the SHA-256, hex, of the previous entry of the same chain as written.
   - On the PostgreSQL store, the `chain` the entry belongs to.

   A certificate appears only by role and serial, a CSR only by its key fingerprint. No entry holds
   a secret in any form. The Server never writes an `Authorization` value, not even as a hash,
   whether an operator presented it or an Agent sent it unasked. The Server cuts a text field at
   256 bytes. A user name or a path can come from a peer that was not admitted, and must not be
   able to fill the record.

3. **The chains.**
   - Each Server process writes the entries of its decisions through one writer thread of its own.
     The thread takes them from the channel of clause 5 in the order the decisions were taken.
     [SRV-state-stores-contract-pair §12](SRV-state-stores-contract-pair.md) says where the entries
     are kept.
   - On the PostgreSQL store the Server writes two entries in the transaction of a lease instead:
     - a chain's `audit.chain_opened` in the transaction that takes its lease;
     - the `audit.chain_closed` of a lease another instance takes over, in the transaction that
       removes that lease (clause 7).

     No lease is then held without its chain open, and none is removed with its chain left open.
   - On the filesystem store the record is one chain.
     - The first entry of a file carries the hash of the last entry of the file before it, or 64
       zeros for the first file ever.
     - At startup the Server reads the end of the newest file that holds a line. It takes `seq`
       from the last whole entry and chains on from the last line as it stands.
     - A last line that is not a whole entry, from a crash mid-write, is kept and closed with a
       newline. The next entry records `audit.chain_broken`. A truncation is therefore itself on
       the record, and `seq` never repeats.
   - That `audit.chain_broken` chains on the torn line by its `prev`. It names as `last_whole` the
     hash of the last whole entry before the torn line, or 64 zeros when there is none.
     - A torn piece holds no `prev` of its own, so `last_whole` binds the entries after the repair
       to those before it.
     - `server audit-verify` holds a line that is not an entry only when the next entry is that
       `audit.chain_broken`. It requires `last_whole` to be the hash of the entry before the torn
       line wherever the files hold one. A repair therefore cannot restart the chain anywhere.
     - An `audit.chain_broken` after a torn line that names no `last_whole` is kept as written,
       since an entry is never rewritten. `server audit-verify` holds it and names it as a repair
       without a link, because the entries after it do not bind those before it.
   - A crash during the repair itself leaves a run of torn lines: the torn piece, then a torn
     `audit.chain_broken`.
     - The next start keeps the run and closes it with a newline. It writes one
       `audit.chain_broken` that chains on the last torn line by its `prev`. That entry names as
       `last_whole` the last whole entry before the run, wherever the files hold it, in the same
       file or an earlier one.
     - `server audit-verify` holds a run of torn lines closed by one `audit.chain_broken` whose
       `last_whole` links to the last whole entry before the run.
     - A run closed by an `audit.chain_broken` that names no `last_whole` fails, and so does a run
       nothing closes.
   - On the PostgreSQL store each lease
     ([SRV-instances-share-one-postgresql §1 and §3](SRV-instances-share-one-postgresql.md)) opens
     a chain of its own, which ends with the lease.
     - A Server instance draws a random chain identifier with each lease it takes and opens its
       chain. Opening writes a chain record with the identifier, the host name and the time it was
       opened. It writes a first entry `audit.chain_opened` whose `prev` is 64 zeros.
     - The first chain the store ever opens names, as its `predecessor`, the last entry of the file
       chain a migration left as an archive (clause 9).
     - Every entry the instance writes under that lease belongs to that chain, and the lease names
       it.
     - Clause 7 decides how a chain ends, and how one that ended is told apart from one that was
       cut.
   - On the PostgreSQL store an entry is written when the transaction that inserts it commits. A
     transaction that does not commit is a failed write (clause 5).

4. **Refusals are aggregated, never dropped.** Within one second, the Server counts rather than
   writes the refusals after the tenth of the same `event` from the same peer address. It counts an
   IPv6 address by its /64, as the throttle does. One `…aggregated` entry with the count follows
   when the second ends. A flood of refusals therefore costs at most ten lines and a count per
   address and second, and every refusal is still accounted for.

5. **Writing never blocks admission on a slow disk, and never fails silently.**
   - Entries go through a bounded channel of 10 000 to the writer thread. A decision goes ahead once
     its entry is queued.
   - When the channel is full, the Server refuses admission and every operator act with `503`, and
     issues no certificate until the channel drains.
   - The acts a restore repeats by their entries withdraw trust. They are a revocation, a Gateway
     mark set or cleared, a floor set, a request rejected, and the window closed by an operator.
     Such an act runs only with a place in the channel held for the entry that says what it did:
     - It takes the place before it runs and is refused with `503` when none is free.
     - Once it has run, it writes into the place what it did.
     - An act whose store answered with an error may have been made all the same. It writes its
       place too, with the outcome `unknown` and what it was asked to do.
     - Only an act known to have failed gives its place back unwritten.
     - A window closed by an operator is closed on the instance whatever its store answers, so its
       entry says `closed`.
     - The places held count against the 10 000.
   - Once an orderly shutdown begins to close the record, the Server gives no place. The close
     waits within its bound for the places held to be written. With one still held when the bound
     ends, the close fails, as it does when something queued was not written.
   - When a write fails, the Server logs the error at `error`. From then on it admits nothing,
     issues no certificate and runs no operator act until a write succeeds. The writer retries
     every second.
   - The Server answers a CSR held back for want of a record with `ServerErrorResponse`
     `Unavailable` and `retry_info`. The Server, not the request, is at fault, and the Agent asks
     again.
   - On the filesystem store the entries of a failed write are lost.
   - On the PostgreSQL store the writer keeps them and writes them with its next attempt, in the
     chain of the lease held then. A write whose answer was lost may have committed, so each attempt
     writes only what the chain does not hold yet. None of them is counted as lost.
   - The Server counts the decisions whose entries a failed write lost, or that it refused for want
     of a record. The next entry that lands carries the count as `unrecorded_before`.
   - Once an orderly shutdown has closed the chain on PostgreSQL, the Server refuses a decision for
     want of a record. It logs the count of those at `error` before the process exits.
   - One remainder is counted and may not be logged. Refusals aggregated (clause 4) after the
     close's last flush of the aggregates, and before the chain counts as closed, are counted when
     the writer next flushes the aggregates. That happens at its next tick once their second has
     passed, or as it ends, which may come after the count was logged.
   - The Server never takes a security decision without its record queued. But for those
     refusals, a record that is lost is never lost silently.

6. **The log carries it too.** The Server also emits every entry as a `tracing` event with the
   target `audit`, the same fields, at `info`. A log collector therefore receives it without reading
   the store. The record is the chains of clause 3, one per lease on PostgreSQL, stored in files or
   in PostgreSQL where the Server keeps its state. The log is a copy.

7. **A chain that ended is told apart from one that was cut.**
   - A closed chain ends with `audit.chain_closed`, and its chain record holds that entry's hash.
     - At an orderly shutdown the instance writes it into its own chain, with `reason` `shutdown`.
     - For a lease that lapsed, the instance that takes it over writes it into the chain the lease
       names. It writes `reason` `lease_lapsed` and `closed_by`, the identifier of its own chain
       ([SRV-instances-share-one-postgresql §18](SRV-instances-share-one-postgresql.md)).
     - An instance that had to be alone and whose process is gone holds a lease the next instance
       takes over before it lapses
       ([SRV-instances-share-one-postgresql §2](SRV-instances-share-one-postgresql.md)). For that
       lease the instance taking over writes `reason` `holder_gone` and `closed_by`, the
       identifier of its own chain.
     - No instance writes any other entry into a chain it did not open.
     - Entries missing from a closed chain's end, a last entry that is not the recorded one, or an
       entry after it fail verification.
   - A closing write at shutdown whose answer was lost may have committed. The instance reads its
     chain back. When the chain holds its closing entry, the close was written, and the instance
     releases the lease. Once its chain is closed, the instance records nothing more. It refuses a
     decision taken after the close for want of a record. Before it exits, it logs at `error` how
     many decisions went unrecorded so.
   - The Server reports a chain without a closing entry as open, with its last entry. It is the
     chain of a lease that is held, or of one that lapsed and that no instance has taken over yet.
   - An instance may queue entries under a lease that lapses before they are written. Those go into
     the chain of its next lease, first after its `audit.chain_opened`. Each carries `taken_under`,
     the identifier of the chain whose lease it was decided under. The chain they are written into
     covers them, and the field says under which lease they were decided. None is counted as lost,
     and none is written into a chain the instance did not open.
   - An entry whose decision was taken while the instance held no lease carries `taken_under`
     `none`, in whichever chain it is written into, that of the lease that just lapsed included.
     - The instance holds no lease from the moment its lease is lapsed by the clock it measures it
       on ([SRV-instances-share-one-postgresql §3](SRV-instances-share-one-postgresql.md)). It
       also holds none after it released its lease. Either lasts until it holds a new one.
     - The entries the writer adds of its own carry no `taken_under`. They are the aggregates,
       `audit.resumed`, `audit.chain_broken` and the closing `audit.chain_closed`.
   - The store itself does not detect the removal of a whole chain together with its chain record.
     The checkpoints of clauses 10 to 14, signed with a key the store does not hold, detect it.
   - Pruning leaves one more cut that the store itself does not detect.
     - Whoever can write to the database can place one `audit.pruned` with a correct hash in a
       chain that then holds. Examples are the end of an open chain, or of a closed one whose chain
       record they rewrite.
     - That entry can name any chain and any of its entries as the last removed. They can then
       delete that chain's entries up to it, younger ones included.
     - `server audit-verify` accepts the link, because it cannot tell the age of what is gone.
     - It takes links and a carried predecessor only from an `audit.pruned` in a chain whose own
       walk holds. An `audit.pruned` in a chain that fails vouches for nothing.
     - The checkpoints of clauses 10 to 14 detect the cut. Without them, a record that holds shows
       that what is left is linked, not that no entry younger than `retain_days` was removed.

8. **`server audit-verify` walks every chain.**
   - `server audit-verify <data_dir>/audit` walks the files of the filesystem store.
   - `server audit-verify --config <server.toml>` reads the PostgreSQL store with the Server's own
     credentials and walks each chain in `seq` order. It checks that:
     - every `prev` matches the entry before;
     - the first retained entry is `audit.chain_opened`, or its `prev` is the `last_hash`, and its
       `seq` the one after the `last_seq`, of a retained `audit.pruned` that names that chain as its
       `pruned_chain`;
     - a closed chain ends on its recorded hash;
     - a closed chain with no entries left has as its closing hash the `last_hash` a retained
       `audit.pruned` names for it.
   - An `audit.pruned` links only when the chain it stands in holds. A chain whose walk needs no
     link from another chain, or only links from chains that hold, holds. The walk repeats until no
     further chain does.
   - The command reads the predecessor from the first chain's `audit.chain_opened`. Once that entry
     is pruned, it reads it from a retained `audit.pruned` that carries it on in a chain that holds,
     by the same rule as the links.
   - The command names the chain and the first entry that fails, lists every open chain, and exits
     non-zero on any failure.

9. **A record moved from files to the database stays linked.**
   - The files under `data_dir/audit/` that a Server wrote on the filesystem store stay where they
     are as a closed archive.
   - The Server neither moves them nor writes to them again. `server audit-verify <dir>` verifies
     them as before. Deleting them is the operator's.
   - `server migrate-to-postgres`
     ([SRV-state-stores-contract-pair §27](SRV-state-stores-contract-pair.md)) records the `seq`
     and the hash of the archive's last entry in the database.
   - The first chain the database store opens names them in its `audit.chain_opened` as its
     `predecessor`.
   - `server audit-verify --config <server.toml> --archive <dir>` checks that the archive ends on
     the entry the predecessor names. Without `--archive` it reports the predecessor as not
     checked.

10. **The audit key signs a checkpoint that names the head of every chain.** `[audit] key_label`
    names a key on the token that signs checkpoints of the heads of every audit chain. Clauses 10
    to 14 decide what a checkpoint holds, when and by which process it is signed, where it is kept,
    and how `server audit-verify` checks it. A checkpoint is the audit entry `audit.checkpoint`. It
    carries the fields of every entry (clause 2) and these:
    - `checkpoint_seq`, a counter from 1 across the whole record that never repeats, and
      `checkpoint_prev`, the SHA-256, hex, of the previous checkpoint entry as written, or 64 zeros
      for the first;
    - `heads`: for every open chain, and for every chain closed or pruned since the previous
      checkpoint, its identifier, the `seq` of its last entry, that entry's hash, and whether the
      chain is closed.
      - A chain closed before the previous checkpoint is named in that one. A verifier carries its
        closed head forward from it. A checkpoint therefore grows with the chains that are open,
        not with every lease ever taken.
      - The filesystem store's one chain is named `fs`.
      - For the signing process's own chain the head is the entry before the checkpoint;
    - `key`: the audit key's label and the SHA-256, hex, of its public half as DER
      `SubjectPublicKeyInfo`;
    - `sig`: the signature, as a DER `ECDSA-Sig-Value` in base64. It is ECDSA P-256 over the
      SHA-256 of the entry's line as written without `sig`. One `C_Sign` makes it, as
      [SEC-admission-by-client-certificate §37](SEC-admission-by-client-certificate.md) makes every
      signature.

    The entry is the audit entry that
    [SEC-admission-by-client-certificate §38](SEC-admission-by-client-certificate.md) asks for every
    signature. It chains as any other entry: its `prev` covers it, and the next entry covers its
    hash. Checkpoints are rotated and pruned with the record:
    - An `audit.rotated` or `audit.pruned` entry that removes checkpoints names, besides what it
      names of the entries, the `checkpoint_seq` and the hash of the last checkpoint it removed.
    - A prune removes no checkpoint while an older one is still held in a chain it does not prune
      with it. It ends that chain's run before such a checkpoint. The checkpoints the store holds
      therefore always follow each other.
    - A prune does not remove the checkpoint that named a closed chain closed while that chain still
      holds entries the prune leaves, since no later checkpoint names the chain.
    - A prune repeats its rounds until a round frees nothing more. The chains one instance prunes,
      its own and the closed ones, therefore drain in one prune.
    - A chain another running instance holds is pruned by that instance at its own time of day.
      After the signer changed between running instances, entries may outlive
      `[audit] retain_days` by up to a day for each change since the oldest checkpoint left.
    - The filesystem store's one chain rotates its checkpoints in order.

11. **One process signs, every 60 seconds and at an orderly shutdown.**
    - On the filesystem store the Server signs.
    - On PostgreSQL the instance that holds a session-level advisory lock signs.
      - Every instance names the same `[audit] key_label` (clause 16) and tries to take the lock at
        every interval. A lock that ends with its connection passes to the next instance that
        tries.
      - Each instance holds the lock on one connection of its own, outside the pool. The signer
        checks that connection at every interval.
      - An instance signs only while its lease is live.
      - Both ends of that connection count it dead after about 30 seconds without an answer, by TCP
        keepalives and a TCP user timeout. The lock of a signer cut off from the database therefore
        passes on well within a minute.
    - The signer reads the heads from the store and writes the checkpoint into its own chain. It
      skips a checkpoint when no chain grew since the last one, apart from that checkpoint itself.
    - The store holds at most one checkpoint per `checkpoint_seq`, by a unique index. It inserts a
      checkpoint only while its `checkpoint_prev` names the newest one.
      - On PostgreSQL the entry's `checkpoint_seq` is a column of its own under a partial unique
        index.
      - One row holds the newest checkpoint's `checkpoint_seq` and hash. The transaction that
        inserts a checkpoint compares and sets that row under `FOR UPDATE`, so a prune does not
        lose it.
      - A signer that lost its lock while a checkpoint was in flight therefore cannot fork the
        checkpoints. The insert of the checkpoint that came second fails, and its signer writes
        `audit.checkpoint_failed`.
    - A checkpoint is written alone, in a write of its own. One whose write fails for any reason is
      dropped and never written again, since its `sig` covers its place. `audit.checkpoint_failed`
      names the error, and the next interval signs afresh.
    - At an orderly shutdown the signer writes a last checkpoint before `audit.chain_closed`. A
      process that is not the signer closes its chain, and the signer's next checkpoint names the
      closed head.
    - Signing runs beside the writer thread and never delays a decision.
      - A thread of its own reads the heads and asks the token.
      - The writer gives the checkpoint its place and waits for the signature at most 2 seconds,
        while decisions go on queueing for it.
      - The writer does not wait for a signature that does not come in time. It takes the
        checkpoint back, and `audit.checkpoint_failed` names `timeout`.
      - When the token does not sign, the Server logs at `error`, writes `audit.checkpoint_failed`
        with the token's error, and tries again at the next interval.
      - Decisions go on: the chains stay the record, and only the anchor waits.

12. **The audit key's public half is printed and kept.** `server pki status` prints the audit key's
    label, its public half as PEM, and the SHA-256 the checkpoints carry in `key`. The operator
    keeps the PEM where the Server cannot rewrite it. A verifier checks against it.

13. **`server audit-verify` checks every chain against the checkpoints.** With `--audit-key <file
    holding the public key as PEM>`, over the files and with `--config` alike, the command checks
    the following besides the checks of clause 8:
    - Only a signed removal counts.
      - The command honours an `audit.rotated` or `audit.pruned` entry only where a later
        checkpoint that verifies names a head of its chain at or after it. The store must still hold
        that head with the hash the checkpoint names. The chain's links from the removal to the head
        then prove that the removal was there when the head was signed.
      - The command reports, and never honours, such an entry after every such head, in the
        unsigned tail or inside a run that was cut. Whoever can write the store therefore cannot
        excuse a removal by adding the entry that would explain it.
    - Every checkpoint's `sig` verifies against the given key, and its `key` names that key.
    - The checkpoints follow each other by `checkpoint_seq` and `checkpoint_prev`. The first
      checkpoint the store still holds follows the last removed one that a signed removal names. Or
      it is the first ever, with `checkpoint_seq` 1 and 64 zeros.
    - Every head any checkpoint names, not only the newest one's, is in the store. Its chain reaches
      at least the `seq` it names and has at that `seq` the hash it names. The exception is a part
      whose removal a signed removal recorded
      ([SRV-state-stores-contract-pair §12](SRV-state-stores-contract-pair.md)). A chain whose first
      entries are gone begins right after the last entry a signed removal names.
    - An entry written more than three intervals after the newest checkpoint, with no
      `audit.checkpoint_failed` between, fails as an unsigned tail. A running signer would have
      covered it.

    `--since <file>` names a file holding a checkpoint line the operator saved from an earlier run,
    the log copy or the export.
    - The store's checkpoints must hold that one unchanged and continue from it.
    - A saved checkpoint that rotation or pruning has removed must match the last removed checkpoint
      a signed removal names, when it is that one.
    - The command reports a saved checkpoint older than that as no longer held. It goes on from the
      oldest checkpoint the store holds.

    The command names the first failure by chain, `seq` and checkpoint. It prints the newest
    checkpoint's line after `checkpoint: `, for the operator to keep as the next `--since`. It exits
    non-zero on any failure. Without `--audit-key` it checks the chains alone and says that it
    checked no signature.

14. **Without a token, the chains are the record.**
    - Without `[audit] key_label` the Server writes no checkpoint. The record is then the chains of
      clauses 3 and 7 alone.
    - The Server logs at startup, at `warn`, that the audit record's heads are not signed.
    - The audit key has no key file. A key that whoever can rewrite the record can also read would
      sign the rewrite.
    - `server audit-verify --audit-key` on a record without a checkpoint fails, naming the missing
      checkpoints. A record whose checkpoints were removed whole therefore does not pass as one
      that never had them.

15. **The audit record may be exported, off by default.**
    - `[audit.export]` names an OTLP/HTTP endpoint. The Server sends every entry to it as an OTLP
      log record.
    - The export is an outbound connection of
      [SEC-tls13-two-planes-bounded-input §21](SEC-tls13-two-planes-bounded-input.md). The
      configuration names it, and this clause decides its purpose.
    - It runs over TLS 1.3 by the provider of that ADR's clause 5, and in plaintext only to a
      loopback endpoint.
    - The section has two keys:
      - `endpoint` is `https://`, or `http://` to a loopback IP literal alone;
      - `ca_file` names one PEM root for a receiver whose certificate no public root signs.
    - The Server authenticates to the receiver with nothing beyond TLS. Each request carries no
      header beyond `Content-Type` and `Content-Encoding`.
    - The export takes no setting from an `OTEL_*` variable. The configuration and the code set its
      endpoint, protocol, headers, compression and timeout.
    - The Server exports an entry after the store has accepted it. The record carries the line the
      store holds, with its sequence number, time and previous hash, so the receiver can walk the
      chain itself.
    - PostgreSQL may hold an entry to try again (clause 5). The Server exports such an entry once
      it is written.
    - The Server encodes and sends the records with the OpenTelemetry crates of the Client's own
      telemetry ([CLT-what-an-agent-reports](CLT-what-an-agent-reports.md)).
    - A queue of the export's own holds 2048 entries, a named constant. The export retries on its
      own and never blocks a decision.
    - The Server counts each entry the export drops while its queue is full. It writes the count as
      the audit entry `audit.export_dropped` with `count`. It writes that entry about every 60
      seconds while the count is not zero, and once at shutdown.
    - Two kinds of entry reach neither the receiver nor a count in the record:
      - an entry dropped after that last count;
      - an entry still waiting when the shutdown's five seconds run out.

      The Server logs both numbers, and the receiver sees a gap in `seq`.
    - A system that the Server cannot rewrite is the only anchor that holds against a compromised
      Server.

16. **Every instance on one database names the same audit key.**
    - Every instance on one database names the same `[audit] key_label`, or none.
    - Each lease records the label.
    - An instance whose label differs from that of a live lease fails startup, naming the key. A
      label against none counts as differing.
    - An instance checks the label again whenever it takes a new lease after its own lapsed. One
      that finds a differing label stops as at a failed start, as it does on a schema it does not
      understand
      ([SRV-instances-share-one-postgresql §16](SRV-instances-share-one-postgresql.md)).
    - The audit key has no file form (clause 14).

**Out of scope:**

- An audit record on the Client.
- Shipping the record to a remote store other than the export of clause 15.
- Signing individual entries, and signing with a key outside a PKCS#11 token.
- A REST route to read the record or its checkpoints.
- Retention by age on the filesystem store.
- The leases themselves, and how an instance finds one that lapsed
  ([SRV-instances-share-one-postgresql](SRV-instances-share-one-postgresql.md)).
- Copying the archived filesystem record into the database, which clause 9 links instead.
- Where a saved checkpoint is kept and who compares it, which is the operator's.
- A transparency log or a witness that cosigns checkpoints.
- A configurable checkpoint interval.
- What the export's receiver does with the entries.
- The token itself, its `[pkcs11]` section and SoftHSM2, which are
  [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)'s (clauses 35 to
  41).
- Where the record is kept, its permissions, its writing, and how long it goes back, which are
  [SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md)'s.
- Reading the record on the Operator plane. Without a rights model every operator who passes the
  guard would read it, where a rights model would keep it from those who only read or prepare.
- A chain for each run of a local command that changes state, such as `server revoke <serial>` or
  `server enrolment close`. Such a run holds no lease, and its authentication is shell access on a
  Server host.

## Alternatives considered

- **A failed write on PostgreSQL loses its entries, which the next entry counts as
  `unrecorded_before`, as on the filesystem store.** One rule for both stores. But a commit whose
  answer was lost may hold all the same, so the writer cannot tell a lost entry from one written,
  and the count would name as lost what the record holds. A database that is briefly away would
  cost entries that could still be written. Rejected; clause 5 keeps them and writes what the chain
  does not hold yet.
- **Writing `audit.chain_opened` and the close of a lapsed lease through the writer thread.** One
  writer for every entry, but a lease would be held, or removed, for as long as the writer takes,
  without its chain opened or closed.
- **Only the normal log, under a target of its own.** Retention and integrity would depend on
  whatever collects the log, and a host without a collector keeps nothing an investigation can rely
  on.
- **OTLP logs to a collector instead of a record of the Server's own.** Central, but a Server
  without a reachable collector loses exactly the record an outage investigation needs. The export
  of clause 15 sends a copy of the record, which the Server keeps all the same.
- **Entries without a chain.** Simpler, but an edit or a removal is invisible.
- **Signing each entry with a key.** It proves more than a chain. But a key file would live on the
  same host as the record, so an intruder who can rewrite the record can re-sign it. With the key
  on the token, one `C_Sign` per decision puts the token in the path of every admission and makes a
  slow token a slow fleet. Rejected; a checkpoint a minute bounds what can be rewritten unseen to
  that minute.
- **Dropping refusals under load.** Cheaper, but an attack is exactly when refusals matter;
  aggregation keeps the count.
- **An audit record on the Client as well.** A second format to maintain. The Client reports its
  package outcomes to the Server, which records them.
- **One chain for the whole record, whatever the store, serialised in the database.** It keeps one
  sequence. But two writers cannot extend one chain without serialising every security decision of
  the fleet on one lock across processes, and the order it records is the database's commit order
  rather than any instance's. Rejected; on PostgreSQL each lease writes a chain of its own, and the
  filesystem store keeps one.
- **One chain per host, taken up again after a restart.** It gives a chain a stable name. But a
  process taking up another's tail would chain on from an entry it did not write, which a restart
  after a crash makes fragile on the filesystem. Rejected; a chain per lease opens and closes with
  the lease.
- **One chain per process, kept across leases.** A process whose lease lapsed would go on writing a
  chain that the instance taking over its lease may already have closed. Rejected; a new lease
  opens a new chain.
- **A repair of a torn line that chains on it and links to nothing before it.** The anchor at the
  end of a record would bind the entries after a repair, not those before it, and a repair could
  restart the chain anywhere. Rejected; clause 3 has the repair name `last_whole`, which the
  verifier checks.
- **No mark on an entry decided under no lease.** It is what an entry decided under the lease of
  its own chain looks like, so a decision of background work after a lapse reads as one of the next
  lease. Rejected.
- **A field of its own naming the lease held last before.** It says between which leases the
  decision fell. But the writer would have to keep that lease beyond the moment the instance loses
  it, and the times of the entries and of the next `audit.chain_opened` say as much. Rejected;
  `taken_under` `none` keeps one field for the question under which lease.
- **Aggregates keyed by the lease of the refusals they count.** An aggregate placed after the
  writer took up a new chain counts refusals of which some may have been decided under the lease
  before, and it reads as of the new one. Keying the count by lease would split one second's
  refusals of one peer into two aggregates for that narrow case, while the listeners are closed and
  only the refusals in flight are counted. Rejected; the aggregate stays the writer's own entry and
  carries no `taken_under`.
- **`reason` `lease_lapsed` for the close of a lease whose holder is gone.** It needs no new value,
  but that lease has not lapsed, and the record would misname why its chain ended. Rejected.
- **A chain whose entries carry no mark of the lease they were decided under, and whose close
  always names a lapsed lease.** An entry an instance decides while it holds no lease would read as
  a decision of the lease whose chain it lands in. The close of a lease whose holder is gone would
  be misnamed. A closing write whose answer was lost would stay undecided, and an orderly shutdown
  would leave its chain open. Rejected; clause 7 marks those entries, names each close by its
  cause, reads a lost close back, and ends the record at shutdown with the close.
- **Retention by age on the filesystem store too.** One rule for both stores. But the filesystem
  store's bound is the disk it shares with the rest of `data_dir`, and size is what keeps that disk
  from filling. Rejected; each store keeps the bound that fits it.
- **Retention by count of entries in the database.** As far from the operator's question as size
  is. Rejected.
- **Detecting a deleted chain by a sequence of chain records.** A gapless sequence of chain records
  in the same database is deleted as easily as the chains it numbers. Only a signature with a key
  outside the database detects it, which the checkpoints of clauses 10 to 14 give. Rejected.
- **A key file for the audit key when no token is configured.** Every installation would have
  signed heads, but the key would sit beside the record it protects. Rejected; without a token the
  heads are unsigned, and the Server says so.
- **Each process signs only the head of its own chain.** No lock is needed. But nobody misses a
  chain removed whole together with its checkpoints, since no other chain names it. Rejected; one
  checkpoint names every chain.
- **Checkpoints in a table or file of their own.** It keeps them apart from the entries. But a
  separate store is one more thing to bound, back up and prune, and nothing chains it to the
  record. As an entry the checkpoint is kept, rotated and pruned with the record, and the chain
  covers it. Rejected.
- **A Merkle tree over all entries, as Certificate Transparency signs.** It allows proofs of
  inclusion for one entry without the whole record, which nobody here asks for; the verifier walks
  every chain anyway. Rejected for the linear heads the chains already have.
- **Failing verification whenever the newest checkpoint is old.** A Server that was stopped would
  fail every check of its record. Rejected; only entries after the newest checkpoint make a tail
  unsigned.

## Sources / Prior art

- [`HARDENING.md`](../HARDENING.md), the entry *An audit record of every security decision*.
- [OWASP Logging Cheat
  Sheet](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html): which events to
  record, and never to record secrets.
- [JSON Lines](https://jsonlines.org/); [RFC 3339](https://www.rfc-editor.org/rfc/rfc3339).
- Hash-chained logs as in [RFC 6962](https://www.rfc-editor.org/rfc/rfc6962) (Certificate
  Transparency), in their simplest linear form.
- [Certificate Transparency, RFC 9162](https://www.rfc-editor.org/rfc/rfc9162): many independent
  append-only logs, each verified on its own, with signed tree heads as checkpoints and as the
  anchor outside the log. Their comparison outside the log is what detects a log that was cut or
  forked.
- PostgreSQL documentation, [advisory
  locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS): a
  session-level lock ends with its connection.
- [PKCS #11 Specification Version
  3.1](https://docs.oasis-open.org/pkcs11/pkcs11-spec/v3.1/pkcs11-spec-v3.1.html): `CKM_ECDSA`
  and its `r || s` signature.
- RFC 5480 and RFC 5758: the DER `ECDSA-Sig-Value` a checkpoint's `sig` carries.
- The code: `AuditStore`, `Limits`, the writer and `verify` in
  `crates/fleet-server/src/audit_log.rs`, the filesystem adapter in
  `crates/fleet-server/src/fs/audit.rs`, and `audit_verify` in `crates/fleet-server/src/main.rs`.

## Consequences

- Positive: an incident becomes an investigation. The record says who was admitted, refused,
  enrolled, approved, issued, revoked and rolled out to, when and by whom, with an edit or a removal
  visible against an earlier copy.
- Positive: several Server instances write the record without waiting on each other, and each
  chain is verified on its own.
- Positive: the filesystem store keeps one chain in files that rotate by size, verified file by
  file.
- Positive: retention on the database is stated in the unit an operator is asked about.
- Positive: with a token, whoever can write the store but cannot use the token cannot cut, remove
  or rewrite a chain before the newest checkpoint without `audit-verify` naming it. The check shows
  a rewrite in the database to anyone holding the audit key's public half.
- Positive: the check needs only the public half and the record. A saved checkpoint extends it to a
  cut of the newest checkpoints themselves.
- Positive: the export can put the record where the Server cannot reach it.
- Negative / trade-offs: a write per decision on the admission path. A disk that stops writing
  stops admission and issuance (clause 5), which is the intended order of failure.
- Negative / trade-offs: the record grows with the fleet's traffic. It is bounded by size in files,
  so a large fleet keeps less time there than a small one, and by age in PostgreSQL.
- Negative / trade-offs: an entry is never rewritten. A record written by an earlier release keeps
  what that release wrote in its chain, such as `rotation.offered` entries or credential hashes cut
  to eight hex digits.
- Negative / trade-offs: the record has as many chains as the Server took leases, at least one per
  restart. Reading it in the order decisions were taken across instances means merging by `time`,
  which no chain guarantees across hosts.
- Negative / trade-offs: without the checkpoints, that is without a token or in a check without
  `--audit-key`, whoever can write to the database can remove a whole chain and its chain record
  without the verification noticing.
- Negative / trade-offs: without the checkpoints, whoever can write to the database can also cut
  the head of any chain with one `audit.pruned` appended to an open chain, young entries included,
  and the verification holds.
- Negative / trade-offs: an open chain does not say whether its instance still runs. Until its
  lease lapses and another instance closes it, a dead instance's chain reads as a running one's.
- Negative / trade-offs: what was written after the newest checkpoint, up to one interval, is
  protected by the chain alone. A cut of the record together with its newest checkpoints is found
  only against a checkpoint that left the store.
- Negative / trade-offs: a compromised Server can have the token sign whatever it wrote. The export
  and the log copy show that afterwards, beside the `pkcs11.signed` entries of
  [SEC-admission-by-client-certificate §38](SEC-admission-by-client-certificate.md).
- Negative / trade-offs: a checkpoint entry a minute while the fleet is busy, and a read of every
  chain's head per checkpoint on PostgreSQL.
- Follow-ups: reading the record through the REST API; an audit record of the Client's own
  decisions; handing checkpoints to a witness or transparency log; a glossary entry for the
  checkpoint; a token file with which the Server authenticates to the export's receiver.

## Enforcement

Tests that carry `Verifies: SEC-append-only-audit-record`, against SoftHSM2
([SEC-admission-by-client-certificate §39](SEC-admission-by-client-certificate.md)) where a token
is needed:

- `crates/fleet-server/src/audit_log.rs` tests: `each_entry_carries_the_hash_of_the_one_before`,
  `the_chain_survives_a_restart_and_a_rotation`, `a_truncated_file_is_recorded_as_a_broken_chain`,
  `refusals_beyond_ten_a_second_are_aggregated_with_their_count` (clauses 2, 3, 4),
  `a_write_that_fails_refuses_until_one_succeeds`,
  `a_held_place_counts_against_the_channel_until_it_is_written_or_given_back`,
  `the_close_waits_for_the_places_held_and_gives_none_once_it_began` (clause 5);
  `crates/fleet-server/src/api.rs` tests
  `a_revocation_whose_store_answer_was_lost_is_recorded_as_unknown` and
  `an_act_withdrawing_trust_whose_store_answered_an_error_is_recorded_all_the_same` (clause 5).
- `crates/fleet-server/src/fs/audit.rs` test:
  `the_record_is_owner_only_and_continues_where_it_ended` (clause 3; the file's permissions are
  [SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md)'s).
- The repair's link (clause 3): `crates/fleet-server/src/audit_log.rs` tests
  `a_truncated_file_is_recorded_as_a_broken_chain`,
  `a_torn_line_holds_only_when_the_next_entry_records_it_as_broken` and
  `a_repair_without_a_link_holds_and_is_named`; `crates/fleet-server/src/fs/audit.rs` tests
  `a_line_torn_inside_a_character_verifies_as_the_store_hashed_it`,
  `a_line_with_an_invalid_byte_inside_is_refused_naming_it`,
  `a_torn_line_that_opens_a_file_is_linked_to_the_file_before` and
  `a_crash_during_the_repair_is_closed_by_one_linked_repair`;
  `crates/fleet-server/src/postgres/audit.rs` tests
  `an_archive_the_file_store_repaired_after_a_crash_is_linked` and
  `an_archive_whose_entries_before_a_repair_were_replaced_fails_audit_verify`.
- `crates/fleet-server/tests/mutual_tls.rs` tests (clauses 1, 5):
  `an_admission_and_its_refusal_each_leave_one_entry`, which refuses a revoked certificate,
  `an_issued_certificate_is_recorded_with_its_predecessor`,
  `a_revocation_records_the_sessions_it_ended`, `an_operator_act_names_the_operator`,
  `an_audit_that_cannot_write_refuses_admission`,
  `a_revocation_whose_entry_cannot_be_queued_is_not_made`,
  `a_revocation_is_recorded_with_its_certificate`,
  `an_act_withdrawing_trust_whose_entry_cannot_be_queued_is_not_made`,
  `an_act_withdrawing_trust_is_recorded_with_what_a_restore_needs`; on PostgreSQL also
  `crates/fleet-server/src/postgres/audit.rs` test `a_revocation_is_recorded_with_its_certificate`.
  `an_operator_act_names_the_operator` also asserts that no entry holds the operator's password.
- `crates/fleet-server/tests/mutual_tls.rs`:
  - `an_authorization_an_older_client_sends_is_never_written`: an Agent admitted on its
    certificate while sending an `Authorization` header leaves an admission entry that holds
    neither the value nor any hash of it (clause 2);
  - `an_audit_that_cannot_write_issues_no_certificate`: with the record unavailable, a CSR on a
    member connection gets no certificate and is answered `Unavailable` with `retry_info`
    (clause 5);
  - `an_unmarked_member_asking_for_the_list_leaves_a_refusal` (clause 1).
- `crates/fleet-server/src/fleet/session.rs`: `unavailable_tells_the_agent_when_to_retry`
  (clause 5).
- A failed write on PostgreSQL is kept and written once, whether or not its answer was lost:
  `crates/fleet-server/src/postgres/audit.rs` tests
  `a_write_whose_answer_was_lost_is_not_written_again` and
  `a_landed_write_is_not_written_again_into_the_next_chain` (clause 5). A decision after the
  shutdown's close is refused and counted, and an aggregate that falls due after it is counted:
  `a_decision_after_the_shutdowns_close_is_refused_and_counted`,
  `an_aggregate_past_the_shutdowns_close_is_counted_and_keeps_the_count` (clause 5). No test reads
  the count from the log line the process writes before it exits.
- The chains per lease on the PostgreSQL store (clauses 2, 3, 7 to 9):
  - on the filesystem store, the tests of the chain, the rotation and `audit-verify` named above
    hold;
  - on the PostgreSQL store, two writers on one database each open a chain, and `audit-verify`
    passes over both; an instance that takes a new lease opens a new chain with it;
  - an entry altered, removed from the middle, or removed from the end of a closed chain, or an
    entry after its `audit.chain_closed`, fails `audit-verify`, naming the chain and the entry; an
    open chain is listed, not failed;
  - entries older than `retain_days` are removed, `audit.pruned` records the last removed hash,
    and `audit-verify` still passes;
  - with two instances, one instance at a time removes the entries older than `retain_days` of a
    chain closed at a shutdown and of one closed for a lapsed lease. Its `audit.pruned` entries
    name both chains, and a closed chain with no entries left keeps its chain record;
  - when the `audit.pruned` entries themselves and the first chain's `audit.chain_opened` are
    pruned, the links still needed and the predecessor are recorded again, and `audit-verify`
    still passes;
  - `retain_days` with `store = "fs"`, or `keep_files` or `max_file_bytes` with
    `store = "postgres"`, fails startup naming the key;
  - with the database held back past an instance's lease, the decisions it queued under that
    lease are written into its next chain with `taken_under` naming the old one, and
    `audit-verify` passes over both chains;
  - a pruned chain whose first retained entry's `prev` is not the hash its `audit.pruned`
    recorded, and a closed chain with no entries left whose closing hash differs from that hash,
    fail `audit-verify`; so does a chain linked only by an `audit.pruned` in a chain that fails,
    and an archive whose predecessor only such an `audit.pruned` carries on;
  - after `server migrate-to-postgres`, the first chain's `audit.chain_opened` names the
    archive's last entry; `audit-verify --archive` passes over the untouched archive and fails,
    naming the predecessor, when the archive's last entry was changed or removed;
  - a decision recorded while the instance holds no lease, by `held` or by its clock, is written
    into the next chain with `taken_under` `none`, and `audit-verify` passes over the record;
  - an instance takes over the live lease of one that had to be alone and whose lock is free, and
    closes its chain with `reason` `holder_gone`;
  - a closing write at shutdown that landed and whose answer was lost counts as written; a
    decision recorded after the close is refused and counted.
- The checkpoints (clauses 10 to 14, 16):
  - with `[audit] key_label` on the filesystem store, a checkpoint is written within an interval
    and at shutdown, names the chain's head, and verifies against the key `server pki status`
    prints;
  - `audit-verify --audit-key` passes over an untouched record. It fails, naming the chain and
    the checkpoint, in each of these cases:
    - an entry before the newest checkpoint is changed;
    - a chain's tail before it is cut;
    - a chain it names is removed with its chain record;
    - a checkpoint's signature does not verify;
  - an entry written more than three intervals after the newest checkpoint fails as an unsigned
    tail, unless `audit.checkpoint_failed` entries lie between;
  - on either store, a chain removed whole or cut at its start, with an `audit.pruned` or
    `audit.rotated` added after the newest checkpoint to explain it, still fails: the added entry
    is reported and not honoured;
  - with checkpoints in two chains after a signer change, a prune of the chain that holds the
    newer ones stops before them while the other chain holds older ones, and
    `audit-verify --audit-key` passes after each prune. After several signer changes, one prune by
    the instance that prunes the closed chains frees every old checkpoint in its rounds. A prune
    keeps the checkpoint that named a closed chain closed while that chain holds entries, so the
    chain removed afterwards still fails;
  - `--since` with a saved checkpoint fails when the store does not hold it and no rotation or
    pruning removed it. After a prune that removed checkpoints, `audit-verify --audit-key` passes
    from the first checkpoint left. A saved checkpoint the prune removed last is matched against
    the hash the `audit.pruned` entry names, and a first checkpoint left that does not follow that
    one fails;
  - on PostgreSQL, with two instances, one signs at a time, each checkpoint names both chains,
    and the other takes over when the signer's connection ends;
  - a signer that loses its lock while a checkpoint is in flight leaves one checkpoint per
    `checkpoint_seq`, the second insert failing as `audit.checkpoint_failed`;
  - a chain closed before the previous checkpoint is not named again, and
    `audit-verify --audit-key` still fails when that chain is removed with its chain record;
  - a token that refuses to sign, or does not sign within 2 seconds, writes
    `audit.checkpoint_failed`, and admission still answers;
  - without `[audit] key_label` no checkpoint is written, the startup warning is logged, and
    `audit-verify --audit-key` fails naming the missing checkpoints;
  - on PostgreSQL, a second instance whose `[audit] key_label` differs from the first's, or that
    names none beside one that names a label, fails startup naming the key, and two instances
    without it both start: `crates/fleet-server/src/postgres/lease.rs` test
    `instances_on_one_database_name_the_same_audit_key` (clause 16).
- The export (clause 15), in `crates/fleet-server/tests/audit_export.rs`:
  - `without_an_export_section_the_server_opens_no_export_connection`: without `[audit.export]`
    the Server opens no export connection;
  - `a_recorded_decision_reaches_the_export_receiver_as_the_stored_line`: with it, a decision
    recorded reaches the receiver as a log record carrying the stored line;
  - `the_export_sends_no_header_the_environment_names`: with `OTEL_EXPORTER_OTLP_HEADERS` or
    `OTEL_EXPORTER_OTLP_LOGS_HEADERS` naming an `Authorization` header, the receiver sees no such
    header;
  - `a_silent_receiver_blocks_no_decision_and_the_dropped_count_reaches_the_record`: with
    `[audit.export]` pointing at an unreachable endpoint, admission and enrolment still answer,
    and the dropped count reaches the audit record;
  - `over_https_the_export_trusts_ca_file_and_speaks_tls_1_3_alone`;
  - `crates/fleet-server/src/config.rs` test
    `the_audit_export_is_off_unless_named_and_never_plaintext_off_the_loopback`;
  - `crates/fleet-server/src/audit_export.rs` tests
    `a_full_queue_drops_counts_and_writes_the_count_into_the_record` and
    `at_shutdown_the_export_answers_what_it_leaves_behind`;
  - `crates/fleet-server/src/audit_log.rs` test
    `the_export_is_handed_each_line_the_store_accepted_as_it_holds_it` and
    `crates/fleet-server/src/postgres/audit.rs` test
    `the_export_is_handed_every_line_once_it_is_in_a_chain`.
