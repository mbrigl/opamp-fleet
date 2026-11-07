# ADR-draft-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision: The Server's state lives under one data directory or in PostgreSQL and S3, one store per domain, and an approval records its certificate with its decision

- **Status:** 🟡 proposed
- **Date:** 2026-11-03
- **Deciders:** Markus Brigl
- **Applies to:** every storage port in `crates/fleet-server/src/` (`agent_store.rs`, `configs.rs`, `deployments.rs`, `packages.rs`, `blobs.rs`, `bootstrap.rs`, `enrolment.rs`, `revocation.rs`, `audit.rs`) and every adapter under `crates/fleet-server/src/fs/`, `crates/fleet-server/src/postgres/` and `crates/fleet-server/src/s3.rs`, the embedded schema and its migrations, the wiring in `crates/fleet-server/src/lib.rs` and `crates/fleet-server/src/main.rs`, `data_dir`, the `[blobs]` and `[database]` sections and the `[audit]` keys `max_file_bytes`, `keep_files` and `retain_days` of `server.toml` and their parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `server migrate`, `server migrate-to-postgres` and `server blobs-to-s3` commands, the `object_store`, `tokio-postgres`, `deadpool-postgres` and `tokio-postgres-rustls` dependencies in `Cargo.toml` and `deny.toml`, the S3 and PostgreSQL adapters' tests and the CI jobs that run them, the rustfs and postgres services in `observability/compose.yaml` and their forward into the Dev Container, the approval of an enrolment request in `crates/fleet-server/src/fleet.rs` and `crates/fleet-server/src/api.rs` and the audit entries it writes, `docs/ARCHITECTURE.md`, and every document that names where the Server keeps its state
- **Supersedes:** [ADR-0044](0044-the-servers-state-and-its-stores.md)

## Context

The Server's state is its records — Configurations with their revisions, Deployments, Package
records, Agents with their labels and assignments, Hosts, the certificate register, the
revocation list, bootstrap entries with their PKCS#12 files, the enrolment window with its queue
— its audit record, and the bytes of its Packages' artifacts. G-19 asks that the artifacts can
live under the data directory or in an S3-compatible object store, and the records under the data
directory or in PostgreSQL, as the operator names, with an Agent receiving the same verified bytes
either way, through the Server, and the fleet never reaching a store itself. G-2 asks that the
Server knows the fleet's state across restarts.

Each store is a port with adapters ([ADR-0009](0009-ports-and-adapters.md)). A port speaks in
domain types; what must hold together is one call; integrity rules — whether a Package is
assigned, whether a Deployment pins a Package, which revisions are still referenced — are port
queries, so a database adapter can enforce them without in-memory scans. A rollout writes
revisions and assignments that must hold together.

What a port stores is secret-bearing whatever the backend
([ADR-0043](0043-the-fleet-record-of-agents-and-hosts.md)
clause 5): effective configurations, bootstrap PKCS#12 files, the certificate register. Q-1 forbids
sending a credential or a package unencrypted beyond the loopback, which holds for the Server's
traffic to its database and its object store as much as for an Agent's. Q-3 bounds what a new
dependency may bring:
[ADR-0045](0045-tls-1-3-two-planes-and-bounded-input.md)
keeps the TLS stack on rustls with *ring*, without `aws-lc-rs` and without OpenSSL, and TLS 1.3
alone.

The artifacts are the largest part of the state: one entry of 1 GiB is allowed
(`max_package_size_bytes`), and the fleet's software multiplies that. On the Server's host they
share a disk with the audit record and the register, are backed up with them, and bind the Server
to one machine. Files bind the records to one host and one disk too: a backup is a copy of a
directory taken while the Server writes into it, and nothing but the Server can read the records
consistently. The target architecture recorded outside the ADRs, the working note "server split",
puts every store in PostgreSQL and the artifacts in S3-compatible storage, content-addressed and
immutable, streamed through the Server in both directions, with the password read from a file and
the connection verified against the database's certificate and host name. Running several Server
instances against one database or one bucket has its own questions — presence, leases, notifying
the other instances — which
[ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
answers.

Existing installations must keep their trust data. Starting empty would lose the revocation list,
and a revoked certificate would be admitted again. The project reads no older store shapes inside
the Server.

A reader without this conversation would expect several things that this ADR decides against:
that S3 and PostgreSQL replace the filesystem, that the Server hashes every stored object again at
startup as it hashes every file, that the Server creates its database, that both record stores
keep the whole fleet in memory, or that neither does (clause 3 decides: the Server on files from
memory, the instances on PostgreSQL from the database), and that the stores' endpoints and
options come from the `AWS_*` and `PG*` environment variables, as their clients read them by
default.

An approval signs a certificate, which then has to be recorded in three places: the register,
the Host the approval mints, and the decision on the request, which is what hands the certificate
to its host. Written one after the other, each committed on its own, a decision that fails after
the other two leaves a valid certificate in the register that nobody holds, beside a Host with no
binding, and the request still waits. Approved again, it is signed a second time for the same key.
On PostgreSQL the commit of a decision fails on a lost connection, on the bound of a call, or when
a commit elsewhere on the database server holds the notification lock longer than the bound
clause 24 puts on each wait for a lock, the session's `lock_timeout`. The audit record cannot be
written with the decision: one writer task per instance gives every entry its place in the chain
of its lease
([ADR-0042](0042-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)) and
writes them in batches, and an entry is accepted before the act it records
([ADR-0039](0039-an-append-only-audit-record.md) clause 5).

This ADR restates [ADR-0044](0044-the-servers-state-and-its-stores.md) in full, which restated
[ADR-0029](0029-the-servers-state-and-its-stores.md). Its clauses 1, 12,
21 and 27 take up the chains per lease of
[ADR-0042](0042-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md),
its clauses 2, 3, 10, 14, 16, 22 and 23 the instances of
[ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md),
and its clause 3 the port of
[ADR-0043](0043-the-fleet-record-of-agents-and-hosts.md). Its clauses 2, 9 and 21 decide where an
approval records its certificate, and what the audit record says of one whose decision did not
commit.

## Decision

We will keep all of the Server's state under one `data_dir`, one store per domain behind a port
shaped for a database or an object store, and let the operator choose in `server.toml` whether
the records live there or in one PostgreSQL database and whether the artifact bytes, kept by their
SHA-256, live there or in one bucket of an S3-compatible object store, each reached over TLS 1.3
and configured in the Server's configuration alone, record an approved certificate in the write
that decides its request, and move an old layout by an offline command.

1. **One key chooses the record store; `data_dir` holds what is kept on the Server's disk.**
   - `[database] store` is `"fs"`, the default, or `"postgres"`. Any other value fails startup
     naming the key. Both are supported in production.
   - `data_dir` (default `fleet-data`) is where the Server keeps what it keeps on its own disk.
     With `"fs"` it holds every record, the audit record among them. With `"postgres"` and the
     filesystem blob store it holds the blobs alone, and the audit record is in the database
     (clause 12). Its layout is fixed
     ([ADR-0028](0028-package-versions-high-water-mark-and-floor.md)).
   - With `"postgres"` and `[blobs] store = "s3"` the Server keeps nothing on its own disk. It
     creates no `data_dir`, and a configuration that names one fails startup, naming the key. It
     reads its configuration, its TLS files and its secret files, all of them read-only, and writes
     to no file. The Server's image names no data directory for that reason, and a Server in it
     that keeps state on its disk is given one by its operator
     ([ADR-0040](0040-releases-attestations-and-the-server-image.md)
     clause 16).
   - `config_dir` and `packages_dir` do not exist. A `server.toml` that names either fails
     startup, naming the key and `server migrate`.
   - The Server creates every directory under `data_dir` owner-only (`0700`) and writes every
     file `0600`. These are Unix modes: the Server ships on Linux only, and built for Windows it
     sets no ACL, so `data_dir` keeps the access its parent directory hands down. Restricting it
     to the Server's account is then the operator's, as no installer exists for the Server
     there.
   - Every file is written through a temporary file and a rename. Where a record answers a
     security decision (the register, the revocation list, the audit), the rename is synced to
     disk before the decision is answered.

2. **One port per domain, shaped for a database.**
   - Every domain of the `data_dir` layout is a port in its core module, and the filesystem
     adapters live under `fs/`. A port speaks in domain types, never in paths.
   - What must hold together is one call to the port. A rollout writes the Agents' assignments
     and the revisions they pin through one operation. The filesystem adapter writes the
     revisions before the assignments, so a crash leaves at most an unreferenced revision, which
     the next load collects. A database adapter makes the same call one transaction.
   - An approval is one call to the enrolment port: the register entry of the certificate it
     signed and the Host it mints are written with its decision (clause 9).
   - Integrity questions are port queries, not scans in the caller. Examples are "is this
     Configuration, Package or Deployment pinned by an assignment?" and "which revisions are
     referenced?". The filesystem adapter may answer them from what it holds in memory.
   - A port never assumes that it is the only writer. Nothing outside the adapter caches its
     records across a request without the adapter's say.
   - A port whose record has parts writes them as parts: a Configuration's revisions are added and
     removed by hash, a Package's entries by platform, a Deployment's members by name, and a call
     that decides from what is stored — the revisions a Configuration keeps, a floor that is never
     lowered — reads it in its own transaction, locking the row. A database adapter deletes no part
     the call does not name.
   - The ports of the enrolment window and its queue, the certificate register, the revocation list
     and the Hosts speak per entry: revoke or lift one, register or remove one certificate, write
     one Host, bind one `instance_uid`, queue, decide or expire one request, open or close the
     window. The filesystem adapter may write a whole file for each, as one Server is its only
     writer; a database adapter changes the rows the call names and no other, and deletes nothing
     the call does not name.

3. **On the filesystem store the Server answers from memory; on PostgreSQL from the database.**
   - With `store = "fs"` there is one Server. It loads every record through its port at startup,
     answers from memory, and writes through the port before an act is answered, as
     [ADR-0043](0043-the-fleet-record-of-agents-and-hosts.md)
     clause 4 states.
   - With `store = "postgres"` an instance holds in memory only the Agents it carries, and the
     Operator plane reads the fleet from the database
     ([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
     clauses 8 and 13), through the port's `get` and `query`
     ([ADR-0043](0043-the-fleet-record-of-agents-and-hosts.md)
     clause 2) and its `query_page` and `reported_values`
     ([ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
     clause 9). An instance, too, writes through the port before an act is answered.

4. **The filesystem adapter keeps one JSON file per Agent, `data_dir/agents/<instance_uid>.json`;
   the PostgreSQL adapter keeps one row per Agent.**
   - The directory is created owner-only. Writes go through a temp file and an atomic rename.
     Every file is loaded at startup, and a file that does not parse fails startup naming it.
   - The envelope is this adapter's format: scalars, the labels and the effective-configuration
     text are readable JSON; the wire-typed fields are protobuf bytes in base64. That encoding's
     compatibility rules the Baseline already governs, so a field upstream adds round-trips
     unread.
   - The envelope carries a `version`. One this Server does not write fails the load rather than
     being guessed at.
   - The PostgreSQL row holds the same fields, the wire-typed ones as `bytea` (clause 21). Startup
     reads the assignments alone, and one that does not decode fails startup naming its
     `instance_uid`; any other field of a row that does not decode fails the read that meets it,
     naming its `instance_uid`.

5. **Configurations persist one record each.** With the filesystem store each is
   `data_dir/configurations/<name>.json`, written through a temp file and an atomic rename,
   loaded at startup; `DELETE` removes the file, and a file that does not parse fails startup
   naming it. With PostgreSQL each is a row, with its revisions in rows of their own, and
   `DELETE` removes them in one transaction. What the record holds besides the saved fields — the
   revisions a rollout pins — is [ADR-0026](0026-deployments-and-rollouts.md)'s.

6. **The Package store's layout is fixed, and no other is read.** With the filesystem store a
   Package is `packages/<agent_type>@<version>.json`, holding its identity and every entry's
   metadata, the hash of its blob included; the Deployments are in `deployments/`. A loose file,
   or one that does not parse, fails startup naming the path; skipping it would open a store that
   merely looks empty. With PostgreSQL a Package and its entries, and each Deployment, are rows of
   the Server's schema, and a row that does not decode fails startup naming it. The bytes are in
   the blob store with either.

7. **The store is the Server's only source of software.**
   - The Package records in the Server's record store and the chosen blob store hold every
     Package; they are managed through the REST API and restored at startup.
   - `OffersPackages` and `AcceptsPackagesStatus` are declared only while the store holds a
     Package; an undeclared capability is never exercised.
   - The directories under `data_dir` are owner-only and every file `0600` (clause 1).

8. **An operator opens an enrolment window for a bounded time, and it is closed by default.**
   - `POST /api/v1/enrolment/window` with `{"open_for_secs": n}` opens the window, or moves its
     end to `n` seconds from now. `n` runs from 1 to 86400, and anything else is answered `400`.
   - `GET /api/v1/enrolment/window` reports whether it is open and until when.
     `DELETE /api/v1/enrolment/window` closes it early.
   - The window's end and its pending requests are persisted through the enrolment port — in
     `data_dir/enrolment.json` with the filesystem store, in the database with PostgreSQL — an
     approved request with its certificate included, before the act that changes them is
     answered. A restart keeps an open window open until its end and keeps its requests. A window
     whose end has passed is closed when it is loaded.
   - When the window closes, every enrolment connection is closed and every pending request
     expires, an approved one whose certificate the host has not yet collected included. That
     host enrols again in the next window. The rejected keys outlive the window, persisted
     through the same port
     ([ADR-draft-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window](draft-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)
     clause 16).

9. **The Server keeps a register of what it signed.**
   - For each certificate the register holds:
     - issuer, serial and subject;
     - the SHA-256 fingerprint of its public key;
     - `not_after`;
     - the `instance_uid` of the message that carried the CSR;
     - its host ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md));
     - on a renewal, the certificate it renewed, its **predecessor**.
   - The predecessor is the certificate the CSR's renewal proof names
     ([ADR-0015](0015-client-modes-and-the-gateway.md)), and without a proof the certificate the
     connection presented, which is never a Gateway host's. It is never the `instance_uid` the
     message names, because that value is self-asserted and a chain must not be left on its
     say-so.
   - A proof is checked against the client CA and its key, so it holds through a Gateway too.
     Behind a Gateway a certificate is renewed only with a proof, in its own chain and host.
   - An enrolment has no predecessor and is the root of its chain.
   - An issuer is identified by the SHA-256 of its DER-encoded name, so no text form of a name
     can fail to match.
   - The register lives in `data_dir/certificates/`, one file per certificate, with the
     filesystem store, and in one row per certificate with PostgreSQL. Each entry is written
     before the certificate is offered.
   - An approval's entry, and the Host it mints, are written with the decision on its request, so
     a decision that is not written leaves neither:
     - With PostgreSQL they are written in the transaction that decides the request, which holds
       the request's row from before the signature until it commits.
     - With the filesystem store the decision is written first, the entry and the Host after it,
       and the certificate is offered once both are written. An entry that cannot be written takes
       the decision back. A request found decided at startup whose certificate the register does
       not hold waits again.
     - The Server's memory takes the entry and the Host up once the decision is written.
     - A request whose decision was not written still waits, and approved again it is signed anew.
       The certificate signed before is recorded nowhere but in the audit record, and was never
       offered.
   - On PostgreSQL, when the transaction of a decision fails, the instance tells a refusal from
     an outcome it does not know:
     - An answer of the database server to the UPDATE that writes the decision, or an ERROR it
       answers to the COMMIT, is a refusal: the transaction was rolled back, and the signature is
       rolled back.
     - Any other failure leaves the outcome unknown: a lost connection, the bound of a call, a
       FATAL or a PANIC at the COMMIT, which may end a backend whose commit is already durable,
       and an error of any other statement of the transaction. The instance then reads the
       request back under a share lock on its row, which waits until the transaction that
       decided it has ended. Decided with the certificate this instance signed, the commit landed
       and the approval stands. Waiting, or decided with another certificate, the signature is
       rolled back. A read back that fails, or waits out its bound, leaves the outcome unknown.
     - Unless the approval stands, the request waits and the approval answers that it could not
       be written.
   - The audit record names each signature and what came of it
     ([ADR-0039](0039-an-append-only-audit-record.md)):
     - `issuance.signed` is recorded before the decision is written, and for an enrolment it
       names the request under `request`;
     - `enrolment.approved` is recorded once the decision is written, before the approval is
       answered, and names the certificate under `serial`;
     - `issuance.undecided`, with `serial`, `request` and the outcome `rolled_back`, records a
       signature whose decision was not written, and with the outcome `unknown` one whose request
       could not be read back, for which the register answers.
     - A certificate of an enrolment counts as issued where `enrolment.approved` names its serial,
       and as not issued where `issuance.undecided` names it with the outcome `rolled_back`. For
       a signature neither names — after a crash before or after the decision, or an
       `enrolment.approved` the record could not take — and for the outcome `unknown`, the
       register answers while the certificate is valid: it was issued if
       `GET /api/v1/certificates` lists it.
       Nothing in the chain is removed or rewritten for it.
   - It holds at most 100 000 certificates, and at most 10 000 in one chain — below one root, the
     first certificate the chain renews from. It refuses a renewal once fewer than 1 000 places
     are left, so renewals alone cannot shut out an enrolment. A CSR beyond any of these is
     refused.
   - An entry stays while the certificate, or one renewed from it, is still valid.
     `GET /api/v1/certificates` lists it.

10. **The revocation list is persisted and bounded.** It lives in `data_dir/revocations.json`
    with the filesystem store and in the database with PostgreSQL, is written before the request
    is answered — atomically, or in one committed transaction — and survives a restart. It holds
    at most 100 000 entries, and a revocation beyond that is answered `507`. With PostgreSQL the
    list carries one version, a counter that the transaction of every revocation and every lift
    increments, and so does that of every change to the register: an entry written for a renewal
    or an approval, and one the prune removes. What
    [ADR-0038](0038-admission-by-client-certificate-and-revocation.md)
    clause 28 revokes depends on the register as much as on the list, so an instance that missed
    the notification of an entry finds the version moved, and never takes its own next write of
    the list as naming a register it does not hold. Every instance reads the same version: it is
    the `ETag` of the Gateways' list (that draft's clause 34) and what an instance compares at
    each renewal of its lease
    ([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
    clause 10). A certificate entry is dropped with its register entry once neither the
    certificate nor any renewal of it is valid. With PostgreSQL a register entry is removed only
    while the database holds no valid certificate renewed from it, whatever the register of the
    instance that prunes holds. A Server of 0.8.0 beside this one does neither: it moves the
    version with the list alone, and its prune removes an entry by what its own register holds,
    whatever renewals the database holds below it. An entry such a Server writes or removes is
    taken up from its notification only. An entry for a certificate the register never held
    stays until an operator removes it.

11. **The Server records the bootstrap certificates it issued, keyed by issuer and serial, and
    keeps a PKCS#12 file until its certificate ends, is revoked or its entry is deleted.**
    - Before it answers, the Server writes the bootstrap certificate's record: the serial, the
      subject, the SHA-256 fingerprint of the public key, the issuing time, `not_after`, and
      whether a PKCS#12 file was made. With the filesystem store the record is
      `data_dir/bootstrap/<issuer>-<serial>.json` and the PKCS#12 file, when one was made, is
      written beside it as `<issuer>-<serial>.p12`; both are owner-only and written atomically.
      With PostgreSQL the record and the file are one row, written in one transaction.
    - `GET /api/v1/bootstrap-certificates` lists every entry with whether it is revoked and
      whether its PKCS#12 file can be downloaded (`p12`).
    - `GET /api/v1/bootstrap-certificates/{serial}/p12` answers `200` with the file as
      `application/x-pkcs12`, `Content-Disposition: attachment; filename="bootstrap.p12"` and
      `Cache-Control: no-store`, as often as it is asked. It answers `404` when the certificate
      has no such file, is revoked, has ended, or its entry was deleted. The `{serial}` names the
      entry of the bootstrap CA in force.
    - An entry and its PKCS#12 file are removed once the certificate has ended, or when an
      operator deletes the entry ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)). A
      revocation with the authority `bootstrap` removes the file before it is answered, and
      lifting the revocation
      ([ADR-0038](0038-admission-by-client-certificate-and-revocation.md))
      does not bring it back.
    - At most 10 000 valid entries are kept, and a request beyond that is answered `409`.
    - A bootstrap certificate is revoked as
      [ADR-0038](0038-admission-by-client-certificate-and-revocation.md)
      states for revocation, with the authority `bootstrap`.
    - The audit records each issuance as `enrolment.bootstrap_issued`, with serial, subject,
      `not_after` and `p12`. It records each download of a PKCS#12 file as
      `enrolment.bootstrap_p12_downloaded` with the serial. It records each deletion as
      `enrolment.bootstrap_deleted` with the serial and subject
      ([ADR-0039](0039-an-append-only-audit-record.md)).
    - When the audit cannot be written, nothing is handed out or deleted, and the request is
      answered `503`.

12. **How long the record goes back.**
    - The audit record is one hash chain on the filesystem store and one hash chain per lease of
      a Server instance on the PostgreSQL store
      ([ADR-0042](0042-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)),
      kept where the Server keeps its records: in files under `data_dir/audit/` with
      `store = "fs"`, in the database with `store = "postgres"`. Each instance writes its chain by
      one writer thread of its own, in the order the decisions were taken.
    - On the filesystem store the audit is written to `data_dir/audit/audit-<first seq>.jsonl`. The
      directory is `0700` and each file `0600` on Unix, under the data root's restricted ACL on
      Windows. Each entry is one line, written with a single append and synced to disk; a write that
      fails partway is cut back to the last whole line. A file reaching `[audit] max_file_bytes`
      (default 64 MiB) is closed and a new one opened. The Server keeps `[audit] keep_files` files
      (default 16) and deletes the oldest. Before it does, it records `audit.rotated` with the
      deleted file's last hash.
    - On the PostgreSQL store entries stored more than `[audit] retain_days` days ago (default 365,
      at least 1, at most 36 500) are deleted, at each instance's start and once a day after. An
      entry's age is the database's time when it was stored. From each chain the longest run of
      entries from its start that are all that old is removed, so no younger entry goes. Each
      instance prunes its own chain. Closed chains, those closed at a shutdown and those closed for
      a lapsed lease, are pruned by the same `retain_days` by one instance at a time, the one that
      holds a session-level advisory lock for it; an instance that finds the lock held leaves them
      to the next day. A chain that is open and whose lease no instance holds is pruned once it is
      closed. Before it deletes, an instance records in its own chain one `audit.pruned` for each
      chain it prunes, naming it as `pruned_chain` with the last removed `seq` as `last_seq` and
      that entry's hash as `last_hash`, and deletes only once they are written. "Days" are periods
      of 24 hours, whatever the database's time zone.
    - A prune runs in rounds. A round removes at most 100 000 entries across the chains it
      prunes, taking each chain's run from its start, and records its own `audit.pruned` entries
      before it deletes. While a round removes as many entries as a round may, the next round
      follows right away, so a backlog drains over as many rounds as it needs; the first round
      that removes fewer ends the prune. A round deletes from every chain in one transaction, so
      one that stops part-way deletes nothing and every round leaves a record that verifies, and
      within it in batches of a bounded number of entries, so no statement outgrows the statement
      timeout every connection of the Server holds. A round that does not end within five
      minutes, its wait for its `audit.pruned` entries included, is cut off: its transaction
      deletes all it was to delete or nothing, and the `audit.pruned` entries it wrote before
      that transaction stay either way. The prune then stops, and the next start or day goes on
      from where the record was left. A round that prunes no chain ends the prune too: there is
      nothing a next round would do differently.
    - An `audit.pruned` is an entry like any other and is pruned in its turn. Where an entry an
      instance removes is an `audit.pruned` whose link is still needed, the instance records that
      link again in a new `audit.pruned`. A link is still needed when the chain it names begins
      right after the entry it records as last removed, or is closed with no entries left on that
      entry's hash, and the same round does not prune that chain further; a chain the same round
      prunes further is linked by the `audit.pruned` recorded for it in that round. Where an entry
      it removes names the archive as its `predecessor`, the `audit.chain_opened` of the first
      chain or an `audit.pruned` that carried it, a new `audit.pruned` carries the predecessor on
      as `predecessor_chain`, `predecessor_seq` and `predecessor_hash`. A closed chain with no
      entries left keeps its record. The filesystem store holds one chain, which is never closed,
      and its rotation bounds it.
    - `max_file_bytes` and `keep_files` apply to the filesystem store alone and `retain_days` to the
      PostgreSQL store alone. A key that does not apply to the configured store fails startup,
      naming it.

13. **Artifact bytes are kept by their hash, behind a blob port.**
    - The port is `put(stream) → (sha256, size)`, `get(sha256) → stream`, `exists(sha256)`,
      `delete(sha256)` and `list() → [(sha256, size)]`. It is synchronous; an adapter that speaks
      an asynchronous protocol bridges inside itself, and no caller changes.
    - A blob is named by the lowercase hex SHA-256 of its bytes and by nothing else. Bytes already
      stored are stored once.
    - A blob that no Package entry references is deleted when its last entry goes, and at
      startup. Only the startup sweep lists every blob; a sweep after an act deletes the blobs the
      act let go. On PostgreSQL whether an entry references a blob is read from the stored
      entries, under the lock of clause 14, and not from what the instance holds, so a blob that
      an entry written through another instance names is kept.
    - The size limits of a Package entry
      ([ADR-0027](0027-packages-signatures-and-delivery.md)) count the blobs.
    - A Package's metadata names a blob by its hash only.

14. **One blob store, chosen in `server.toml`.** `[blobs] store` is `"fs"`, the default, or
    `"s3"`. Any other value fails startup naming the key. Both are supported in production. The
    filesystem blob store is read and written by one Server instance alone. With `"s3"` every
    instance on one database uses the same bucket and prefix. On PostgreSQL every sweep of
    unreferenced blobs (clause 13) and every cleanup of `<prefix>staging/` (clause 16) run under an
    advisory lock, so one instance does them at a time
    ([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
    clause 2). The lock is taken whatever the blob store: with `"fs"` the instance is alone and
    the lock decides nothing, but the PostgreSQL record store does not ask which blob store it
    serves, and one rule for every PostgreSQL Server costs one session per sweep.
    - A sweep holds the lock alone. An upload takes it shared inside the transaction that writes
      its entry, checks there that its blob is stored, and commits, the lock let go with the
      transaction. A sweep therefore never runs between an upload's check and its commit, and
      never deletes a blob that an entry is about to name. An upload whose blob a sweep deleted
      first is refused, to be uploaded again, and one whose transaction does not commit, its
      session ended included, writes no entry.
    - A sweep marks what it deletes before it deletes it, and deletes nothing else. On the session
      that holds the lock, in one statement, it marks each blob it would delete that no stored entry
      names, and deletes only the blobs that statement marked; its session ended, the statement
      fails and nothing is deleted. A mark names the lease of the sweep's instance: at startup the
      lease the instance is about to take, which is not live yet, and none for a command. Before
      each deletion the sweep asks whether its lock still holds, and on the same session renews the
      time of that blob's mark and finds it still there. Then, before it calls the blob store, it
      finds that no more than 15 seconds have passed since it sent that renewal, on the clock that
      counts a suspend which the lease of
      [ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side](draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md)
      clause 3 is measured on. Once any of these fails it deletes nothing more and says so in the
      log; a blob whose mark another sweep took away it passes over. An upload
      whose blob is marked is refused, to be uploaded again, in the transaction that checks that the
      blob is stored.
    - A sweep takes its marks away once its deletions are done, whether its lock still holds or not,
      and also when its marking failed, since the statement may have landed without its answer.
      Marks it could not take away its instance tries again every `[database] lease_renew_secs`.
      Every sweep, holding the lock, takes away the marks of a lease that is not live, or of none,
      once they were last renewed more than 130 seconds before, as the database's clock reads both.
      A renewal that answered was sent at most 15 seconds before its answer, the bound of every call
      of clause 24, and is followed by one call to the blob store, which takes at most about 100
      seconds on the S3 store of clause 17 (a connection of 10 seconds, retries started within 60,
      and 30 seconds of silence); 15 seconds more leave room for the waits between retries and for
      the clocks of two primaries after a failover. So a sweep that renewed a mark is no longer
      deleting that blob once its mark may be taken away, and no entry is written that names a blob
      a sweep may still be deleting once its session ended and its lock is gone. A mark not yet
      renewed for its deletion may be taken away meanwhile, and its sweep then passes over its blob.
    - Three cases are outside this. A process suspended for longer than 130 seconds while its call
      to the blob store is in flight may delete a blob whose mark another sweep has taken away
      meanwhile: a request sent before the suspend is sent on, or tried again, once the host runs,
      and nothing in the process can call it back. A failover to a replica
      the marks had not reached, under asynchronous replication, leaves the new primary without them
      while the old sweep may still be deleting, so the exclusion holds across a failover only with
      the synchronous replication that clause 17 of
      [ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side](draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md)
      requires. And an instance of 0.8.0
      beside the instances of this release neither marks nor reads a mark, so until the last one is
      replaced a sweep through it may still delete a blob that an entry written meanwhile names. In
      each case the next start fails on that entry, naming it, and its artifact is uploaded again.
    - A sweep's lock is a session-level advisory lock, held on a connection of its own, outside
      the pool: one session more for each sweep in progress, which the database's
      `max_connections` must allow for. It waits for the lock 60 seconds, its session's
      `statement_timeout` raised to let it; an upload waits as long as the database's
      `lock_timeout` of 5 seconds. A sweep's session that ends while it holds the lock is logged,
      and the sweep deletes nothing more; a blob it marked stays refused to uploads until the
      mark is cleared. A session that ends before it had the lock is not logged.
    - A startup sweep that cannot have the lock fails startup, naming the lock. A sweep after an
      act logs it, and the blobs it would have deleted stay until the next startup. A sweep after
      an act runs once the act has let go of every lock of its Server's own, such as the fleet's,
      so no other act waits behind its wait for the lock.

15. **The filesystem blob store** keeps one file per blob in `data_dir/blobs/`. It streams an
    upload to a staging file inside `blobs/`, hashes it on the way, and renames it to its hash. At
    startup every blob is hashed again, and one whose content does not match its name fails
    startup, naming it.

16. **The S3 store keeps one object per blob under a fixed layout.**
    - `[blobs.s3]` names `endpoint`, `bucket`, `region` and an optional `prefix`. Objects live at
      `<prefix>blobs/<sha256>`; an upload in progress lives at `<prefix>staging/<lease>/<random>`.
      The
      Server touches no key outside `<prefix>`. A key under `<prefix>blobs/` that is not a hash,
      or any other key under `<prefix>`, fails startup naming the key.
    - `<lease>` is the identifier of the lease of the instance that writes the upload
      ([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
      clause 3), or `fs` with the filesystem record store, which has one Server and no lease. A
      staging object is left behind only by an upload that was cut off, or one whose staging
      object could not be deleted. The cleanup runs with every sweep of clause 13 — at startup, and
      after an entry is replaced or deleted, a Package deleted, or an upload fails — and whenever
      an instance takes over a lapsed lease. It deletes the staging keys of every lease that is not
      live, and never those of a live one. With the filesystem record store it deletes everything
      under `<prefix>staging/`, and at startup alone, as every upload in progress is its one
      Server's. A starting instance therefore never deletes an upload another
      instance is still writing, and a staging object that a lease released at an orderly
      shutdown left, or that a takeover's cleanup could not delete, goes with the next sweep on
      any instance. There is no periodic cleanup.
    - A multipart upload that was cut off before it completed is not an object, and the cleanup
      does not see it. Its parts are the operator's to expire, with a lifecycle rule on the bucket
      that aborts incomplete multipart uploads.
    - The Server does not create the bucket. A bucket that does not exist or cannot be listed
      fails startup, naming the bucket and the error.
    - `put` streams the upload into a multipart upload at a staging key and hashes it on the way.
      Once the hash is known, the Server copies the staging object to its hash key unless that key
      already exists, then deletes the staging object. A stream that fails leaves no object under
      `blobs/`.
    - At startup the Server lists `<prefix>blobs/` for each blob's hash and size and downloads
      nothing. The integrity of what it serves rests on the hash it computed at upload, on the
      signature over every artifact, and on the hash the Client checks before it installs
      ([ADR-0027](0027-packages-signatures-and-delivery.md)).
    - `get` streams the object through the Server to the Agent. The fleet never reaches the
      store, and the Server hands out no presigned URL, neither for downloads nor for uploads.

17. **The S3 connection is TLS 1.3 and is configured in the Server's configuration alone.**
    - `endpoint` is `https://`. An `http://` endpoint is accepted only on the loopback, and any
      other fails startup naming the key (Q-1).
    - The HTTP client is built with the TLS 1.3, *ring*-only configuration the rest of the project
      uses, and `object_store` is built without its `aws-lc-rs` feature. `[blobs.s3] ca_file`
      adds one PEM root for a store whose certificate no public root signs.
    - Endpoint, region, bucket, prefix and every TLS or HTTP option come from the Server's own
      configuration ([ADR-0030](0030-the-servers-configuration.md)) and from nothing else. The
      Server reads no `AWS_*` variable that would change them, such as `AWS_ENDPOINT` or
      `AWS_ALLOW_HTTP`.
    - Every request is bounded: its connection, and the silence before its answer begins and
      between two pieces of it, with a bounded number of retries in a bounded time. A store that
      stops answering fails the call that needs it, and no request to the store runs while the
      Server holds the lock its offers and listings wait for.
    - A `prefix` holding a character the S3 client would percent-encode fails startup naming the
      key, since the keys the Server writes would not be those it lists.

18. **S3 credentials are never written into `server.toml`.**
    - With `access_key_id` and `secret_access_key_file` set, the Server reads the secret from
      that file and signs with that key pair. An `access_key_id` without the file, or the
      reverse, fails startup naming both keys.
    - The file may be read by its owner and its group, as an orchestrator mounts a secret for a
      group (`0440` with Kubernetes' `fsGroup`), and by nobody else, and written by its owner
      alone: a file others may read, or its group may write, fails startup naming the key.
    - Without them, the Server uses the workload identity of its platform, in this order: a web
      identity token (`AWS_WEB_IDENTITY_TOKEN_FILE` with `AWS_ROLE_ARN`), the container
      credentials of ECS or EKS (`AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`, or
      `AWS_CONTAINER_CREDENTIALS_FULL_URI` with the EKS Pod Identity token
      `AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE`), and the instance metadata service. These are the
      only `AWS_*` variables it reads.

19. **Switching from the filesystem to S3 copies what is missing.** With the filesystem record
    store, when `store = "s3"` and `data_dir/blobs/` exists, the Server hashes, at startup, every
    file there that a Package entry references and that the bucket does not hold, uploads it as
    `put` does, and logs each one. A file whose content does not match its name fails startup,
    naming it. The Server does not delete `data_dir/blobs/`, does not write to it again, and with
    `store = "s3"` does not create it; removing it is the operator's. With PostgreSQL and S3 the
    Server has no `data_dir` (clause 1), and two commands make the copy instead, never a start:
    - `server migrate-to-postgres` from the old `data_dir`, as the records move (clause 27).
    - `server blobs-to-s3 --data-dir <data_dir>`, for a Server already on PostgreSQL with the
      filesystem blob store. It runs once, with the `server.toml` the Server will start with,
      which names the database and the bucket and no `data_dir` (clause 1), and refuses to run
      while any instance holds a live lease. It holds the lock of clause 14 alone for its whole
      run, so an instance that starts meanwhile waits at its startup sweep and fails naming the
      lock, rather than deleting the command's staging objects. It uploads, as above, every blob of
      `<data_dir>/blobs/` that a Package entry references and the bucket lacks, then checks that
      the bucket holds the blob of every entry, and fails naming an entry whose blob it does not
      hold. It only reads `<data_dir>`; removing the directory afterwards is the operator's.

    The reverse switch copies nothing: a Package entry whose blob the chosen store does not hold
    fails startup, naming the entry.

20. **The object store is external; the adapter only connects to it.** The Server ships with no
    object store and creates nothing in one but the objects of clause 16. In production the
    bucket, its credentials, its policies, its retention and its backups are the operator's.
    - For development, rustfs runs as a service of `observability/compose.yaml`, the Compose
      project on the host beside the Dev Container
      ([ADR-0004](0004-dev-container-runtime.md)). The Dev Container forwards its port onto its
      own loopback, as it forwards the Collector's, so a development Server reaches it at an
      `http://127.0.0.1` endpoint that clause 17 allows, through `[blobs.s3]` like any other
      store.
    - The S3 adapter's tests run against the store that `FLEET_TEST_S3_ENDPOINT`,
      `FLEET_TEST_S3_BUCKET` and the credentials of clause 18 name. CI runs them in a job with a
      rustfs service, its image pinned. Where no store is named, the tests are skipped and say
      so; they never start a store of their own.
    - The project supports S3-compatible stores in general; rustfs is the one development and CI
      use.

21. **One PostgreSQL adapter per port, one transaction per call.**
    - Every port of clause 2 but the blob port has an adapter that keeps its records in tables
      of the Server's schema: Configurations with their revisions, Deployments, Package records,
      Agents with their labels and assignments, Hosts, the certificate register, the revocation
      list, bootstrap entries with their PKCS#12 files, the enrolment window with its queue, and
      the audit record, one chain per lease of a Server instance (clause 12).
    - What one port call writes is one transaction. A rollout's revisions and assignments commit
      together or not at all, a bootstrap entry commits with its PKCS#12 file, and an approval's
      register entry and Host commit with its decision (clause 9).
    - Wire-typed fields of an Agent are kept as protobuf bytes, as the filesystem envelope keeps
      them (clause 4), so a field upstream adds round-trips unread. Scalars are columns.
    - Text an Agent or a CSR chose — an Agent's effective configuration, a certificate's subject
      — is kept as `bytea`, and every JSON document as `json`: PostgreSQL's `text` and `jsonb`
      refuse a NUL character, which a file keeps, and one Agent's report must never fail the
      writes of others or a rollout. The same bytes round-trip as with the filesystem store.
    - A call that answers a security decision returns only after its transaction has committed.
    - The creation of a new Agent's record commits without waiting for the database to flush it
      to disk, so creations, which count one after another on one row, are not each held for a
      flush. A crash of the database may lose the records created last, with their count in the
      same transaction, and each such Agent's next report creates its record again. Every
      transaction committed after one waits for its flush too, so no take, binding, report or
      audit entry committed later outlives a record it follows.
    - A call of the enrolment, register, revocation or Host port changes the rows it names and no
      other (clause 2), and counts the bound it could pass in the same transaction.
    - A Configuration's revisions, a Package's entries and a Deployment's members are rows of their
      own, and a package floor one row per Agent type, written under `SELECT … FOR UPDATE` after
      the check that it is not lowered
      ([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
      clause 19).

22. **The Server applies its own schema.**
    - The schema is embedded in the binary as numbered migrations. At startup the Server applies,
      in one transaction, every migration the database has not yet seen, and records each in a
      table of its own.
    - A migration only adds: tables, columns with defaults, indexes. A column or table is removed
      only by a release after the one that stopped using it.
    - Each migration names the oldest Server version that understands the schema it leaves, and
      the schema table records it. A database whose schema names a version newer than a Server
      fails that Server's startup, naming both versions; the Server changes nothing in it.
    - Migrations 3 to 10, the schema of the release in which several instances share a database,
      name that release, 0.8.0. A Server of 0.7.1 holds no lease, keeps its audit record in files
      and leaves the revocation list's version where it was, so it refuses the schema once a
      Server of 0.8.0 has applied them, rather than serve beside its instances
      ([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
      clause 16). A rollback to 0.7.1 after the upgrade restores the database from its backup.
    - Migration 13 adds the table of the sweeps' marks (clause 14) and names 0.8.0, which reads
      and writes none of them.
    - Migration 14 adds the count of the Agents' records: one row, which a trigger on the Agents'
      table raises and lowers as rows are added and removed, so the Agent-record ceiling is
      decided without reading every row. A new Agent's record is created in one statement that
      locks that row and inserts only while the count is below the ceiling, under no other lock.
      The migration names 0.8.0, whose additions and removals the trigger counts as well. A Server
      of 0.8.0 counts the rows under a lock of its own, which this release does not take, so while
      one runs beside the instances of this release the ceiling may be passed by the records they
      create at the same moment. A creation of this release locks the count's row before it
      inserts, one of 0.8.0 inserts before its addition reaches that row: when both create the
      record of the same new Agent at the same moment, each waits for the other, and the database
      ends one of them as a deadlock after its `deadlock_timeout`. The report it ended is answered
      `Unavailable`, and the Agent reports again.
    - The first columns to be removed are the Deployments' `packages` and `configurations`,
      which mirror their member tables, and the Hosts' `instance_uids` arrays, which the
      bindings' table replaced. No release reads the mirror columns. The release after 0.8.0
      writes them no more, and the release after it removes them. A Server of 0.8.0 still reads
      an array for an `instance_uid` that has no binding, so the release after 0.8.0 adds nothing
      to the arrays and no longer reads them. It still takes an `instance_uid` out of them when
      its binding is released, so that a Server of 0.8.0 beside it never sees a released
      binding as held. The release after it stops that, and the arrays go with the release after
      that one.
    - A Server checks its schema only at its own start, and a Server of 0.7.1 holds the exclusive
      lock of clause 23, under the same key, for its whole run. So an instance that shares the
      database tries that lock, shared and without waiting, in the transaction that migrates the
      schema, and holds it until that transaction ends. While another Server holds the lock, the
      instance applies no migration and fails startup, saying that another Server holds the
      database alone and is to be stopped first. An instance that must be alone holds the lock
      already, and does not try it again.
    - The migrations run under an advisory lock. An instance that finds it held waits for it, then
      reads the schema table again and applies only what is still missing, so instances of a new
      version started together apply each migration once.
    - The Server creates tables, indexes, its schema table, and the trigger that keeps the count
      of the Agents' records with its function, and nothing else. The database, the role, its
      rights and its extensions are the operator's.

23. **Every Server on a database schema is an instance with a lease.** The Server works in the
    schema that is the role's default; Servers in different schemas of one database share
    nothing, as the tables are the schema's. In its schema each Server takes an instance id and a
    lease before it opens either listener and renews the lease while it runs. An instance that
    cannot renew before its lease lapses ends its sessions, closes its listeners and takes a new
    lease once the database answers again
    ([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
    clauses 1 to 3). An instance with the filesystem blob store (clause 14) or a CA key in a file
    must be the only instance in the schema: it takes an exclusive lock, fails startup beside
    another live lease, makes a second instance fail startup, and refuses every call once the
    lock's connection ends (the instance draft, clause 2).

24. **The database connection is TLS 1.3, verified, and configured in `server.toml` alone.**
    - `[database.postgres]` names `host`, `port` (default 5432), `dbname`, `user` and
      `password_file`, and optionally `ca_file`. A missing key fails startup naming it.
    - The connection requires TLS. The client is built with the TLS 1.3, *ring*-only
      configuration the rest of the project uses, verifies the database's certificate against
      the public roots or the one PEM root `ca_file` names, and verifies that it names `host` —
      what libpq calls `verify-full`. `tokio-postgres-rustls` is built with its `ring` feature
      and without `aws-lc-rs`.
    - Without TLS the Server connects only to a loopback IP literal, and only when
      `[database.postgres] tls = false` says so. `tls = false` with any other `host` fails
      startup naming both keys (Q-1).
    - The password is read from `password_file`, which may be read by its owner and its group, as
      an orchestrator mounts a secret for a group, and by nobody else, and written by its owner
      alone; one that others may read, or its group may write, fails startup naming it. No
      password is written into `server.toml`, and the Server reads no `PG*` environment variable.
    - Connections come from a `deadpool-postgres` pool of `[database] pool_size` connections, 16
      by default and from 2 to 256, which the database's `max_connections` must allow for each
      instance; at most half of them create Agents' records at once, so the others serve every
      other call while creations wait for the count's row. A creation waits for a pooled
      connection up to the bound of its whole call, so a new Agent is not answered `Unavailable`
      while the calls of a connecting fleet hold every connection for a few seconds; every other
      call waits five seconds at most and fails as a failed write fails. A database that cannot be
      reached at startup fails startup, naming host and database; one lost while the Server runs
      fails the calls that need it, which answer as a failed write answers.
    - Every call is bounded, since its caller holds the Server's locks while a record is written:
      the wait for a pooled connection, the whole call on the client's side, and on the
      database's side each statement, each wait for a lock and a transaction left idle; TCP
      keepalives notice a host that is gone. A database that takes a connection and never answers
      fails the call within its bound.

25. **The database is external; the adapter only connects to it.** In production the database,
    its role, its replication and its backups are the operator's.
    - For development, PostgreSQL runs as a service of `observability/compose.yaml`, the Compose
      project on the host beside the Dev Container
      ([ADR-0004](0004-dev-container-runtime.md)). The Dev Container forwards its port onto its
      own loopback, so a development Server reaches it at `127.0.0.1:5432`, with `tls = false` as
      clause 24 allows on the loopback.
    - The PostgreSQL adapter's tests run against the database that `FLEET_TEST_POSTGRES_HOST`,
      `FLEET_TEST_POSTGRES_PORT`, `FLEET_TEST_POSTGRES_DBNAME`, `FLEET_TEST_POSTGRES_USER` and
      `FLEET_TEST_POSTGRES_PASSWORD_FILE` name, over TLS against the root
      `FLEET_TEST_POSTGRES_CA_FILE` names when it is set. CI runs them in a job against a
      PostgreSQL it starts with TLS 1.3 and a certificate of a CA made for the run, its image
      pinned. Where no database is named, the tests are skipped and say so; they never start a
      database of their own. Each test works in a schema of its own and drops it afterwards.

26. **An old layout is moved by `server migrate`, offline, and the Server never reads it.**
    - `server migrate --config-dir <old> --packages-dir <old> --data-dir <new>` runs while the
      Server is stopped. It refuses a `data_dir` that is not empty, and it copies rather than
      moves, so the old directories stay as the backup.
    - What it does with each kind of data:
      - **Trust data:** the certificate register, the revocation list, the host register,
        bootstrap entries with their PKCS#12 files, and the audit files are carried over whole.
        The audit chain is kept: its lines are copied, not rewritten.
      - **Packages:** artifacts go to `blobs/`. Each entry takes the signature a Deployment held
        for it. The statement names no Deployment, so these are the same bytes wherever the entry
        was held; when two Deployments held different signatures, the migration names the entry
        and takes none. A referenced entry is dropped with a line naming it.
      - **Agents:** records are carried over, with each Agent's labels moved into its record.
      - **Configurations and Deployments:** they keep their content. A Configuration loses its
        Selector, and the migration lists every Configuration with the Selector it had, for the
        operator to place in a Deployment ([ADR-0026](0026-deployments-and-rollouts.md)). A
        Configuration without an Agent type is not carried, because the Server would refuse it at
        startup; the migration names it, and its file stays in the old directory for the operator
        to save again with a type.
    - The migration prints what it carried, dropped and listed, and exits non-zero when it could
      not carry trust data.
    - A Server started on an empty `data_dir` while `server.toml` still names `config_dir` or
      `packages_dir` fails startup, naming `server migrate` (clause 1).

27. **An existing `data_dir` is copied into PostgreSQL offline.**
    `server migrate-to-postgres --from <data_dir> [--data-dir <new>]` runs while the Server is
    stopped, with the `server.toml` that names the database.
    - It refuses a database that holds any of the Server's tables, and a `--data-dir` that is not
      empty.
    - It applies the schema and copies every record of the old `data_dir` into the database in one
      transaction.
    - It reads the old `data_dir` in place and changes nothing there: it creates no directory,
      changes no mode and removes no leftover temporary file, which a Server's start would repair.
      It copies no record elsewhere to read it, so no private key is written a second time.
    - With the filesystem blob store it copies `blobs/` into the new `data_dir`.
    - With S3 it takes no `--data-dir`, as a Server on PostgreSQL and S3 has none (clause 1), and
      one named fails the copy, naming the flag. Before the transaction it uploads into the bucket
      every blob of the old `data_dir/blobs/` that a Package entry references and the bucket does
      not hold, as clause 19 copies them: hashed, a file whose content does not match its name
      failing the copy, naming it. A Package entry whose blob neither holds fails the copy, naming
      the entry. What it uploaded stays in the bucket when the transaction fails, and a second run
      finds it there.
    - It copies no audit entry. The old `data_dir/audit/` stays where it is as the closed archive,
      and the migration records the `seq` and the hash of its last entry in the database, which
      the first chain names as its predecessor
      ([ADR-0042](0042-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)
      clause 6). No Server writes to the archive again; its instances open their chains in the
      database (clause 12).
    - It copies rather than moves: the old `data_dir` stays as the backup. It prints what it
      copied and exits non-zero when it could not copy all of it, leaving the database without
      the Server's tables.
    - Moving records back from PostgreSQL to files is not provided.

**Out of scope:** presence, leases and notifying other instances
([ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md));
copying the archived filesystem audit record into the database, which clause 27 links instead;
paging and filtering in the database, which
[ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
decides; moving records from PostgreSQL back to files or blobs from S3 back to the filesystem;
Object Lock and bucket versioning, which the operator may enable but the Server neither needs nor
checks; server-side encryption beyond what the bucket's own default applies; keeping the
effective configuration other than as the display text it is; migrating between two `data_dir`
versions, which the first such change decides; anything that runs, provisions, replicates or
monitors a production store or database.

## Alternatives considered

- **Two roots, `config_dir` and `packages_dir`, with the new stores beside them.** The smallest
  change. It keeps Configurations in a top level shared with every other store, Deployments under
  the package root, a label directory that is not owner-only, and two keys for one state.
- **Two roots: one for state, one for bulk artifacts.** It puts the large bytes on their own
  volume, but it is a second key before anyone needs it. A `blob_dir` can be added when one does;
  the S3 store meets the need.
- **Go straight to PostgreSQL and S3.** It reaches several instances in one step. But it binds the
  model of the records to high-availability questions that are not decided, and makes every test
  depend on a database.
- **Start empty after an update of the layout.** It is the simplest. It loses the revocation list,
  so revoked certificates are admitted again (Q-1).
- **Read the old layout in the Server.** No step for the operator. But readers for older shapes
  are untestable over time and untrue as documentation, and a one-shot tool can be tested against
  fixtures and then left alone.
- **Keep the enrolment queue in memory.** It is simpler. It loses approved certificates on a
  restart and cannot work across two instances.
- **An embedded database as the default record store** (SQLite; Bindplane's bbolt-then-Postgres
  path), or **SQLite as the second store.** Embedded and simple, but a second storage idiom for
  small records loaded whole, and it binds the records to one host as files do, no step towards
  several instances. Rejected.
- **S3 or PostgreSQL replaces the filesystem, which stays as a test double only.** This is where
  the working note ends up once several instances run. One Server needs neither, and every small
  installation would need a database and an object store before it gains anything. Rejected.
- **`aws-sdk-s3`.** The official SDK reads every credential source AWS defines, the shared config
  files included. It brings the largest dependency tree of the three options and its own HTTP and
  TLS stack, which is more for `cargo-deny` and Q-3 to check. Rejected.
- **`rust-s3`.** Small, but it lacks web identity and container credentials, so it misses the
  workload identity this decision asks for. Rejected.
- **`object_store` with its default `aws` feature.** It pulls `aws-lc-rs`, a C build with cmake,
  against
  [ADR-0045](0045-tls-1-3-two-planes-and-bounded-input.md).
  The `aws-base`, `ring` and `reqwest` features build the same client on *ring*.
- **Hash every object again at startup.** That downloads the whole store at every start, which
  for a fleet's software costs minutes and egress. The signature and the Client's own hash check
  already refuse altered bytes. Rejected.
- **Hash every download on its way to the Agent.** It detects an altered object on the Server, at
  the cost of CPU per download, and only after the bytes have been sent. The Client refuses them
  anyway. Rejected.
- **Presigned URLs for downloads or uploads.** They move the traffic off the Server, but the fleet
  would then reach the store directly, outside the admission by client certificate, and an upload
  would bypass the Server's own hash and the audit record. Rejected.
- **Take the store's configuration from the `AWS_*` environment, or the database's from `PG*`
  variables, as their clients do by default.** `AWS_ALLOW_HTTP`, `AWS_ENDPOINT`,
  `PGSSLMODE=disable` or `PGHOST` set in a service's environment would then move the Server to
  plaintext or to another store without its configuration saying so. Rejected.
- **S3 or PostgreSQL tests only where a developer names a store.** No service in CI, nothing in
  the development stack. A defect in an adapter would then be found only by whoever runs the tests
  against a store, not by every pull request. Rejected.
- **A Compose project of its own for the stores.** It would keep the observability stack to its
  one purpose, but a developer would start and forward two stacks on the host instead of one.
  Rejected.
- **MinIO as the reference store.** The working note named it. MinIO no longer maintains its open
  source distribution; rustfs is S3-compatible, Apache-2.0, and reached 1.0 in 2026. Rejected in
  favour of rustfs.
- **An explicit copy command in `opamp-fleetctl` instead of copying blobs at startup.** It keeps
  startup free of a long first run, but adds a command and a state in which the Server refuses to
  start until it has run. Rejected.
- **`sqlx`.** Widely used, with a pool and migrations built in. Its compile-time checked queries
  need a database or prepared offline data at build time, and its tree is larger. Its TLS feature
  set has to be kept off `aws-lc-rs` as well. Rejected for the smaller `tokio-postgres`, whose
  queries are plain SQL tested against a real database.
- **`diesel`.** An ORM with a query DSL, synchronous at its core, with `diesel-async` apart. It
  brings a model layer the ports do not need. Rejected.
- **One rule of memory for both record stores.** Keeping the whole fleet in memory on PostgreSQL
  too would make every instance hold every Agent and learn of every change, and a start read the
  whole database. Querying the database per request on the filesystem store too would add a read
  of files to every report. Rejected; each store keeps the mode of clause 3.
- **The Server creates its database and role.** It would need a superuser's credentials and
  takes a decision that belongs to whoever runs the database. Rejected.
- **A client certificate towards the database instead of a password.** Managed services such as
  RDS offer no client-certificate sign-in, as the working note found. Rejected; the password comes
  from a file.
- **Mark an approval's register entry and Host as pending until its decision commits.** The
  entry would be written in a transaction of its own, marked with its request, and the decision's
  transaction would clear the mark; an approval asked again would find the pending certificate and
  hand it out instead of signing anew. Every reader of the register — admission, the list, the
  Gateways' list and its `ETag`, the bounds and the prune — would have to know the mark, and a
  mark left by a decision that failed would have to be cleared by a sweep. It saves only a
  signature that grants nothing. Rejected.
- **Write the audit entry in the transaction that decides the request.** The chain of a lease is
  given its order by one writer per instance; an entry written by the decision's transaction would
  have to stop that writer until the commit and take the chain's hash back on a rollback, past
  entries other acts wrote meanwhile. Rejected for an entry that records the outcome.
- **Record the signature only once its decision committed.** The entry would then follow an act
  that already counts, against [ADR-0039](0039-an-append-only-audit-record.md) clause 5, and a
  record that cannot take it could no longer refuse the act. Rejected.
- **Migrate at Server startup instead of a command.** It saves a step, but the first start would
  copy a whole `data_dir` while serving nothing, and a failure would leave a half-filled database
  for the next start. Rejected for an offline copy.

## Sources / Prior art

- The target architecture's working note "server split" (PostgreSQL, S3 content-addressed,
  active-active; sections D7, D8, D8a and D10), recorded outside the ADRs, and the working note
  on the Server's data model, in which the human chose each point of the layout.
- [ADR-0009](0009-ports-and-adapters.md) (ports and adapters).
- [Bindplane Bolt
  Store](https://bindplane.com/docs/advanced-setup/backup-and-disaster-recovery/bolt-store)
  and [Postgres Store](https://docs.bindplane.com/how-to-guides/postgres/postgres-store) — one
  store interface, interchangeable backends.
- `object_store` 0.14.2 ([docs.rs](https://docs.rs/object_store/0.14.2/object_store/),
  [repository](https://github.com/apache/arrow-rs-object-store)), MIT or Apache-2.0: its features
  `aws`, `aws-base`, `ring` and `reqwest`; the credential providers in `src/aws/builder.rs`
  (static keys, web identity, ECS task, EKS pod, instance metadata); `with_http_connector`, which
  takes a `reqwest::Client`; `with_root_certificate` and `with_allow_http`. A probe built with
  `aws-base`, `ring` and `reqwest` resolved `ring` and `reqwest` 0.13 with no `aws-lc-rs` and ran.
-
  [`AmazonS3Builder`](https://docs.rs/object_store/0.12.4/object_store/aws/struct.AmazonS3Builder.html),
  for the environment variables `from_env` reads.
- [RustFS documentation](https://docs.rustfs.com/) and
  [RustFS 1.0 reaches GA](https://linuxiac.com/rustfs-1-0-s3-compatible-object-storage-reaches-ga/):
  Apache-2.0, S3-compatible, multipart uploads, and MinIO's end of maintenance of its open source
  products.
- `tokio-postgres` 0.7.18 ([docs.rs](https://docs.rs/tokio-postgres/0.7.18/tokio_postgres/),
  [repository](https://github.com/rust-postgres/rust-postgres)), MIT or Apache-2.0:
  `SslMode::{Disable, Prefer, Require}` and `ChannelBinding` in `src/config.rs`.
- `deadpool-postgres` 0.14.2
  ([docs.rs](https://docs.rs/deadpool-postgres/0.14.2/deadpool_postgres/)),
  MIT or Apache-2.0, feature `rt_tokio_1`.
- `tokio-postgres-rustls` 0.14.0 ([repository](https://github.com/jbg/tokio-postgres-rustls)), MIT,
  features `ring` and `aws-lc-rs`, neither on by default; it passes the host to rustls as the
  server name. A probe built with `tokio-postgres` (`runtime`), `deadpool-postgres` (`rt_tokio_1`)
  and `tokio-postgres-rustls` (`ring`) over a TLS 1.3-only, *ring*-only `rustls::ClientConfig`
  resolved `ring` and `rustls` 0.23 with no `aws-lc-rs` and no OpenSSL, and built.
- [PostgreSQL: SSL support in libpq](https://www.postgresql.org/docs/current/libpq-ssl.html), for
  `verify-full`, and [explicit locking: advisory
  locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS).
- The code as it stands: the ports in `crates/fleet-server/src/` (`AgentStore`, `ConfigBackend`,
  `DeploymentBackend`, `PackageBackend`, `BlobStore`, `BootstrapStore`, `EnrolmentStore`,
  `LedgerStore`), their adapters under `crates/fleet-server/src/fs/` and
  `crates/fleet-server/src/postgres/`, the S3 adapter, `server migrate` in
  `crates/fleet-server/src/main.rs` and `crates/fleet-server/src/migrate.rs`, and the TLS 1.3
  provider in `crates/opamp/src/tls.rs`.

## Consequences

- Positive: one key and one tree. Every directory is owner-only, and each directory is the shape
  of a table or bucket.
- Positive: identical artifacts are stored once, and a blob's name is its integrity check.
- Positive: an approval survives a restart.
- Positive: a decision that does not commit leaves no certificate recorded, and approving the
  request again leaves one certificate for its key and one Host. The audit record says of a
  signature whether it was decided, and where it cannot, after a crash or an outcome not known,
  the register does.
- Positive: integrity rules sit behind the ports, so a database adapter enforces them without
  in-memory scans.
- Positive: G-19. The fleet's software can live in storage that scales, is replicated and is
  backed up apart from the Server, and the records in a database the operator replicates, backs
  up and restores with its own tools. Workload identity lets a Server in a cloud reach its bucket
  with no secret on disk.
- Positive: the ports, the core and the REST API are the same with every store, and the
  filesystem store remains the simplest installation. A later step to several instances starts
  from adapters that exist and a schema that is already migrated additively.
- Negative / trade-offs: every installation of the old layout runs `server migrate` once, and
  changes `server.toml` to `data_dir`; the migration cannot place the old Configurations, and the
  operator does it from the list.
- Negative / trade-offs: re-hashing every blob of the filesystem store at startup costs time with
  a large store. An object altered in the bucket is not detected by the Server; it is served, and
  the Client refuses it.
- Negative / trade-offs: four new dependencies, `object_store` with `reqwest`'s stream feature and
  an XML parser, `tokio-postgres`, `deadpool-postgres` and `tokio-postgres-rustls`, for
  `cargo-deny` to check, with feature sets that must stay off `aws-lc-rs`.
- Negative / trade-offs: on PostgreSQL the approval holds the register's table lock from the
  register's write until its decision commits, as the register's own transaction held it, so a
  commit that waits for the notification lock holds renewals on every instance as long. An
  approval whose decision does not commit has spent a signature and a serial.
- Negative / trade-offs: on the filesystem store a decision written whose register entry can be
  neither written nor taken back stays decided on disk until the next start puts its request back.
- Negative / trade-offs: two stores per port to test and document. The S3 and PostgreSQL tests
  need a store, so they run in CI jobs of their own and against the development stack, and are
  skipped in a plain `cargo test`.
- Negative / trade-offs: with PostgreSQL and the filesystem blob store the Server's state is in
  two places, the database and `data_dir` (the blobs). A backup takes both. On the filesystem
  store the whole fleet is loaded at startup and held in memory, so a start takes as long as
  reading it; on PostgreSQL every list the Operator plane shows is a query (clause 3). A first
  start on S3 with a large `data_dir/blobs/` takes as long as the upload of
  what is missing.
- Follow-ups: whether the sweep of unreferenced blobs moves to a background task once the store is
  shared; whether a separate blob directory is needed; for the operator manual: backing up and
  restoring PostgreSQL and the bucket is the operator's, and a database restored from before a
  revocation lets the revoked certificate in again, so after a restore the operator sets again the
  revocations the audit record holds since the backup.

## Enforcement

Tests that carry `Verifies: ADR-0044`:

- Layout, ports and migration of the old layout:
  [`fs/mod.rs`](../../crates/fleet-server/src/fs/mod.rs)
  `the_data_directory_has_one_layout_and_no_other`;
  [`config.rs`](../../crates/fleet-server/src/config.rs)
  `a_leftover_config_dir_or_packages_dir_fails_startup_naming_server_migrate`;
  [`fs/agents.rs`](../../crates/fleet-server/src/fs/agents.rs)
  `a_rollout_cut_short_after_its_revisions_leaves_a_state_the_next_load_accepts`,
  `the_store_answers_what_its_assignments_pin`;
  [`fleet.rs`](../../crates/fleet-server/src/fleet.rs)
  `a_rollout_that_fails_part_way_leaves_disk_and_memory_as_they_were`;
  [`fs/revocation.rs`](../../crates/fleet-server/src/fs/revocation.rs)
  `each_host_is_a_file_of_its_own_and_its_name_must_match`;
  [`shutdown_signal.rs`](../../crates/fleet-server/tests/shutdown_signal.rs)
  `sigterm_shuts_the_server_down_gracefully`;
  [`migrate.rs`](../../crates/fleet-server/src/migrate.rs)
  `migrate_carries_an_old_layout_into_a_data_directory`,
  `migrate_refuses_a_data_directory_that_is_not_empty_and_fails_on_lost_trust_data`,
  `differing_signatures_leave_the_entry_unsigned_and_named`,
  `a_mismatching_artifact_leaves_the_blob_another_entry_holds`,
  `a_package_that_does_not_carry_leaves_its_deployments_and_assignments`,
  `a_deployment_naming_a_deleted_package_loses_that_name`;
  [`migrate_cli.rs`](../../crates/fleet-server/tests/migrate_cli.rs)
  `migrate_writes_a_new_data_directory_once_and_says_so`.
- Enrolment, bootstrap and audit:
  [`enrolment.rs`](../../crates/fleet-server/src/enrolment.rs)
  `an_approved_enrolment_survives_a_restart`,
  `a_window_whose_end_passed_while_down_is_closed_on_load`,
  `an_act_that_cannot_be_persisted_changes_nothing`;
  [`fs/enrolment.rs`](../../crates/fleet-server/src/fs/enrolment.rs)
  `the_window_persists_in_enrolment_json`;
  [`fs/bootstrap.rs`](../../crates/fleet-server/src/fs/bootstrap.rs)
  `a_pkcs12_file_is_kept_owner_only_and_removed_with_its_entry`,
  `entries_survive_a_reopen_and_a_removal_persists`;
  [`audit_log.rs`](../../crates/fleet-server/src/audit_log.rs)
  `a_batch_rotates_where_a_single_line_would`.
- Blobs: [`fs/blobs.rs`](../../crates/fleet-server/src/fs/blobs.rs)
  `identical_bytes_are_one_blob_named_by_its_hash`,
  `a_blob_that_does_not_match_its_name_fails_the_open_naming_it`;
  [`packages.rs`](../../crates/fleet-server/src/packages.rs)
  `identical_bytes_are_one_blob_that_goes_with_its_last_entry`, `a_corrupt_artifact_fails_reopen`,
  `a_store_in_an_older_layout_refuses_to_open_and_names_what_is_in_the_way`,
  `an_upload_never_records_a_blob_a_deletion_takes`,
  `a_slow_blob_store_holds_no_reader_of_the_packages`,
  `a_store_that_cannot_be_asked_fails_the_upload_and_keeps_the_blob`,
  `a_sweep_whose_marking_fails_takes_its_marks_away`,
  `a_sweep_asks_before_each_deletion_and_passes_over_a_blob_no_longer_marked`,
  `a_sweep_suspended_after_it_renewed_a_mark_deletes_nothing_more`,
  `a_sweep_paused_within_the_bound_of_its_renewal_deletes_as_before`,
  `marks_that_could_not_be_taken_away_are_taken_away_by_the_next_try`;
  [`s3.rs`](../../crates/fleet-server/src/s3.rs) `the_blob_port_contract_holds_on_the_filesystem`,
  `the_blob_port_contract_holds_on_s3`,
  `leftover_staging_objects_go_and_unknown_keys_fail_startup_on_s3`,
  `a_bucket_that_does_not_exist_fails_startup_naming_it`,
  `a_store_that_never_answers_fails_the_call_within_the_limits`,
  `aws_variables_change_neither_the_endpoint_nor_plaintext`,
  `the_secret_file_is_read_and_must_be_the_servers_alone`,
  `switching_to_s3_copies_what_the_bucket_lacks_and_leaves_the_file`,
  `the_copy_from_the_filesystem_verifies_each_file_and_leaves_it`;
  [`config.rs`](../../crates/fleet-server/src/config.rs)
  `the_blob_store_is_chosen_and_checked_naming_the_key`,
  `the_blob_store_is_configured_from_the_environment_too`.
- PostgreSQL: one port contract test per port runs against both adapters —
  `the_agent_port_contract_holds_on_*`, `the_configuration_port_contract_holds_on_*`,
  `the_deployment_port_contract_holds_on_*`, `the_package_port_contract_holds_on_*`,
  `the_bootstrap_port_contract_holds_on_*`, `the_enrolment_port_contract_holds_on_*`,
  `the_ledger_port_contract_holds_on_*` (`postgres` and `the_filesystem`) — with the partial-write
  and undecodable-row tests beside them in
  [`crates/fleet-server/src/postgres/`](../../crates/fleet-server/src/postgres/) (`agents.rs`,
  `configs.rs`, `deployments.rs`, `packages.rs`, `bootstrap.rs`, `enrolment.rs`, `ledger.rs`);
  [`postgres/agents.rs`](../../crates/fleet-server/src/postgres/agents.rs)
  `the_count_of_agents_follows_every_addition_and_removal_and_decides_the_ceiling`,
  `a_creation_waits_for_a_pooled_connection_longer_than_any_other_call`;
  [`postgres/mod.rs`](../../crates/fleet-server/src/postgres/mod.rs)
  `the_connection_requires_tls_unless_the_configuration_waives_it`,
  `the_database_certificate_is_verified_against_its_root_and_its_host_name`,
  `a_password_file_others_may_read_fails_startup_naming_it`,
  `pg_variables_change_neither_the_database_nor_plaintext`,
  `an_unreachable_database_fails_startup_naming_it`,
  `a_database_that_never_answers_fails_startup_within_the_bound`,
  `a_database_that_stops_answering_fails_the_call_within_the_bound`,
  `the_embedded_schema_applies_on_an_empty_database`,
  `the_server_applies_its_schema_and_refuses_a_newer_one`;
  [`postgres/schema.rs`](../../crates/fleet-server/src/postgres/schema.rs)
  `a_migration_only_adds`, `the_migrations_are_numbered_and_this_server_understands_them`;
  [`postgres/import.rs`](../../crates/fleet-server/src/postgres/import.rs)
  `migrate_to_postgres_carries_every_record_and_leaves_the_old_data_dir`,
  `a_copy_checks_the_database_before_it_touches_anything`,
  `a_copy_that_fails_leaves_the_database_without_the_servers_tables`;
  [`config.rs`](../../crates/fleet-server/src/config.rs)
  `the_record_store_is_chosen_and_checked_naming_the_key`,
  `the_record_store_is_configured_from_the_environment_too`.
- The S3 and PostgreSQL tests run in CI jobs with a rustfs and a PostgreSQL service, and are
  reported skipped where no store is named. `cargo tree` of the Server contains neither
  `aws-lc-rs` nor `openssl`; `cargo-deny` fails on a licence or source outside the allow list.
- Tests that carry `Verifies: ADR-0044`, for clauses 2, 10, 21 and 22:
  [`postgres/ledger.rs`](../../crates/fleet-server/src/postgres/ledger.rs)
  `the_list_version_moves_with_each_revocation_and_lift_and_names_only_the_list_held`,
  `a_register_change_moves_the_list_version`,
  `the_prune_keeps_an_entry_a_valid_renewal_in_the_database_descends_from`;
  [`tests/instances.rs`](../../crates/fleet-server/tests/instances.rs)
  `a_renewal_whose_notification_is_missed_is_refused_once_its_predecessor_is_revoked`;
  [`s3.rs`](../../crates/fleet-server/src/s3.rs)
  `the_cleanup_leaves_the_staging_objects_of_a_live_lease_and_deletes_a_lapsed_ones`,
  `the_cleanup_lists_the_staging_keys_before_it_asks_which_leases_are_live`,
  `the_next_sweep_on_any_instance_deletes_a_released_leases_staging_object_once`;
  [`postgres/packages.rs`](../../crates/fleet-server/src/postgres/packages.rs)
  `a_sweep_keeps_a_blob_an_entry_stored_through_another_writer_names`,
  `an_upload_holds_off_a_sweep_until_its_entry_is_committed`,
  `an_upload_whose_blob_a_sweep_took_first_is_refused`,
  `an_upload_whose_session_ends_before_its_commit_writes_no_entry`,
  `a_hold_whose_session_ends_is_marked_lost`,
  `an_upload_beside_a_sweep_whose_session_ended_keeps_its_artifact`,
  `a_sweep_whose_hold_is_lost_deletes_nothing_more_and_says_so`,
  `a_sweep_marks_nothing_once_its_session_has_ended`,
  `a_sweep_takes_away_old_marks_of_a_lease_that_is_not_live_and_keeps_the_others`,
  `a_sweep_renews_a_marks_time_and_finds_a_blob_another_sweep_unmarked`,
  `the_marks_outlive_a_renewal_its_call_and_the_longest_blob_call`,
  `a_startup_sweeps_marks_outlive_its_lost_session_and_keep_an_uploads_artifact`,
  `a_lapsed_leases_marks_outlive_its_lost_session_and_keep_an_uploads_artifact`,
  `a_sweep_whose_hold_is_lost_cleans_up_no_staging_object`,
  `a_sweep_waits_its_whole_wait_for_the_lock_and_an_upload_five_seconds`,
  `a_startup_sweep_that_cannot_have_the_lock_fails_naming_it`;
  [`fleet.rs`](../../crates/fleet-server/src/fleet.rs)
  `a_package_deletion_sweeps_once_it_lets_the_fleet_lock_go`;
  [`postgres/import.rs`](../../crates/fleet-server/src/postgres/import.rs)
  `migrate_to_postgres_with_s3_uploads_the_referenced_blobs_and_takes_no_data_dir`,
  `blobs_to_s3_uploads_the_referenced_blobs_and_checks_every_entry`,
  `blobs_to_s3_holds_the_blob_lock_alone_and_fails_naming_it_when_held`.
- That two writers on the database keep both of what they write, and that the migrations are
  applied once while instances start together, is decided by tests that carry the marker of
  [ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
  or of
  [ADR-draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side](draft-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md):
  [`tests/instances.rs`](../../crates/fleet-server/tests/instances.rs)
  `two_revocations_committed_at_once_on_two_instances_are_both_kept`,
  `an_instance_with_an_older_floor_in_memory_cannot_lower_the_stored_one`,
  `two_revisions_rolled_out_at_once_on_two_instances_are_both_kept`,
  `two_entries_added_at_once_on_two_instances_are_both_kept`,
  `two_members_added_at_once_on_two_instances_are_both_kept`,
  `this_release_starts_beside_a_live_lease_of_0_8_0_and_migrates_once`,
  `two_versions_serve_side_by_side_and_the_newer_migrates_once`;
  [`postgres/ledger.rs`](../../crates/fleet-server/src/postgres/ledger.rs)
  `two_binders_of_one_host_bind_both_without_a_deadlock`,
  `a_gateway_mark_finds_a_host_another_instance_wrote`;
  [`postgres/enrolment.rs`](../../crates/fleet-server/src/postgres/enrolment.rs)
  `two_takers_at_the_queues_bound_take_one_request_between_them`.
- Tests that carry `Verifies: ADR-0044`, for clause 23:
  [`postgres/mod.rs`](../../crates/fleet-server/src/postgres/mod.rs)
  `a_second_instance_that_must_be_alone_fails_startup_naming_it`,
  `a_server_that_loses_its_lock_logs_it_and_writes_nothing_more`. Beside them, with the marker of
  [ADR-0041](0041-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md):
  [`postgres/lease.rs`](../../crates/fleet-server/src/postgres/lease.rs)
  `instances_that_share_the_database_each_hold_a_lease`,
  `an_instance_that_must_be_alone_is_alone`;
  [`tests/instances.rs`](../../crates/fleet-server/tests/instances.rs)
  `an_instance_without_a_lease_closes_its_listeners_until_it_holds_a_new_one`, which opens the
  listeners again only once the instance holds a new lease. No test of its own decides that an
  instance takes its lease before it first opens a listener.
- Tests that carry `Verifies: ADR-0044`, for clause 22:
  [`postgres/mod.rs`](../../crates/fleet-server/src/postgres/mod.rs)
  `a_server_of_0_7_1_refuses_the_schema_once_a_migration_of_0_8_0_is_applied`: the schema of
  migrations 1 and 2 is taken by 0.7.1, and after each of migrations 3 to 10 it is refused, naming
  0.8.0, and left as it was; and
  `a_shared_instance_migrates_nothing_while_a_server_holds_the_database_alone`: with the exclusive
  lock held on the schema of migrations 1 and 2, as a running Server of 0.7.1 holds it, an
  instance that shares the database fails startup and the schema is left as it was, and once the
  lock is free the instance migrates and starts.
- Tests that carry `Verifies: ADR-0044`, for clauses 1, 16 and 27:
  [`fs/mod.rs`](../../crates/fleet-server/src/fs/mod.rs)
  `a_data_dir_beside_the_database_holds_the_blobs_alone`, that the Server makes `blobs/` in a
  `data_dir` beside the database with the filesystem blob store and keeps `audit/` there only as
  the archive an older Server left; [`config.rs`](../../crates/fleet-server/src/config.rs)
  `with_postgresql_and_s3_a_named_data_dir_fails_startup_naming_it`, that with S3 the Server has
  no `data_dir` and refuses one the configuration names;
  [`fs/mod.rs`](../../crates/fleet-server/src/fs/mod.rs)
  `a_data_directory_read_in_place_is_not_written`;
  [`s3.rs`](../../crates/fleet-server/src/s3.rs)
  `a_store_opened_before_its_lease_takes_no_upload_until_the_lease_is_named`;
  [`tests/stateless_instance.rs`](../../crates/fleet-server/tests/stateless_instance.rs)
  `a_bucket_that_fails_startup_leaves_no_lease_behind`.
- Tests that carry `Verifies:
  ADR-draft-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision`,
  for clauses 2, 9 and 21:
  [`postgres/enrolment.rs`](../../crates/fleet-server/src/postgres/enrolment.rs)
  `an_approval_whose_commit_fails_records_nothing_and_a_second_records_one_certificate`,
  `two_instances_approving_one_request_whose_first_commit_fails_sign_it_once_more`,
  `an_approval_whose_register_entry_is_refused_rolls_its_decision_back`,
  `a_commit_the_database_refuses_is_answered_as_a_refusal`,
  `only_an_error_at_the_commit_is_a_certain_rollback`,
  `a_read_back_waits_for_the_transaction_that_holds_the_request`;
  [`enrolment.rs`](../../crates/fleet-server/src/enrolment.rs)
  `a_commit_that_landed_and_whose_answer_was_lost_is_read_back_as_approved`,
  `a_signature_whose_decision_was_not_written_is_recorded_undecided`,
  `a_read_back_that_finds_another_certificate_rolls_the_signature_back`,
  `a_refusal_the_database_answers_is_rolled_back_without_a_read_back`;
  [`fleet.rs`](../../crates/fleet-server/src/fleet.rs)
  `an_approval_on_files_whose_register_entry_fails_takes_its_decision_back`,
  `a_decision_found_at_startup_without_its_register_entry_waits_again`,
  `the_audit_record_of_an_approval_that_did_not_commit_verifies_and_names_it_undecided`.
- Clause 12 as it stands here, retention by `retain_days` on the PostgreSQL store included, is
  verified by the tests
  [ADR-0042](0042-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)
  names.
