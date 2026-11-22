# Architecture — OpAMP Fleet

> The system **as it stands today**: the parts it is built from, what each is responsible for, and
> how they fit together. It holds no rule and no decision. It names the ADR behind each structural
> choice; where it disagrees with an accepted ADR, the ADR is right and this document is stale. It
> is updated in the same change as the structure it describes
> ([NUC-documents-are-fixed](adr/NUC-documents-are-fixed.md)).
>
> **Kept current by:** Markus Brigl. A document everyone may edit and nobody owns is
> the one that goes stale.
>
> **Last design revision:** 2026-11-24, due after 50 changes. The revision that took the count
> moves the date; the number is this project's to set; a sensor counts the changes outside
> `docs/` since the commit that set the date and says when the next is due
> ([NUC-design-is-revised §4](adr/NUC-design-is-revised.md)).

## Context

What sits outside the system and what crosses its boundary. A diagram earns its place here more
than anywhere else, and none beats one that has stopped being true.

```text
 operator ── REST / bundled UI ──▶ Operator plane :4321 ─┐
                                                         │  Server (Linux): one, or several
 Client ──── OpAMP, mTLS ──────────────▶ Agent plane :4320 ─┘   │  instances, each opening one plane
   │  ▲                                                          │  or both, behind a balancer
   │  │                                                          └─▶ data_dir, or PostgreSQL with
   │  └── Gateway (a Client) ◀── OpAMP, mTLS ── other Clients        the blobs as files or in an S3
   │                                                                 bucket, which instances share:
   │                                                                 fleet state, register, audit
   │                                                                 record, artifacts by SHA-256
   ├── Supervisor Endpoint (loopback) ◀── a Collector's opampextension
   ├── Managed Processes: Collector, each wrapped kind's agent, any command — where a process
   │     account is registered, under it, started by the second service over a Unix socket or
   │     named pipe
   ├── package downloads: the Server's own origin, and no other
   ├── own-telemetry destinations: OTLP/HTTP, as the Server offers them
   └── service manager: systemd, launchd or the Windows SCM
```

- **Operators** drive the fleet through the REST API on the Operator plane, guarded by
  `[rest.auth]` beyond the loopback; the bundled UI uses the same API
  ([SEC-admission-by-client-certificate](adr/SEC-admission-by-client-certificate.md)).
- **Clients** reach the Agent plane over OpAMP — WebSocket or plain HTTP — admitted by the client
  certificate in the TLS 1.3 handshake alone
  ([SEC-tls13-bounded-input-chosen-planes](adr/SEC-tls13-bounded-input-chosen-planes.md),
  SEC-admission-by-client-certificate). A Client in Gateway Mode carries other Clients' Agents over
  its own upstream connections and refuses what the Server revoked
  ([CLT-modes-and-gateway](adr/CLT-modes-and-gateway.md)).
- **Managed Processes** run on the Client's host under a Supervisor each; a Collector reports
  through its own `opampextension` to the Supervisor Endpoint, which admits only the process its
  Supervisor started ([CLT-modes-and-gateway](adr/CLT-modes-and-gateway.md)).
- **Package downloads** come from the Server alone: every artifact is uploaded to it, and a
  Client fetches only from its own Server's origin, or from its Gateway. What arrives is checked
  against its hash and the operator's signature
  ([PKG-signed-packages-and-delivery](adr/PKG-signed-packages-and-delivery.md)).
- **The service manager** starts the Client and restarts it after a self-update
  ([CLT-self-update-signed-package](adr/CLT-self-update-signed-package.md)). Where a process
  account is registered, it also runs the second service, which starts the Managed Processes
  under that account (CLT-accounts-service-runs-under).
- **Upstream**, the opamp-spec release named as the Protocol Baseline in
  [`CONFORMANCE.md`](CONFORMANCE.md) fixes the wire format.

## Building blocks

The parts the system is made of, each with one responsibility, named in the vocabulary of
[`GLOSSARY.md`](GLOSSARY.md). One level deep, deeper only where the size of a part earns it. The
golden path and the test pattern an agent copies from are named here.

Six crates in one workspace
([DEV-six-crates-tokio-axum](adr/DEV-six-crates-tokio-axum.md)):

- **`opamp`** — the OpAMP communication layer, publishable on its own
  ([DEV-six-crates-tokio-axum](adr/DEV-six-crates-tokio-axum.md)).
  Always the
  generated types (`proto`), the framing of a WebSocket message behind its varint header (`frame`),
  and the endpoint's body rules. Behind `client`, an Agent's protocol state machine
  (`client::protocol`), the two transports — `client::http`, one POST per exchange, and
  `client::ws`, one connection for every Agent of the session — and `client::connection`, which
  builds a connection, its TLS and its HTTP client from a `Connection` the application fills in,
  and every other HTTPS client of the Client through `ClientTls::https_client`: TLS 1.3, no
  redirect followed, bounded by its `Timeouts`. The WebSocket driver ends a connection on which
  nothing arrived for one heartbeat interval (`client::ws::Silence`)
  ([API-silent-websocket-is-ended](adr/API-silent-websocket-is-ended.md)),
  and both drivers draw their reconnects and their waits after a refusal or an `Unavailable` from
  `client::backoff`. Both ends draw their spread from `random`: the Client's reconnect jitter and
  the `Retry-After` of a Server that sheds load. `uid` is the `InstanceUid` and `attributes` the
  attribute keys both sides use.
  Behind `server`, the endpoint around a `Handler`, which answers a WebSocket's pings while it
  handles a message, `server::listen`, the listener with its TLS and its bounds — among them a
  cap on the connections it holds and the new ones it takes a second, a deadline on the TLS
  handshake, one timeout on the time to a connection's first request and on an HTTP/1 request's
  headers, and HTTP/2's bounds on streams and pings — and `server::pace`, the floor every body
  and WebSocket message it serves is held to. The server side takes a WebSocket over through
  hyper's upgrade and reads its frames with `tokio-tungstenite`, the library the client side
  uses too. Both sides take their WebSocket configuration from `websocket`: the receive limit
  and a read buffer of 4 KiB, sized to an Agent's messages.
  `tls` reads PEM and installs the ring provider. It reads no file and knows nothing of this
  project.
- **`fleet-core`** — what the Server and the Client implement identically beyond the protocol: the
  version taken apart (`version`), the platform aliases (`platform`), the statements a package
  signature and a floor cover
  ([PKG-versions-high-water-mark-floor](adr/PKG-versions-high-water-mark-floor.md)), the
  renewal proof (`renewal`) and the bootstrap proof (`bootstrap_proof`) a CSR carries — read from
  a CSR by the adapter `csr` — the host a certificate that names none is taken for, when a
  certificate counts as ending, with its end read and put in the line `pki status` prints by the
  adapter `ending::read`, for both programs and the running Server, what a certificate is called
  (`naming`),
  the token buckets the Server and a Gateway count a sender in (`rate`), where the Server serves
  the Gateways' revocation list and the shape of its body, which the Server writes and a Gateway
  reads (`revocation_list`),
  what a client certificate is known by on the Server and on a Gateway alike — its host, its issuer
  and serial, and the 20-octet bound on its serial — whose rule is the core `x509`, read with
  `x509-parser` by the adapter `x509::read`, with the random serial and the clock skew every issued
  certificate gets, the shape of a leaf by its role and the loading of a CA from its certificate
  and key file, by the Server's two CAs and the operator tool alike, in the adapter `x509::issue`;
  the first certificate of a CA file is read by `opamp::tls::first_certificate` — and the
  bootstrap certificate's PKCS#12 file, whose rule is core and whose reading and writing with
  `p12-keystore` is the adapter `pkcs12::file` (SEC-bootstrap-certificates-and-enrolment) — and,
  behind the feature `pkcs11` that only the Server and the operator tool turn on, what both do alike
  on a PKCS#11 token with `cryptoki` in the adapter `pkcs11`: the module's one context per process,
  the slot of a token by its label, the login, the curve P-256, a key's point and the DER of a
  token's signature
  ([SEC-admission-by-client-certificate](adr/SEC-admission-by-client-certificate.md)).
- **`fleet-util`** — what the Client and the operator tool share and the Server does not use
  ([DEV-six-crates-tokio-axum](adr/DEV-six-crates-tokio-axum.md)):
  the unpackers of a package's artifact and the 7z Unix-mode convention (`archive`), the links a
  kind's tree package keeps, which `opamp-fleetctl` packs and the Client creates (`tree`), though
  no kind on `main` states one yet, the file operations that follow no link (`nofollow`), the
  reader of a password file (`password`), and a bootstrap identity as PEM (`identity`), opened
  from a PKCS#12 file by the adapter `identity::pkcs12`. The Unix `*at` system calls of
  `nofollow` are wrapped once in `nofollow::at`, which the Client's work by directory handle
  calls too. The calls only the Client makes stay in its `storage::handle`, and a test decides
  that each of the Client's `*at` calls is in one of the two. A test decides from who names a
  module whether it belongs here or in `fleet-core`.
- **`fleet-server`** — the Server: the fleet and its Hosts, its Configurations, labels and the
  effective description Selectors match (`labels`), packages and Deployments, the Agent plane, and
  the Operator plane, whose credential check is `credentials`.
- **`fleet-agent`** — the Client in all its modes, the program `supervisor`, its command-line
  surface in `cli`.
- **`fleet-tools`** — the operator tool, the one program `opamp-fleetctl`: its `package` commands
  fetch (`fetch`), pack, hash and sign package artifacts — or sign a Package's signing file from
  the UI (`signing`,
  PKG-signed-packages-and-delivery)
  — its `pki` commands make the fleet's certificates and, through
  `token`, the CA keys and the audit key on a PKCS#11 token, and `pki p12-extract` takes a pair out
  of a PKCS#12 file (OPS-operator-program-fleetctl). `gpgv` verifies an upstream's OpenPGP signature
  (OPS-operator-program-fleetctl §29); no command calls it yet. `fetch` reads every download
  within a cap, and `fetch::upstream` holds the parsers of what upstream sends; the crate has no
  library, so the fuzz targets `checksum_file` and `release_listing` compile that file by its path
  (Q-2). The crate depends on neither end: what it
  shares with the Client — the 7z Unix-mode convention (`archive::unix_mode_attributes`; its tests
  also use the unpacker), the links a tree package keeps (`tree::KeptLink`, `tree::is_kept`), the
  PKCS#12 reader (`identity::pkcs12::open`), and the reads of an administrator's file
  (`nofollow::read_privileged` with `nofollow::Named`, `password::read_password_file`) — it takes
  from `fleet-util`.

Inside the Server and the Client the code is ports and adapters
([DEV-ports-and-adapters](adr/DEV-ports-and-adapters.md)): a **core** that holds the domain and owns
its **ports**, **adapters** that bind a port to a technology, and a **composition root** — `lib.rs`,
`main.rs`, and on the Client `supervisor`, `service::runtime` and `service::enrol` — that wires the
two. On the Client a fourth role, the **Plugin**, is a Supervisor kind: it names the Plugin port
and nothing of the host
([CLT-wrappers-act-through-the-core](adr/CLT-wrappers-act-through-the-core.md)). Every module's
role is listed in
[`dependency_direction.rs`](../crates/fleet-core/tests/dependency_direction.rs), which fails when a
core module names an adapter or a technology, when a Plugin names more than its port, when a
module has no role, when a role names no module, and when this overview names a module with a role
nowhere and the test does not exempt it.

- **Server stores** — one port per domain in the core module it serves, shaped so a database or
  an object store could implement it, and speaking in domain types, never paths
  ([SRV-state-stores-contract-pair](adr/SRV-state-stores-contract-pair.md)):
  `AgentStore` in `agent_store`, which holds each Agent's labels in its record, writes a rollout's
  revisions and assignments as one operation, answers what an assignment pins, and answers a page
  of the list of Agents (`query_page`) and the values the fleet reports (`reported_values`), with
  the filter, the order and what is derived from each record defined in the core module `listing`
  ([API-lists-paged-by-cursor](adr/API-lists-paged-by-cursor.md));
  `ConfigBackend` under the `ConfigStore` in `configs`; `DeploymentBackend` under the
  `DeploymentStore` in `deployments`; `PackageBackend` and `BlobStore` (`blobs`, artifact bytes by
  SHA-256) under the `PackageStore` in `packages`, the bytes in `data_dir` or, with
  `[blobs] store = "s3"`, in one bucket of an S3-compatible store through the adapter in `s3`, which
  bridges the synchronous port onto a runtime of its own (`bridge`)
  ([SRV-state-stores-contract-pair](adr/SRV-state-stores-contract-pair.md));
  `BootstrapStore` under the
  `BootstrapCertificates` in `bootstrap`; `EnrolmentStore` under the `Enrolment` in `enrolment`;
  `FloorStore` under the `PackageFloors` in `floors`, the floor the operator signed per Agent type
  ([PKG-versions-high-water-mark-floor](adr/PKG-versions-high-water-mark-floor.md));
  `LedgerStore` in `revocation`; `AuditStore` in `audit_log`. The filesystem adapters are in `fs`,
  which also opens the data directory and refuses anything its layout does not name; the Agent
  records' is `fs::agents`. With `[database] store = "postgres"` every port but the blob port has
  its PostgreSQL adapter in
  `postgres` instead (`postgres::agents`, with the query of a page in `postgres::agent_page`,
  `configs`, `deployments`, `packages`, `ledger`, `bootstrap`, `enrolment`, `floors`, `audit`),
  each write one transaction; every read of several statements that must agree — the load of the
  Configurations, the Packages, the Deployments, the enrolment queue and the ledger, a page, the
  Agents' `query`, a checkpoint and `server audit-verify` — reads one read-only snapshot, which
  `postgres::snapshot` alone builds. `postgres` holds the
  connection they share — TLS 1.3 on *ring*, a pool on a `bridge` runtime of its own, each call
  bounded in time by `Database::run` and `run_within`, the exclusive lock of an instance that must
  be alone on the database, the blob lock a sweep, the staging cleanup and `server blobs-to-s3` hold
  alone and an upload holds shared (`hold_blobs`), the leases of `postgres::lease` and the
  notifications of `postgres::notify` — and applies the schema embedded in `postgres::schema` as
  numbered migrations. Every session-level advisory lock is a `postgres::SessionLock`: a session of
  its own, a try and a wait, the unlock on drop, and the watch that says the session and the lock
  are gone, which the exclusive lock, the blob lock, the prune lock and the signer's lock take.
  `postgres::import` is `server migrate-to-postgres`, which copies a `data_dir` into the database
  offline, and `server blobs-to-s3`, which copies the blob files into the bucket
  ([SRV-state-stores-contract-pair](adr/SRV-state-stores-contract-pair.md)).
  `config::read_secret_file` is the one reader of the password, PIN and secret files the PostgreSQL,
  S3 and PKCS#11 adapters name. The core's constructors take the ports — `AppState::with_stores`,
  `PackageStore::with_backend`, `PackageOffering::with_deployments`, `BootstrapCertificates::open`,
  `Enrolment::open_on` — and `lib.rs` chooses the adapters by `[database]` in `Records`, which
  `AppState::on`, `PackageStore::open_on`, `PackageOffering::on` and `BootstrapCertificates::on`
  take, and wires the filesystem adapters in `AppState::new`, `PackageStore::open`,
  `PackageOffering::new` and `BootstrapCertificates::on_disk`. `migrate` is the one module that
  reads the layout of an older Server, for `server migrate`, and nothing in the running Server calls
  it.
- **The golden path** — a Server store: the port `FloorStore` in the core module `floors`, its
  filesystem adapter in `fs::floors`, its PostgreSQL adapter in `postgres::floors`, and the one
  `contract` test there that runs on both stores. A new store copies its shape and its test
  ([DEV-ports-and-adapters](adr/DEV-ports-and-adapters.md),
  [SRV-state-stores-contract-pair](adr/SRV-state-stores-contract-pair.md)).
- **Server instances** — on PostgreSQL every Server is an instance, and several share one
  database and one bucket
  ([SRV-instance-opens-chosen-planes](adr/SRV-instance-opens-chosen-planes.md)). Each opens
  the planes `planes` names, both by default and always both where it must be alone; every
  instance reads and checks the whole configuration
  ([SRV-instance-opens-chosen-planes](adr/SRV-instance-opens-chosen-planes.md)).
  `instance` takes a lease, a row of `postgres::lease`, before any listener it opens, renews it,
  proves it before every answer, and closes the planes it opens while it cannot. The lease records
  those planes, and each take and renewal warns once of a plane no live lease serves. Each lease it
  takes checks the schema under the migrations' lock and records the instance's release and its
  `[audit] key_label`, refused while a live lease names another label, and a migration refuses
  while a live lease is of a release older than the schema it leaves needs, so two consecutive
  versions run side by side. A change that concerns a live session is one `NOTIFY` of
  `postgres::notify`, sent in the transaction that commits it, or right after the commit for a
  binding and a Host row that widen trust and for a Client Agent first arrived over a Gateway's
  Host, which the write of its record finds against the row it replaces; each instance listens on a
  connection of its own, reads its caches again rather than patching them, and counts a missed
  notification, or one of a kind it does not know, as every notification. The kinds are one type of
  the core, `changed::Changed`: `postgres::notify` names the one the wire carries, and
  `instance::follow` maps each to what it reads again (`Due`), both by a `match` that does not
  compile without every kind; `follow` answers
  what it heard meanwhile with one read, tries a failed read again with back-off, and at every
  renewal reads the ledger again where the revocation list's version moved; a call whose answer was
  lost has its notifications heard once more (`postgres::notify::Unsure`). `instance` also runs the
  presence batches, the retry of the restarts kept for it and the retry of the unmarks. `fleet`
  holds only the Agents whose connections the instance carries: the holder of an Agent is its
  row's instance, the newest connection takes it in one transaction on its row, which raises the
  row's count of takes and announces the take where it replaced another holder, and an instance
  writes the presence of what it carries in batches. The contexts of `fleet` are ranked, and each
  names only those below it, while all share the one state
  ([SRV-fleet-contexts-call-by-rank](adr/SRV-fleet-contexts-call-by-rank.md)). From the bottom:
  1. The module root of `fleet` keeps the state and the types of its parts (among them `Kept`, what
     an instance keeps of each Agent beside its record, which a forget drops and a re-key moves as
     one), the state's limits, the lease, the audit helpers, the capabilities the Server declares,
     the staleness rule, and the types the contexts share, with the tests of staleness, of the host
     a register derives, and of label inheritance. It also keeps what `approval` and `session` both
     build a report's answer from: the end a report answers with, and `compose_settings_offer`,
     which composes one connection settings offer message with its hash; `session` builds the offer.
     It names no context but in its own `pub use` lines.
  2. How the Agents of one Client are placed under its Client Agent
     ([SRV-fleet-record-agents-hosts §19](adr/SRV-fleet-record-agents-hosts.md)) is the core module
     `fleet::placement`, behind the same `AppState` entry points: the rule for one group, which the
     fleet in memory and a page `postgres::agent_page` reads share; the index of the groups this
     instance holds members of; whether the Client Agents of a group another instance carries are
     read under the fleet lock or before it (`Elsewhere`); and the placing again after a notice or a
     catch-up. Since each ends by placing the Agents, the construction of the state, the arming of
     the host register and the reads of the fleet from a shared store stand here too.
  3. What one instance holds of the Agents — the take and the release, the presence it writes, the
     re-keys whose answer was lost, the restarts kept for retry, the take-up of what other instances
     announced of the Agents a notice names, the catch-up after a missed notification (`catch_up`:
     one read of the records and one of the holders of every Agent held, from which each kind of
     kept state and every group is taken up), and the per-Agent locks — is the core module
     `fleet::held`, behind the same `AppState` entry points. It also takes labels into a held
     record, notes the end of a forgotten Agent's session, and keeps the persist path that writes a
     record when its durable content changed, which `approval` and `session` call.
  4. Packages in the fleet — the Package Sets, their entries and signatures, the upload limit, the
     offer of a Package to an Agent and of an artifact to a Host, and the Packages a Deployment
     holds ([PKG-signed-packages-and-delivery](adr/PKG-signed-packages-and-delivery.md)) — are the
     core module `fleet::delivery`, behind the same `AppState` entry points.
  5. Configurations, Deployments and rollouts — the catalogue of Configurations, what a Deployment
     releases, the rollout acts that assign it, what waits for an Agent's next press and the types
     the views show of it and of a Deployment's reach, labels, and the restart an operator requests
     ([SRV-deployments-and-pinned-rollouts](adr/SRV-deployments-and-pinned-rollouts.md)) — are the
     core module `fleet::rollout`, behind the same `AppState` entry points.
  6. The views of the fleet — the Agents each Host carries, the view of an Agent and of the whole
     fleet, and the list of Agents a page at a time
     ([API-lists-paged-by-cursor](adr/API-lists-paged-by-cursor.md)) — are the core module
     `fleet::views`, behind the same `AppState` entry points.
  7. Enrolment and certificates — the approval of an enrolment and its take-back, the answer an
     enrolling Agent is given, the issuing and renewal of a host certificate with the records of
     what was issued and signed, the mark of a host as a Gateway, which places every Agent again,
     and the answers a refused report gets, which `session` uses too
     ([SEC-bootstrap-certificates-and-enrolment](adr/SEC-bootstrap-certificates-and-enrolment.md)) —
     are the core module `fleet::approval`, behind the same `AppState` entry points.
  8. The round of an Agent's report — a report processed under the Agent's lock, its record
     created, taken or settled, the reply and the connection settings it is offered, a goodbye and a
     forget — is the core module `fleet::session`, behind the same `AppState` entry points.

  The test helpers the contexts share are the test-only `fleet::fixtures`, which has no rank. Each
  context imports by name what it takes from the root, from its siblings and from the crate;
  `clippy::wildcard_imports`, denied in `fleet`, refuses a glob import in its production code.
- **Server use cases** — each plane reaches the core through one use-case module
  ([SRV-two-use-case-layers](adr/SRV-two-use-case-layers.md)). `transport`, the Agent plane's
  endpoints, carries the protocol and calls `agent_cases::AgentCases`, whose methods are the plane's
  operations: admission, a report, a certificate request, an enrolment, a download, and the
  Gateways' read of the revocation list. `api`, the REST API and the bundled UI, calls
  `operator_cases::OperatorCases`, whose methods are every read and change of the Operator plane,
  with the guard of `[rest.auth]` (`OperatorAuth`) and the record of each act. Neither endpoint
  module names the other plane's modules or a type of the core that holds state; the core's values
  cross unchanged, and the plain answers and the peer's address both endpoints use are `answers`.
  `lib.rs` builds each module over one core, and `main.rs` builds only those of the planes the
  instance opens, so an instance without the Agent plane carries no Agent and writes no presence
  ([SRV-instance-opens-chosen-planes](adr/SRV-instance-opens-chosen-planes.md)). They are adapters
  in the table, since they read certificates and credentials through adapters, and
  [`dependency_direction.rs`](../crates/fleet-core/tests/dependency_direction.rs) also lists what
  each plane's modules name of the other.
- **Server listeners** — `tls` reads the `[tls]`, `[rest.tls]` and `[enrolment]` files into the
  material each plane serves with — the Agent plane requiring a client certificate in the
  handshake, the Operator plane asking for none and serving `[rest.tls]`'s certificate when the
  section is present
  ([SEC-tls13-bounded-input-chosen-planes](adr/SEC-tls13-bounded-input-chosen-planes.md)) — and
  tells a member's certificate from a bootstrap one by its
  issuer. `tls` reads the files of both planes on every instance, whichever it opens. An instance
  binds and serves the planes it opens and no other, and `listen` serves them on one handle with
  one drain and a connection cap each
  ([SEC-tls13-bounded-input-chosen-planes](adr/SEC-tls13-bounded-input-chosen-planes.md)). `pki` is
  `server pki status` and the running Server's warnings before a certificate it depends on ends
  (OPS-operator-program-fleetctl).
- **Server admission** — `agent_cases::Admission` requires the client certificate on every request
  to `/v1/opamp` and reads no `Authorization` header, admits a bootstrap certificate only while the
  enrolment window is open, and guards the package download with the same certificate. Behind
  the same admission it serves a Gateway its revocation list on `/v1/gateway/revocations` and
  every member the floors on `/v1/package-floors`
  ([PKG-versions-high-water-mark-floor](adr/PKG-versions-high-water-mark-floor.md)). Two core
  modules hold the state behind it: `enrolment`, the operator-opened window, the queue of requests
  an operator approves or rejects through `operator_cases`, and the rejected keys, refused beyond
  the window, and `throttle`, the per-address back-off after repeated failures, which both planes
  use ([SEC-admission-by-client-certificate](adr/SEC-admission-by-client-certificate.md)).
  `revocation` holds the register of issued certificates, the revocation list and the Hosts —
  each with the subject values of its certificate, its Gateway mark and the Agents bound to it —
  behind the `LedgerStore` port
  ([SEC-admission-by-client-certificate](adr/SEC-admission-by-client-certificate.md),
  [SRV-fleet-record-agents-hosts](adr/SRV-fleet-record-agents-hosts.md)).
  `agent_rate` holds the token buckets that bound how often an admitted host, an Agent behind a
  marked Gateway, and the Gateway as a whole are heard, in the bounded tables of
  `fleet_core::rate`; `agent_cases` takes a token for each
  message before anything else is done with it and for each download in its guard
  ([SEC-tls13-bounded-input-chosen-planes](adr/SEC-tls13-bounded-input-chosen-planes.md)).
- **Audit record** — `audit` is the port every security decision is recorded through;
  `audit_log` chains the entries by hash and `fs::FsAuditStore` keeps them under `data_dir/audit/`
  ([SEC-append-only-audit-record](adr/SEC-append-only-audit-record.md)).
  An act a restore
  repeats by its entry — a revocation, a Gateway mark, a floor set, a rejection, the window
  closed — holds a place in the writer's channel first, an `audit::Permit`, and writes the entry
  into it once it has run. On PostgreSQL
  `postgres::audit::PgAuditStore` keeps one chain per lease; the lease's take opens it and closes
  the chains of the lapsed leases it removes, as each renewal does, and an orderly shutdown closes
  the instance's own
  ([SEC-append-only-audit-record](adr/SEC-append-only-audit-record.md)).
  Each entry names the lease its decision was taken under (`taken_under`), and on PostgreSQL the
  writer keeps a batch whose write failed and tries it again first
  ([SEC-append-only-audit-record](adr/SEC-append-only-audit-record.md)).
  `postgres::audit_prune` bounds it by `[audit] retain_days`: the task `main` starts with its
  `prune_daily` prunes at startup and once a day, the instance's own chain always and the closed
  chains while it holds the prune's advisory lock, recording each `audit.pruned` through the
  instance's writer and waiting for it before it deletes, in rounds of a bounded number of entries,
  every chain of one round in one transaction
  ([SRV-state-stores-contract-pair §12](adr/SRV-state-stores-contract-pair.md)).
  With `[audit.export]`, `audit_export` sends every line a store has accepted, the writer's and
  those a lease's transaction writes, as an OTLP log record to the receiver it names: a queue of
  its own, a thread that sends through `opentelemetry-otlp` over the TLS 1.3 *ring* client and
  tries again until the receiver takes a batch, and the count of what the full queue dropped
  written back as `audit.export_dropped`
  ([SEC-append-only-audit-record §15](adr/SEC-append-only-audit-record.md)).
  With `[audit] key_label`, the writer signs a checkpoint of every chain's head at its start,
  each minute and at the shutdown's close. `checkpoint` owns the signer port `CheckpointSigner`,
  which the token key of `pkcs11` implements, and the port `CheckpointSource`, which answers the
  newest checkpoint and the heads to name: `checkpoint::OneChain` on the filesystem store, and
  `postgres::checkpoint::Checkpoints` on PostgreSQL, which elects the signer by a session-level
  advisory lock on a connection of its own. A thread `audit-sign` reads the heads and asks the
  token, while the writer places the checkpoint and waits up to 2 seconds for its signature.
  `AuditStore::append_checkpoint` writes it alone; on PostgreSQL it is written under a unique
  index and the compare-and-set of the head row of migration 17. `checkpoint::Check` is what
  `server audit-verify --audit-key` runs over either store
  ([SEC-append-only-audit-record](adr/SEC-append-only-audit-record.md)).
- **Server ports beyond storage** — `fleet` owns `CertificateSigner`, which the local CA in `ca`
  implements for the CSR flow and the renewal proof (SEC-admission-by-client-certificate), and the
  `UseCase` each signature names. A CA key in `ca` is a key file or a key on the PKCS#11 token of
  `pkcs11`, which signs through `rcgen`'s `SigningKey` from a pool of four sessions, one `cryptoki`
  context per module in the process, and holds a place in the audit record before it signs
  (SEC-admission-by-client-certificate);
  `opamp-fleetctl`'s `token` makes the keys on it. `fleet` also owns
  `Clock`, which `clock::SystemClock` implements. It owns `Elapsed` too, the time that has passed
  as a lease and a sweep measure it: `PackageStore::with_backend` takes it, and `instance`'s
  `Lease::take_as`. `clock::Elapsed::monotonic` is the adapter on `std::time::Instant`, and
  `main.rs` hands both of them `CLOCK_BOOTTIME`, which counts a suspend of the host, on Linux
  ([SRV-instance-opens-chosen-planes §3](adr/SRV-instance-opens-chosen-planes.md)).
  The `[connection_offer]` and `[telemetry_offer]` sections become the fleet's offers in
  `config`.
- **Server configuration** — `config` builds one TOML document from the defaults, `server.toml`
  when one is read, and the `FLEET_SERVER__…` variables over it, and deserializes it once, so the
  environment is checked as strictly as the file; `config::server_variables` reads the
  `FLEET_SERVER__…` variables of the process, and `main.rs` hands them in
  ([SRV-configuration-toml-and-environment](adr/SRV-configuration-toml-and-environment.md)).
- **REST views** — what the REST API reads and returns is shaped in `api`, which derives its
  OpenAPI schema ([SRV-configurations-compose-one-map](adr/SRV-configurations-compose-one-map.md));
  the core's types — among them the fleet view `AgentView` — carry no schema, and `api` converts
  them. `api::paging` reads `limit` and `cursor`, encodes the opaque cursor, and writes
  `X-Total-Count` and the `Link` of the four lists that page.
- **Client supervision** — `supervisor::port` is the Plugin port, plain data and asynchronous
  calls: a Plugin states its manifest and makes an instance per Supervisor, an event handler that
  acts only through the host functions it is handed
  ([CLT-wrappers-act-through-the-core](adr/CLT-wrappers-act-through-the-core.md)). The Plugins
  are the two Supervisors this project brings, the Collector's in `supervisor::collector` and the
  example Custom Supervisor in `supervisor::command`, and the wrapped kinds the build's features
  carry, Telegraf's in `supervisor::telegraf`. `supervisor::host` is the core's side of
  that port, an adapter: it feeds an instance its inputs one at a time, answers what the instance
  leaves unanswered, and carries out each host function over the process runner, the files in
  the Supervisor's own roots through `supervisor::host::files` and the timers through
  `supervisor::host::timers`. `supervisor::ports` holds the commands and events between the core
  and a Supervisor's adapter task and the registry's port, and `shutdown` the handle every task
  stops on. How a `[[supervisor]]` block is
  read — its plugin, program, Agent type and timings — is `supervisor::block`, and what a
  Server-delivered block may bring is `supervisor::delivered`; the registry of Plugins stays in
  `supervisor`, which knows the kinds and hands them to both, and which hands `delivered` the
  Supervisors' root as an absolute path too. When a failed process is started again or held is
  `supervisor::restart`'s decision; the runner reads the clock and starts it. Who starts the
  program is the runner's `Spawner`: the Client itself, or the second service of
  [CLT-accounts-service-runs-under](adr/CLT-accounts-service-runs-under.md),
  `supervisor run --agents`, through `supervisor::channel`, which holds the private protocol, the
  service and the Client's end of its socket — on Windows of its named pipe, in
  `supervisor::channel::windows`, which runs each program in a job object. The Windows system
  calls neither the standard library nor a dependency offers — the SIDs, the pipe's security
  descriptor and its two ends' identities, the job objects — are declared in `win32`. The
  registry starts every Supervisor's host on the one `Spawner`
  of the run, which `service::runtime` chooses once from the Client's command line — the unit
  or plist `service install --process-account` writes names the second service's socket there —
  and hands `build_engine`. The Engine keeps that registry behind the port `PluginRegistry` of
  `supervisor::ports`, so a Supervisor a Supervisor-set apply starts later starts its program
  the same way, and a Supervisor it purges has the same `Spawner` remove first what its
  programs wrote under their own account, in the directory the purge first moved aside to a name
  no Supervisor carries. What the Client changes where the process account writes, it reaches
  through `storage::handle`, on top of `fleet_util::nofollow::at`, one directory handle at a time
  from a root of its own — the state directory, the Supervisors' root, a Supervisor's directory —
  beneath which no link is followed. On Windows that is `win32::files`, which sets a DACL on one
  handle at a time and walks a tree itself, never into a link, with the ACE inheritance rules in
  `win32::acl`; the install's hand-over and its take-over of the data root walk the same way.
  `supervisor::agent`, an Agent's decisions, names four more ports there: `AgentStorage`, which
  `storage::Storage` implements on the state directory, `HighWaterMarks`, which
  `storage::HighWaterFile` implements on `state_dir` and every Supervisor of the Client shares
  ([PKG-versions-high-water-mark-floor](adr/PKG-versions-high-water-mark-floor.md)),
  `PackageFloors`, which `floors::FloorFile` implements on `<state_dir>/package-floors.json` and
  fetches from the endpoint, every Agent of the Client holding to it, the self-update included
  ([PKG-versions-high-water-mark-floor](adr/PKG-versions-high-water-mark-floor.md)),
  and `HostFacts`, which `host::SystemHost` implements from the platform. It answers each Server
  message in `handle` and leaves three
  channels and its description each to a submodule: `packages`, `remote_config`,
  `connection_settings`, and `description`, what the Agent reports about itself.
  `storage`, `csr`, `connection`, `tls`, `config_init`'s writers of `supervisor.toml` and its CA
  copy, the staging, the state directory's creation and the `current` switch in `service::layout`,
  and the re-owning in `service::run_as` reach the data root, the state directory and the layout
  root through the adapter `nofollow`: a directory held open without following a link at its name,
  read from and written to by name, each file written whole and renamed into place, because a
  privileged install works in directories the service's account owns
  ([CLT-accounts-service-runs-under](adr/CLT-accounts-service-runs-under.md)). `nofollow` reads
  every file one way: a regular file within a bound the caller names, into a buffer sized before it
  reads and wiped when dropped ([SEC-secrets-wiped-on-drop](adr/SEC-secrets-wiped-on-drop.md)). A
  path a privileged step did not choose itself is resolved by `nofollow::read_privileged`, described
  below. The test
  `privileged_steps_follow_no_link` holds `main.rs`, `config_init`, `config_file`, `service/`,
  `csr`, `connection` and `tls` to that, with the path-based calls that change or read a file left
  to them listed and justified, and each call of such a function listed by where it is made and
  why; a by-path `metadata` or `exists` check is not among them.
  `supervisor.toml` is read by a privileged step through `ClientConfig::load_held` and rewritten,
  by an install and by a Server-driven apply alike, through `config_file::replace`, in the held
  directory, which on Windows refuses a configuration directory that is a reparse point for the
  apply too; the run-time Client reads it by path with `load`. The privileged enrolment
  (`service::enrol`) reads the CA file and the bootstrap pair once, before it connects, into
  `ClientConfig::ca_pem` and `bootstrap_identity`: a regular file of at most 1 MiB, opened by its
  name in a held directory. `nofollow::read_privileged` resolves the path one component at a time,
  and a root or the state directory it passes must hold the file directly; elsewhere a link on the
  administrator's path is followed. The questionnaire's bootstrap PKCS#12 file and
  `--bootstrap-password-file` are read the same way. A path the questionnaire offers from
  `supervisor.toml` (`[tls] ca_file`, `cert_file`, `key_file`, which the service's account may
  write) and the answer keeps is refused where a link stands at its name, wherever it lies
  (`config_init::Offered`); so is every such path of a file a run that answered nothing kept
  (`service::enrol::prepare`). A run that answered the questionnaire or
  `--endpoint` hands the enrolment the endpoint and the CA certificates it copied
  (`config_init::Answered`), which stand in for what the file says by then; the answered CA file
  itself is read as the enrolment reads one, and only its certificates are kept. `--endpoint` over a
  kept file refuses the enrolment where the file names another endpoint. What else runs under the
  Client's or the process account — the self-update's and a Supervisor's program staging through
  `install`, the Gateway's package cache, and the second service's purge on Windows, which goes
  by path — works by path in directories that account owns and no other account may write.
- **Client configuration** — `config` is what `supervisor.toml` may say and the rules it must meet
  ([DEV-six-crates-tokio-axum](adr/DEV-six-crates-tokio-axum.md));
  `config_file` reads it from disk, makes
  its
  directories absolute, finds the identity the state directory holds, and redacts the text the
  Client reports as its effective configuration.
- **Client connection** — `tls` decides which CA and which identity are in force, and `transport`
  describes the upstream `Connection` from `supervisor.toml` and runs the Engine over it. The
  settings verification in `connection`, the Gateway's upstream pool (`gateway::pool`) and its
  downstream listener, and the Supervisor Endpoint (`supervisor::endpoint`) use the same `opamp`
  building blocks
  ([DEV-six-crates-tokio-axum](adr/DEV-six-crates-tokio-axum.md)).
  The Gateway's
  `revocations` keeps the list it fetches from its Server and refuses what that list names
  ([CLT-modes-and-gateway](adr/CLT-modes-and-gateway.md)),
  and its `floors` fetches the floors and hands the hosts behind it the ones it last fetched,
  behind the same verdict
  ([PKG-versions-high-water-mark-floor](adr/PKG-versions-high-water-mark-floor.md)).
  The Gateway's `cache` sees every offer `registry` hands down, fetches each artifact offered on
  the Server's route once into the state directory, and serves it on the downstream download route
  to the hosts it was offered to
  ([PKG-signed-packages-and-delivery](adr/PKG-signed-packages-and-delivery.md)).
  Which host the Gateway takes to speak for an `instance_uid` is decided in the core module
  `gateway::policy`; the cache holds the bindings and keeps them across restarts in the journal
  `gateway::store` reads and writes beside the artifacts; while that journal cannot be read the
  Gateway carries no downstream Agent
  ([CLT-modes-and-gateway](adr/CLT-modes-and-gateway.md)).
  With a bootstrap CA the Gateway tells an
  enrolling peer from a member and relays its one plain-HTTP enrolment request without a binding
  ([CLT-modes-and-gateway](adr/CLT-modes-and-gateway.md)).
  `gateway::rate` holds a token bucket per downstream host, or per enrolling peer's address, in a
  table of `fleet_core::rate`; the downstream endpoint and the download route take a token before
  they do anything else
  ([CLT-gateway-rate-limits-hosts](adr/CLT-gateway-rate-limits-hosts.md)).
- **Client engine** — `engine` routes the Server's replies to the Agents over one connection
  ([CLT-modes-and-gateway](adr/CLT-modes-and-gateway.md)). The transports, the Gateway, telemetry
  (`telemetry`) and the service runtime are adapters around it. `span` records an operation's
  outcome on the span that measures it, which `telemetry` turns into the span status. `product`
  holds the product's name and title, fixed at build time
  ([CLT-installed-service-and-state](adr/CLT-installed-service-and-state.md)).
- **Client service** — `service` is what makes the Client an OS service, but for its log on disk,
  which `logging` writes while it runs as a service. On Windows `service::windows` is the SCM
  runtime shim: it reports the service running and runs the same daemon body as the foreground.
  `service::manager` registers, starts, stops and deregisters the Client's
  service and, where a process account is named, the second service `<PRODUCT_NAME>-agents` beside
  it, and writes both systemd units, or both launchd plists, itself
  ([CLT-accounts-service-runs-under](adr/CLT-accounts-service-runs-under.md)). `service::run_as`
  decides and checks the Client's account and the process account, and hands the roots over,
  leaving what the process account owns. On Windows `service::windows_config` sets the SCM's
  accounts, recovery actions and required privileges, `service::windows_rights` restricts the data
  root, `service::run_as` also grants the process account its share of the layout and withdraws a
  former account's grants, and `service::withdrawal` keeps the record
  of what an install left to finish — the former accounts to withdraw and the services to start
  again — under `HKLM\SOFTWARE\<PRODUCT_NAME>\WithdrawalPending`; the install lock that serialises
  `service install`, `uninstall`, `start` and `stop` is in `win32`.
- **Client self-update** — `update` is the Client updating itself
  ([CLT-self-update-signed-package](adr/CLT-self-update-signed-package.md)). It owns the port
  `SelfUpdater` and
  `SelfUpdate`, the state the Engine keeps about it: armed or not, probation committed, restart
  due. `update::installer` implements the port on the version directories and holds the start-up
  check of the process that follows an install.

## How it runs

The few paths worth following end to end, such as a request, a job, or a build, and where state
lives between them. Only what a newcomer would otherwise reconstruct from code.

**A Configuration reaches an Agent.** An operator saves a Configuration through `api`, puts it
into a Deployment in `deployments`, and rolls the Deployment out; `fleet` replaces the assignment of
every Agent the Deployment's Selector claims with one release — the Package for its type and every
Configuration that fits its type, pinned by revision hash in `configs` — composes each Agent's
configuration and pushes the offer over the Agent's WebSocket, or answers the next poll with it.
On the Client, `engine` routes the offer to the Agent's Supervisor, which writes the entries into
the Supervisor's `config/` directory and hands its Plugin each entry's name, role and digest; the
Plugin answers with how its program runs on them, and `supervisor::host` restarts the process on
that, or signals it where the kind's manifest names a reload signal; the Agent reports `APPLIED` or
`FAILED` with the reason, and the Server stops offering once the reported hash matches
([SRV-configurations-compose-one-map](adr/SRV-configurations-compose-one-map.md),
[SRV-deployments-and-pinned-rollouts](adr/SRV-deployments-and-pinned-rollouts.md)). A
Configuration typed for the Client
itself carries `[[supervisor]]` blocks, which `reconfigure` checks and writes into
`supervisor.toml` before it starts or stops anything
([CLT-wrappers-act-through-the-core](adr/CLT-wrappers-act-through-the-core.md)).

**An Agent is placed under its Host.** Each Agent record names the Host it last arrived over — its
own, or the Gateway's behind one. `fleet` derives from it, per Agent, what its effective description
adds to its report: the `fleet.host.*` values of the Host it arrived over directly, and the labels
of its Client Agent — the Agent reporting `supervisor.kind.*` in the same group, which is the Host,
or behind a Gateway the machine the Agents report. It keeps that placement per group and places a
group again when a report, a label, a forget or a Gateway mark changes it, and on PostgreSQL when
another instance announces a binding of its Host, a Client Agent arriving behind its Gateway, or
the labels or the forgetting of a Client Agent that instance carries, and every group after a
missed notification, so every Selector match
reads the same description without a fleet-wide pass
([SRV-fleet-record-agents-hosts](adr/SRV-fleet-record-agents-hosts.md)).

**A package reaches an Agent.** An operator uploads an artifact into `packages` and signs its
entry there — `packages` checks the signature against `[packages] verification_key` when the
Server holds one, and only reports — then puts the Package into a Deployment with a Selector and
releases it, together with the Configurations the Deployment holds. The Agent is offered the package
with its entry's signature; the Client downloads it
from its own Server's origin and nowhere else, and `transport` serves it only when `fleet` finds it
offered to an Agent the certificate's host speaks for, by the test `packages` shares with the
offer
([PKG-signed-packages-and-delivery](adr/PKG-signed-packages-and-delivery.md));
behind a Gateway the
Client downloads it from the Gateway's cache, which `gateway` fetched once from the Server. The
Client checks hash and signature in `packages`, and the
Supervisor swaps the program, keeps the previous one for its grace period and rolls back if the
new one does not stay up. Before anything is downloaded, an offer below the floor the operator
signed for its Agent type is refused: the Client fetches the floors from its endpoint as they fall
due and keeps the highest that verifies
([PKG-versions-high-water-mark-floor](adr/PKG-versions-high-water-mark-floor.md)). The Client's own
package goes through `update` instead: a version
directory beside the running one, a self-check, the `current` pointer moved, and a restart on
probation that commits or rolls back.

**A host joins and stays.** The Server issues bootstrap certificates on an operator's request:
`bootstrap` checks the subject and the life, records what was issued, and asks its
`BootstrapSigner` port — the bootstrap CA with its key in `ca` — to make the key, sign, and
seal both into the PKCS#12 file it keeps, the key's only way out
([SEC-bootstrap-certificates-and-enrolment](adr/SEC-bootstrap-certificates-and-enrolment.md)).
A fresh host presents a bootstrap certificate while an operator has
the enrolment window open — to the Server, or to a Gateway that relays its request — and proves it
in its CSR ([CLT-modes-and-gateway](adr/CLT-modes-and-gateway.md));
the request is filed under the fingerprint of the key it asks a certificate for and waits until
an operator approves or rejects it — a rejected key stays refused beyond the window, and may be
rejected before its request arrives — and the certificate it
gets names a new host and carries the subject values `tls` read from the bootstrap certificate
and `ca` checked again, never the CSR's; every renewal carries the predecessor's
([SEC-bootstrap-certificates-and-enrolment](adr/SEC-bootstrap-certificates-and-enrolment.md)).
`service install` — asked through its questionnaire, or unasked with the bootstrap identity found
beside the program or in `--fleet-files` — and `service config` do this before the service is
registered: `service::enrol` runs the Client's own Agent over the ordinary `transport` with the
bootstrap pair, or the identity a `bootstrap.p12` opened to, until the issued certificate is in
the state directory
([SEC-bootstrap-certificates-and-enrolment](adr/SEC-bootstrap-certificates-and-enrolment.md)).
From then on the Client renews at two thirds of the certificate's life,
proving with the old key which certificate it renews. A revocation closes the sessions it
concerns at once, on every instance by its notification, and follows every renewal.

**An instance is lost.** Killed, its connections are cut; frozen, its Agents end their
WebSockets after one heartbeat interval of silence. Each Agent reconnects after a delay drawn over
`[0, 30 s)`, through the balancer, to an instance that opens the Agent plane and still holds its
lease, which reads the Agent's record at its first report and takes it as its holder. Once the lost
instance's lease has lapsed, the instance that removes it closes its audit chain. The others serve
from what the lost instance committed to the database, the Operator plane included, which
instances of its own may serve
([SRV-instance-opens-chosen-planes](adr/SRV-instance-opens-chosen-planes.md)).

**A Supervisor starts its program.** Without a process account the runner starts the Managed
Process, its version probe and a package's preflight as the Client, and `supervisor::host` every
program a Plugin runs once through the same `Spawner`. Where `service install
--process-account` registered one, the Client's command line carries `--agents-socket` and the
process account's uid (on Windows its SID), and `service::runtime` hands every Supervisor the
`Spawner::Channel` of `supervisor::process`, over the `Channel` of `supervisor::channel`, instead:
each start, signal, stop and purge is a request to the second service, `supervisor run --agents`,
the same program from the same `current` under the process account, which streams the program's
output and exit back. The second service admits one connection at a time, from the Client's account
alone by the peer's credentials or the pipe client's token, and stops every program it started — by
process group on Unix, by job object on Windows — when that connection closes. Before it writes
anything, the Client accepts the connection only from the process the service manager runs as the
second service, which `service::manager::agents_process` asks of systemd, launchd or the SCM
([CLT-accounts-service-runs-under §19](adr/CLT-accounts-service-runs-under.md)).
Both ends refuse a peer of another build, and the one that `current` no longer names exits so its
service manager starts it from the new version. While the second service is away, a delivered
package is staged and
waits, and the Supervisor retries with backoff
([CLT-accounts-service-runs-under](adr/CLT-accounts-service-runs-under.md)).

**Where state lives.** The Server keeps all of its state under one `data_dir`, one directory per
domain and owner-only: `configurations/`, `deployments/`, `packages/` (metadata) and `blobs/` (the
artifacts, named by SHA-256, unless a bucket keeps them), `agents/` (each Agent's record, its labels
inside), `hosts/`,
`certificates/` (the register), `revocations.json`, `bootstrap/`, `enrolment.json` (the window and
its queue), `package-floors.json` (the floor of each Agent type) and `audit/`
([SRV-state-stores-contract-pair](adr/SRV-state-stores-contract-pair.md)).
With `[database] store = "postgres"` the records are rows of one PostgreSQL database instead, and
`data_dir` keeps, with the filesystem blob store, `blobs/` and an archived `audit/`; with the blobs
in S3 too there is no `data_dir`, and an instance keeps nothing on its disk
([SRV-state-stores-contract-pair](adr/SRV-state-stores-contract-pair.md)).
On files the Server loads every record at startup and answers from memory; on PostgreSQL an
instance holds only the Agents connected to it, each read at its first report, and the Operator
plane reads the fleet from the database.
A restart on files restores the fleet from them and shows each Agent disconnected until it
reports; an
installation an older Server wrote is moved there once by `server migrate`. The Client keeps its
state
under `state_dir`: the issued certificate and key, the connection settings the Server offered,
and per Supervisor a directory with `config/` and `program/`, and, once its kind or its Plugin
first needs them, `data/`, which the Managed Process writes into, and `state/`, the Plugin's own
and the Client's alone
([CLT-wrappers-act-through-the-core](adr/CLT-wrappers-act-through-the-core.md)). An installed
Client runs from
`versions/<version>/` through the `current` pointer
([CLT-accounts-service-runs-under](adr/CLT-accounts-service-runs-under.md)).
