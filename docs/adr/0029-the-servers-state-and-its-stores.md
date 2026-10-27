# ADR-0029: The Server's state lives under one data directory or in PostgreSQL and S3, one store per domain

- **Status:** 🟢 accepted
- **Date:** 2026-10-23
- **Deciders:** Markus Brigl
- **Applies to:** every storage port in `crates/fleet-server/src/` (`agent_store.rs`, `configs.rs`, `deployments.rs`, `packages.rs`, `blobs.rs`, `bootstrap.rs`, `enrolment.rs`, `revocation.rs`, `audit.rs`) and every adapter under `crates/fleet-server/src/fs/`, `crates/fleet-server/src/postgres/` and `crates/fleet-server/src/s3.rs`, the embedded schema and its migrations, the wiring in `crates/fleet-server/src/lib.rs` and `crates/fleet-server/src/main.rs`, `data_dir` and the `[blobs]` and `[database]` sections of `server.toml` and their parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, the `server migrate` and `server migrate-to-postgres` commands, the `object_store`, `tokio-postgres`, `deadpool-postgres` and `tokio-postgres-rustls` dependencies in `Cargo.toml` and `deny.toml`, the S3 and PostgreSQL adapters' tests and the CI jobs that run them, the rustfs and postgres services in `observability/compose.yaml` and their forward into the Dev Container, `docs/ARCHITECTURE.md`, and every document that names where the Server keeps its state

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
([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md) clause 5): effective configurations,
bootstrap PKCS#12 files, the certificate register. Q-1 forbids sending a credential or a package
unencrypted beyond the loopback, which holds for the Server's traffic to its database and its
object store as much as for an Agent's. Q-3 bounds what a new dependency may bring:
[ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) keeps the TLS stack on rustls with
*ring*, without `aws-lc-rs` and without OpenSSL, and TLS 1.3 alone.

The artifacts are the largest part of the state: one entry of 1 GiB is allowed
(`max_package_size_bytes`), and the fleet's software multiplies that. On the Server's host they
share a disk with the audit record and the register, are backed up with them, and bind the Server
to one machine. Files bind the records to one host and one disk too: a backup is a copy of a
directory taken while the Server writes into it, and nothing but the Server can read the records
consistently. The target architecture recorded outside the ADRs, the working note "server split",
puts every store in PostgreSQL and the artifacts in S3-compatible storage, content-addressed and
immutable, streamed through the Server in both directions, with the password read from a file and
the connection verified against the database's certificate and host name. Running several Server
instances against one database or one bucket is a further step with its own questions —
presence, leases, notifying the other instances — and waits for a decision of its own.

Existing installations must keep their trust data. Starting empty would lose the revocation list,
and a revoked certificate would be admitted again. The project reads no older store shapes inside
the Server.

A reader without this conversation would expect several things that this ADR decides against:
that S3 and PostgreSQL replace the filesystem, that the Server hashes every stored object again at
startup as it hashes every file, that the Server creates its database, that with a database the
in-memory fleet stops being what the Server answers from, and that the stores' endpoints and
options come from the `AWS_*` and `PG*` environment variables, as their clients read them by
default.

## Decision

We will keep all of the Server's state under one `data_dir`, one store per domain behind a port
shaped for a database or an object store, and let the operator choose in `server.toml` whether
the records live there or in one PostgreSQL database and whether the artifact bytes, kept by their
SHA-256, live there or in one bucket of an S3-compatible object store, each reached over TLS 1.3
and configured in the Server's configuration alone, and move an old layout by an offline command.

1. **One key chooses the record store; `data_dir` stays.**
   - `[database] store` is `"fs"`, the default, or `"postgres"`. Any other value fails startup
     naming the key. Both are supported in production.
   - `server.toml` names `data_dir` (default `fleet-data`) with either store. With `"fs"` it
     holds every record; with `"postgres"` it holds the audit record and, with the filesystem
     blob store, the blobs. Its layout is fixed
     ([ADR-0028](0028-package-versions-high-water-mark-and-floor.md)).
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
   - Integrity questions are port queries, not scans in the caller. Examples are "is this
     Configuration, Package or Deployment pinned by an assignment?" and "which revisions are
     referenced?". The filesystem adapter may answer them from what it holds in memory.
   - A port never assumes that it is the only writer. Nothing outside the adapter caches its
     records across a request without the adapter's say.

3. **The in-memory state stays what the Server answers from.** With either record store, the
   Server loads every record through its port at startup, answers from memory, and writes through
   the port before an act is answered, as
   [ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md) clause 4 states. The database is the
   durable copy, not a second source: nothing but this Server writes to it while it runs
   (clause 23).

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
   - The PostgreSQL row holds the same fields, the wire-typed ones as `bytea` (clause 21). A row
     that does not decode fails startup naming its `instance_uid`.

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
     host enrols again in the next window.

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
   - It holds at most 100 000 certificates, and at most 10 000 in one chain — below one root, the
     first certificate the chain renews from. It refuses a renewal once fewer than 1 000 places
     are left, so renewals alone cannot shut out an enrolment. A CSR beyond any of these is
     refused.
   - An entry stays while the certificate, or one renewed from it, is still valid.
     `GET /api/v1/certificates` lists it.

10. **The revocation list is persisted and bounded.** It lives in `data_dir/revocations.json`
    with the filesystem store and in the database with PostgreSQL, is written before the request
    is answered — atomically, or in one committed transaction — and survives a restart. It holds
    at most 100 000 entries, and a revocation beyond that is answered `507`. A certificate entry
    is dropped with its register entry once neither the certificate nor any renewal of it is
    valid. An entry for a certificate the register never held stays until an operator removes
    it.

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
      ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)) does not bring it
      back.
    - At most 10 000 valid entries are kept, and a request beyond that is answered `409`.
    - A bootstrap certificate is revoked as
      [ADR-0021](0021-admission-by-client-certificate-and-revocation.md) states for revocation,
      with the authority `bootstrap`.
    - The audit records each issuance as `enrolment.bootstrap_issued`, with serial, subject,
      `not_after` and `p12`. It records each download of a PKCS#12 file as
      `enrolment.bootstrap_p12_downloaded` with the serial. It records each deletion as
      `enrolment.bootstrap_deleted` with the serial and subject
      ([ADR-0023](0023-an-append-only-audit-record.md)).
    - When the audit cannot be written, nothing is handed out or deleted, and the request is
      answered `503`.

12. **The audit file.**
    - The audit is written to `data_dir/audit/audit-<first seq>.jsonl`, with either record
      store. The directory is `0700` and each file `0600` on Unix, under the data root's
      restricted ACL on Windows.
    - Each entry is one line. It is written with a single append and synced to disk by one writer
      thread of its own, in the order the decisions were taken. A write that fails partway is cut
      back to the last whole line.
    - A file reaching `[audit] max_file_bytes` (default 64 MiB) is closed and a new one opened.
      The Server keeps `[audit] keep_files` files (default 16) and deletes the oldest. Before it
      does, it records `audit.rotated` with the deleted file's last hash.

13. **Artifact bytes are kept by their hash, behind a blob port.**
    - The port is `put(stream) → (sha256, size)`, `get(sha256) → stream`, `exists(sha256)`,
      `delete(sha256)` and `list() → [(sha256, size)]`. It is synchronous; an adapter that speaks
      an asynchronous protocol bridges inside itself, and no caller changes.
    - A blob is named by the lowercase hex SHA-256 of its bytes and by nothing else. Bytes already
      stored are stored once.
    - A blob that no Package entry references is deleted when its last entry goes, and at
      startup.
    - The size limits of a Package entry
      ([ADR-0027](0027-packages-signatures-and-delivery.md)) count the blobs.
    - A Package's metadata names a blob by its hash only.

14. **One blob store, chosen in `server.toml`.** `[blobs] store` is `"fs"`, the default, or
    `"s3"`. Any other value fails startup naming the key. Both are supported in production. The
    store is read and written by one Server, unless a later decision on several Server instances
    says how they share one bucket and prefix.

15. **The filesystem blob store** keeps one file per blob in `data_dir/blobs/`. It streams an
    upload to a staging file inside `blobs/`, hashes it on the way, and renames it to its hash. At
    startup every blob is hashed again, and one whose content does not match its name fails
    startup, naming it.

16. **The S3 store keeps one object per blob under a fixed layout.**
    - `[blobs.s3]` names `endpoint`, `bucket`, `region` and an optional `prefix`. Objects live at
      `<prefix>blobs/<sha256>`; an upload in progress lives at `<prefix>staging/<random>`. The
      Server touches no key outside `<prefix>`. A key under `<prefix>blobs/` that is not a hash,
      or any other key under `<prefix>`, fails startup naming the key; a key under
      `<prefix>staging/` is deleted at startup.
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

19. **Switching from the filesystem to S3 copies what is missing.** When `store = "s3"` and
    `data_dir/blobs/` exists, the Server hashes, at startup, every file there that a Package
    entry references and that the bucket does not hold, uploads it as `put` does, and logs each
    one. A file whose content does not match its name fails startup, naming it. The Server does
    not delete `data_dir/blobs/`, does not write to it again, and with `store = "s3"` does not
    create it; removing it is the operator's. The reverse switch copies nothing: a Package entry
    whose blob the chosen store does not hold fails startup, naming the entry.

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
    - Every port of clause 2 but the blob port and the audit port has an adapter that keeps its
      records in tables of the Server's schema: Configurations with their revisions,
      Deployments, Package records, Agents with their labels and assignments, Hosts, the
      certificate register, the revocation list, bootstrap entries with their PKCS#12 files, and
      the enrolment window with its queue.
    - What one port call writes is one transaction. A rollout's revisions and assignments commit
      together or not at all, and a bootstrap entry commits with its PKCS#12 file.
    - Wire-typed fields of an Agent are kept as protobuf bytes, as the filesystem envelope keeps
      them (clause 4), so a field upstream adds round-trips unread. Scalars are columns.
    - Text an Agent or a CSR chose — an Agent's effective configuration, a certificate's subject
      — is kept as `bytea`, and every JSON document as `json`: PostgreSQL's `text` and `jsonb`
      refuse a NUL character, which a file keeps, and one Agent's report must never fail the
      writes of others or a rollout. The same bytes round-trip as with the filesystem store.
    - A call that answers a security decision returns only after its transaction has committed.
    - The audit record stays in `data_dir/audit/` with either store (clause 12). Moving it into
      the database is a later decision on audit chains per instance.

22. **The Server applies its own schema.**
    - The schema is embedded in the binary as numbered migrations. At startup the Server applies,
      in one transaction, every migration the database has not yet seen, and records each in a
      table of its own.
    - A migration only adds: tables, columns with defaults, indexes. A column or table is removed
      only by a release after the one that stopped using it, so the release before a change runs
      on the schema after it.
    - The schema table names the oldest Server version that understands it. A database whose
      schema a Server does not understand fails that Server's startup, naming both versions; the
      Server changes nothing in it.
    - The Server creates tables, indexes and its schema table, and nothing else. The database,
      the role, its rights and its extensions are the operator's.

23. **One Server per database schema.** At startup the Server takes a session-level advisory lock
    named after the schema it works in — the role's default — and holds it on a connection of its
    own while it runs. A second Server against the same database and schema fails startup, naming
    the database; Servers in different schemas of one database share nothing, as the tables are the
    schema's. When the lock's connection ends while the Server runs, the lock is gone with it: the
    Server logs an error naming the database and refuses every call from then on, so it never writes
    beside a second Server. Several instances against one schema are a later decision.

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
    - Connections come from a `deadpool-postgres` pool. A database that cannot be reached at
      startup fails startup, naming host and database; one lost while the Server runs fails the
      calls that need it, which answer as a failed write answers.
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
    `server migrate-to-postgres --from <data_dir> --data-dir <new>` runs while the Server is
    stopped, with the `server.toml` that names the database.
    - It refuses a database that holds any of the Server's tables, and a `--data-dir` that is not
      empty.
    - It applies the schema, copies every record of the old `data_dir` into the database in one
      transaction, and copies `audit/` and, with the filesystem blob store, `blobs/` into the new
      `data_dir`, the audit lines unchanged so the chain holds.
    - It copies rather than moves: the old `data_dir` stays as the backup. It prints what it
      copied and exits non-zero when it could not copy all of it, leaving the database without
      the Server's tables.
    - Moving records back from PostgreSQL to files is not provided.

**Out of scope:** several Server instances
against one database or one bucket, presence, leases and notifying other instances; the audit
record in the database; paging and filtering in the database; moving records from PostgreSQL back
to files or blobs from S3 back to the filesystem; Object Lock and bucket versioning, which the
operator may enable but the Server neither needs nor checks; server-side encryption beyond what
the bucket's own default applies; keeping the effective configuration other than as the display
text it is; migrating between two `data_dir` versions, which the first such change decides;
anything that runs, provisions, replicates or monitors a production store or database.

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
  against [ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md). The `aws-base`, `ring` and
  `reqwest` features build the same client on *ring*.
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
- **The database as the source of truth, queried per request.** It is what several instances
  will need. With one Server per database it adds a round trip to every report and changes every
  read path at once. Rejected for one Server; the in-memory state stays (clause 3).
- **The Server creates its database and role.** It would need a superuser's credentials and
  takes a decision that belongs to whoever runs the database. Rejected.
- **A client certificate towards the database instead of a password.** Managed services such as
  RDS offer no client-certificate sign-in, as the working note found. Rejected; the password comes
  from a file.
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
- Negative / trade-offs: two stores per port to test and document. The S3 and PostgreSQL tests
  need a store, so they run in CI jobs of their own and against the development stack, and are
  skipped in a plain `cargo test`.
- Negative / trade-offs: with PostgreSQL the Server's state is in two places, the database and
  `data_dir` (the audit record, and blobs with the filesystem blob store). A backup takes both.
  The whole fleet is still loaded at startup and held in memory, so a start against a large
  database takes as long as reading it. A first start on S3 with a large `data_dir/blobs/` takes
  as long as the upload of what is missing.
- Follow-ups: several Server instances against one database and one bucket, with presence, leases
  and notifications; the audit record in the database, one chain per instance; paging and
  filtering the REST API in the database; how a backup takes the database and `data_dir`
  together; whether the sweep of unreferenced blobs moves to a background task once the store is
  shared; whether a separate blob directory is needed.

## Enforcement

Tests that carry `Verifies: ADR-0029`:

- Layout, ports and migration of the old layout:
  [`fs/mod.rs`](../../crates/fleet-server/src/fs/mod.rs)
  `the_data_directory_has_one_layout_and_no_other`,
  `a_data_dir_beside_the_database_holds_the_audit_and_the_blobs_alone`;
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
  `a_store_that_cannot_be_asked_fails_the_upload_and_keeps_the_blob`;
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
  [`postgres/mod.rs`](../../crates/fleet-server/src/postgres/mod.rs)
  `the_connection_requires_tls_unless_the_configuration_waives_it`,
  `the_database_certificate_is_verified_against_its_root_and_its_host_name`,
  `a_password_file_others_may_read_fails_startup_naming_it`,
  `pg_variables_change_neither_the_database_nor_plaintext`,
  `a_second_server_against_the_same_database_fails_startup_naming_it`,
  `a_server_that_loses_its_lock_logs_it_and_writes_nothing_more`,
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
