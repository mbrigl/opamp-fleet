# Architecture — OpAMP Fleet

> The system **as it stands today**: the parts it is built from, what each is responsible for, and
> how they fit together. It holds no rule and no decision. It names the ADR behind each structural
> choice; where it disagrees with an accepted ADR, the ADR is right and this document is stale. It
> is updated in the same change as the structure it describes
> ([ADR-0001](adr/0001-agent-governance-model.md)).
>
> **Kept current by:** Markus Brigl. A document everyone may edit and nobody owns is
> the one that goes stale.
>
> **Last design revision:** 2026-11-12, due after 20 changes. The revision that took the count
> moves the date; the number is this project's to set; a sensor counts the changes outside
> `docs/` since the commit that set the date and says when the next is due
> ([ADR-0006](adr/0006-feature-layer.md)).

## Context

What sits outside the system and what crosses its boundary. A diagram earns its place here more
than anywhere else, and none beats one that has stopped being true.

```text
 operator ── REST / bundled UI ──▶ Operator plane :4321 ─┐
                                                         │  Server (Linux): one, or several
 Client ──── OpAMP, mTLS ──────────────▶ Agent plane :4320 ─┘   │  instances behind a balancer
   │  ▲                                                          └─▶ data_dir, or PostgreSQL with
   │  └── Gateway (a Client) ◀── OpAMP, mTLS ── other Clients        the blobs as files or in an S3
   │                                                                 bucket, which instances share:
   │                                                                 fleet state, register, audit
   │                                                                 record, artifacts by SHA-256
   ├── Supervisor Endpoint (loopback) ◀── a Collector's opampextension
   ├── Managed Processes: Collector, any command — where a process account is registered, under
   │     it, started by the second service over a Unix socket or named pipe
   ├── package downloads: the Server's own origin, and no other
   ├── own-telemetry destinations: OTLP/HTTP, as the Server offers them
   └── service manager: systemd, launchd or the Windows SCM
```

- **Operators** drive the fleet through the REST API on the Operator plane, guarded by
  `[rest.auth]` beyond the loopback; the bundled UI uses the same API
  ([ADR-0040](adr/0040-admission-by-client-certificate-and-revocation-with-the-ca-keys-in-a-file-or-on-a-token.md)).
- **Clients** reach the Agent plane over OpAMP — WebSocket or plain HTTP — admitted by the client
  certificate in the TLS 1.3 handshake alone
  ([ADR-0012](adr/0012-tls-1-3-two-planes-and-bounded-input.md),
  ADR-0040). A Client in Gateway Mode carries other Clients' Agents over its own upstream
  connections and refuses what the Server revoked
  ([ADR-0016](adr/0016-client-modes-and-the-gateway.md)).
- **Managed Processes** run on the Client's host under a Supervisor each; a Collector reports
  through its own `opampextension` to the Supervisor Endpoint, which admits only the process its
  Supervisor started ([ADR-0016](adr/0016-client-modes-and-the-gateway.md)).
- **Package downloads** come from the Server alone: every artifact is uploaded to it, and a
  Client fetches only from its own Server's origin, or from its Gateway. What arrives is checked
  against its hash and the operator's signature
  ([ADR-0034](adr/0034-packages-signatures-and-delivery.md)).
- **The service manager** starts the Client and restarts it after a self-update
  ([ADR-0019](adr/0019-the-client-updates-itself-from-a-signed-package.md)). Where a process
  account is registered, it also runs the second service, which starts the Managed Processes
  under that account (ADR-0021).
- **Upstream**, the opamp-spec release named as the Protocol Baseline in
  [`CONFORMANCE.md`](CONFORMANCE.md) fixes the wire format.

## Building blocks

The parts the system is made of, each with one responsibility, named in the vocabulary of
[`GLOSSARY.md`](GLOSSARY.md). One level deep, deeper only where the size of a part earns it. The
golden path and the test pattern an agent copies from are named here.

Five crates in one workspace ([ADR-0008](adr/0008-five-crates-on-tokio-and-axum.md)):

- **`opamp`** — the OpAMP communication layer, publishable on its own
  ([ADR-0008](adr/0008-five-crates-on-tokio-and-axum.md)). Always the
  generated types, framing and the endpoint's body rules. Behind `client`, an Agent's protocol state
  machine, the two transports, and `client::connection`, which builds a connection, its TLS and its
  HTTP client from a `Connection` the application fills in, and every other HTTPS client of the
  Client through `ClientTls::https_client`: TLS 1.3, no redirect followed, bounded by its
  `Timeouts`. The WebSocket driver ends a connection on which nothing arrived for one heartbeat
  interval (`client::ws::Silence`)
  ([ADR-0015](adr/0015-a-websocket-on-which-nothing-arrives-for-one-heartbeat-interval-is-ended.md)),
  and both drivers draw their reconnects and their waits after a refusal or an `Unavailable` from
  `client::backoff`. `uid` is the `InstanceUid` and `attributes` the attribute keys both sides use.
  Behind `server`, the endpoint around a `Handler`, which answers a WebSocket's pings while it
  handles a message, `server::listen`, the listener with its TLS and its bounds on connection
  setup, and `server::pace`, the floor every body and WebSocket message it serves is held to. The
  server side takes a WebSocket over through hyper's upgrade and reads its frames with
  `tokio-tungstenite`, the library the client side uses too.
  `tls` reads PEM and installs the ring provider. It reads no file and knows nothing of this
  project.
- **`fleet-core`** — what the Server and the Client implement identically beyond the protocol: the
  version, the platform aliases, the statements a package signature and a floor cover
  ([ADR-0035](adr/0035-package-versions-high-water-mark-and-floor.md)), the
  renewal proof (`renewal`) and the bootstrap proof (`bootstrap_proof`) a CSR carries — read from
  a CSR by the adapter `csr` — the host a
  certificate that names none is taken for, when a certificate counts as ending, what a
  certificate is called, the token buckets the Server and a Gateway count a sender in (`rate`),
  what a client certificate is known by on the Server and on a Gateway alike — its host, its issuer
  and serial, and the 20-octet bound on its serial — whose rule is the core `x509`, read with
  `x509-parser` by the adapter `x509::read`, with the random serial and the clock skew every issued
  certificate gets, the shape of a leaf by its role and the loading of a CA from its certificate
  and key file, by the Server's two CAs and the operator tool alike, in the adapter `x509::issue`;
  the first certificate of a CA file is read by `opamp::tls::first_certificate` — and the
  bootstrap certificate's PKCS#12 file, whose rule is core and whose reading and writing with
  `p12-keystore` is the adapter `pkcs12::file` (ADR-0027).
- **`fleet-server`** — the Server: the fleet and its Hosts, its Configurations, labels and the
  effective description Selectors match (`labels`), packages and Deployments, the Agent plane, and
  the Operator plane, whose credential check is `credentials`.
- **`fleet-agent`** — the Client in all its modes, the program `supervisor`, its command-line
  surface in `cli`.
- **`fleet-tools`** — the operator tool, the one program `opamp-fleetctl`: its `package` commands
  fetch (`fetch`), pack, hash and sign package artifacts — or sign a Package's signing file from
  the UI (`signing`, ADR-0034) — its `pki` commands make the fleet's certificates and, through
  `token`, the CA keys and the audit key on a PKCS#11 token, and `pki p12-extract` takes a pair out
  of a PKCS#12 file (ADR-0042).

Inside the Server and the Client the code is ports and adapters
([ADR-0009](adr/0009-ports-and-adapters.md)): a **core** that holds the domain and owns its
**ports**, **adapters** that bind a port to a technology, and a **composition root** — `lib.rs`,
`main.rs`, and on the Client `supervisor`, `service::runtime` and `service::enrol` — that wires the
two. Every
module's role is listed in
[`dependency_direction.rs`](../crates/fleet-core/tests/dependency_direction.rs), which fails when a
core module names an adapter or a technology, when a module has no role, and when a role names
no module.

- **Server stores** — one port per domain in the core module it serves, shaped so a database or
  an object store could implement it, and speaking in domain types, never paths
  ([ADR-0023](adr/0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)):
  `AgentStore` in `agent_store`, which holds each Agent's labels in its record, writes a rollout's
  revisions and assignments as one operation, answers what an assignment pins, and answers a page
  of the list of Agents (`query_page`) and the values the fleet reports (`reported_values`), with
  the filter, the order and what is derived from each record defined in the core module `listing`
  ([ADR-0031](adr/0031-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md));
  `ConfigBackend` under the `ConfigStore` in `configs`; `DeploymentBackend` under the
  `DeploymentStore` in `deployments`; `PackageBackend` and `BlobStore` (`blobs`, artifact bytes by
  SHA-256) under the `PackageStore` in `packages`, the bytes in `data_dir` or, with
  `[blobs] store = "s3"`, in one bucket of an S3-compatible store through the adapter in `s3`, which
  bridges the synchronous port onto a runtime of its own (`bridge`)
  ([ADR-0023](adr/0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md));
  `BootstrapStore` under the
  `BootstrapCertificates` in `bootstrap`; `EnrolmentStore` under the `Enrolment` in `enrolment`;
  `FloorStore` under the `PackageFloors` in `floors`, the floor the operator signed per Agent type
  ([ADR-0035](adr/0035-package-versions-high-water-mark-and-floor.md));
  `LedgerStore` in `revocation`; `AuditStore` in `audit_log`. The filesystem adapters are in `fs`,
  which also opens the data directory and refuses anything its layout does not name. With
  `[database] store = "postgres"` every port but the blob port has its PostgreSQL adapter in
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
  numbered migrations; `postgres::import` is `server migrate-to-postgres`, which copies a `data_dir`
  into the database offline, and `server blobs-to-s3`, which copies the blob files into the bucket
  ([ADR-0023](adr/0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)).
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
  ([ADR-0009](adr/0009-ports-and-adapters.md),
  [ADR-0023](adr/0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)).
- **Server instances** — on PostgreSQL every Server is an instance, and several share one
  database and one bucket
  ([ADR-0045](adr/0045-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)).
  `instance` takes a lease, a row of `postgres::lease`, before either listener opens, renews it,
  proves it before every answer, and closes both planes while it cannot. Each lease it takes checks
  the schema under the migrations' lock and records the instance's release and its `[audit]
  key_label`, refused while a live lease names another label, and a migration refuses
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
  writes the presence of what it carries in batches. What one instance holds of them — the take and
  the release, the presence it writes, the re-keys whose answer was lost, the restarts kept for
  retry, the take-up of what other instances announced of the Agents a notice names, the catch-up
  after a missed notification (`catch_up`: one read of the records and one of the holders of every
  Agent held, from which each kind of kept state and every group is taken up), and the per-Agent
  locks — is the core module `fleet::held`, behind the same `AppState` entry points. What it keeps
  of each Agent beside its record is one entry, `Kept`, which a forget drops and a re-key moves as
  one. How the Agents of one Client are placed under its Client Agent
  ([ADR-0030](adr/0030-the-fleet-record-of-agents-and-hosts.md) clause 19) is the core module
  `fleet::placement`, behind the same `AppState` entry points: the rule for one group, which the
  fleet in memory and a page `postgres::agent_page` reads share; the index of the groups this
  instance holds members of; whether the Client Agents of a group another instance carries are read
  under the fleet lock or before it (`Elsewhere`); and the placing again after a notice or a
  catch-up.
- **Server listeners** — `tls` reads the `[tls]`, `[rest.tls]` and `[enrolment]` files into the
  material each plane serves with — the Agent plane requiring a client certificate in the
  handshake, the Operator plane asking for none and serving `[rest.tls]`'s certificate when the
  section is present
  ([ADR-0012](adr/0012-tls-1-3-two-planes-and-bounded-input.md)) — and tells a member's certificate
  from a bootstrap one by its
  issuer. `listen` serves both planes on one handle with one drain and a connection cap each
  ([ADR-0012](adr/0012-tls-1-3-two-planes-and-bounded-input.md)). `pki` is `server pki status`
  and the running Server's warnings before a certificate it depends on ends (ADR-0042).
- **Server admission** — `transport::Admission` requires the client certificate on every request
  to `/v1/opamp` and reads no `Authorization` header, admits a bootstrap certificate only while the
  enrolment window is open, and guards the package download with the same certificate. Behind
  the same admission it serves a Gateway its revocation list on `/v1/gateway/revocations` and
  every member the floors on `/v1/package-floors`
  ([ADR-0035](adr/0035-package-versions-high-water-mark-and-floor.md)). Two core
  modules hold the state behind it: `enrolment`, the operator-opened window, the queue of requests
  an operator approves or rejects through `api`, and the rejected keys, refused beyond the window,
  and `throttle`, the per-address back-off after
  repeated failures, which both planes use
  ([ADR-0040](adr/0040-admission-by-client-certificate-and-revocation-with-the-ca-keys-in-a-file-or-on-a-token.md)).
  `revocation` holds the register of issued certificates, the revocation list and the Hosts —
  each with the subject values of its certificate, its Gateway mark and the Agents bound to it —
  behind the `LedgerStore` port
  ([ADR-0040](adr/0040-admission-by-client-certificate-and-revocation-with-the-ca-keys-in-a-file-or-on-a-token.md),
  [ADR-0030](adr/0030-the-fleet-record-of-agents-and-hosts.md)).
  `agent_rate` holds the token buckets that bound how often an admitted host, an Agent behind a
  marked Gateway, and the Gateway as a whole are heard, in the bounded tables of
  `fleet_core::rate`; `transport` takes a token for each
  message before the handler does anything else and for each download in the guard
  ([ADR-0012](adr/0012-tls-1-3-two-planes-and-bounded-input.md)).
- **Audit record** — `audit` is the port every security decision is recorded through;
  `audit_log` chains the entries by hash and `fs::FsAuditStore` keeps them under `data_dir/audit/`
  ([ADR-0028](adr/0028-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again.md)).
  An act a restore
  repeats by its entry — a revocation, a Gateway mark, a floor set, a rejection, the window
  closed — holds a place in the writer's channel first, an `audit::Permit`, and writes the entry
  into it once it has run. On PostgreSQL
  `postgres::audit::PgAuditStore` keeps one chain per lease; the lease's take opens it and closes
  the chains of the lapsed leases it removes, as each renewal does, and an orderly shutdown closes
  the instance's own
  ([ADR-0029](adr/0029-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under.md)).
  Each entry names the lease its decision was taken under (`taken_under`), and on PostgreSQL the
  writer keeps a batch whose write failed and tries it again first
  ([ADR-0029](adr/0029-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under.md),
  [ADR-0028](adr/0028-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again.md)).
  `postgres::audit_prune` bounds it by `[audit] retain_days`: the task `main` starts with its
  `prune_daily` prunes at startup and once a day, the instance's own chain always and the closed
  chains while it holds the prune's advisory lock, recording each `audit.pruned` through the
  instance's writer and waiting for it before it deletes, in rounds of a bounded number of entries,
  every chain of one round in one transaction
  ([ADR-0023](adr/0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
  clause 12).
  With `[audit.export]`, `audit_export` sends every line a store has accepted, the writer's and
  those a lease's transaction writes, as an OTLP log record to the receiver it names: a queue of
  its own, a thread that sends through `opentelemetry-otlp` over the TLS 1.3 *ring* client and
  tries again until the receiver takes a batch, and the count of what the full queue dropped
  written back as `audit.export_dropped`
  ([ADR-0043](adr/0043-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md)
  clause 9).
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
  ([ADR-0041](adr/0041-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token.md)).
- **Server ports beyond storage** — `fleet` owns `CertificateSigner`, which the local CA in `ca`
  implements for the CSR flow and the renewal proof (ADR-0040), and the `UseCase` each signature
  names. A CA key in `ca` is a key file or a key on the PKCS#11 token of `pkcs11`, which signs
  through `rcgen`'s `SigningKey` from a pool of four sessions, one `cryptoki` context per module in
  the process, and holds a place in the audit record before it signs
  (ADR-0043);
  `opamp-fleetctl`'s `token` makes the keys on it. `fleet` also owns
  `Clock`, which `clock::SystemClock` implements. It owns `Elapsed` too, the time that has passed
  as a lease and a sweep measure it: `PackageStore::with_backend` takes it, and `instance`'s
  `Lease::take_as`. `clock::Elapsed::monotonic` is the adapter on `std::time::Instant`, and
  `main.rs` hands both of them `CLOCK_BOOTTIME`, which counts a suspend of the host, on Linux
  ([ADR-0045](adr/0045-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)
  clause 3). The `[connection_offer]` and `[telemetry_offer]` sections become the fleet's offers in
  `config`.
- **Server configuration** — `config` builds one TOML document from the defaults, `server.toml`
  when one is read, and the `FLEET_SERVER__…` variables over it, and deserializes it once, so the
  environment is checked as strictly as the file; `config::server_variables` reads the
  `FLEET_SERVER__…` variables of the process, and `main.rs` hands them in
  ([ADR-0024](adr/0024-the-servers-configuration.md)).
- **REST views** — what the REST API reads and returns is shaped in `api`, which derives its
  OpenAPI schema ([ADR-0032](adr/0032-configurations-and-the-rest-api.md)); the core's types —
  among them the fleet view `AgentView` — carry no schema, and `api` converts them. `api::paging`
  reads `limit` and `cursor`, encodes the opaque cursor, and writes `X-Total-Count` and the `Link`
  of the four lists that page.
- **Client supervision** — `supervisor::ports` is the Port a Plugin implements
  ([ADR-0046](adr/0046-supervisor-mode-and-its-kinds-and-a-repeated-offer-is-applied-once.md)),
  and `shutdown` the handle every task stops on; the Plugins and the process runner are its
  adapters, among them the two Supervisors
  this project brings, the Collector's in `supervisor::collector` and the example Custom
  Supervisor in `supervisor::command`. How a `[[supervisor]]` block is
  read — its plugin, program, Agent type and timings — is `supervisor::block`, and what a
  Server-delivered block may bring is `supervisor::delivered`; the registry of Plugins stays in
  `supervisor`, which knows the kinds and hands them to both, and which hands `delivered` the
  Supervisors' root as an absolute path too. When a failed process is started again or held is
  `supervisor::restart`'s decision; the runner reads the clock and starts it. Who starts the
  program is the runner's `Spawner`: the Client itself, or the second service of
  [ADR-0021](adr/0021-the-accounts-the-service-runs-under.md),
  `supervisor run --agents`, through `supervisor::channel`, which holds the private protocol, the
  service and the Client's end of its socket — on Windows of its named pipe, in
  `supervisor::channel::windows`, which runs each program in a job object. The Windows system
  calls neither the standard library nor a dependency offers — the SIDs, the pipe's security
  descriptor and its two ends' identities, the job objects — are declared in `win32`. The
  registry hands every plugin the one `Spawner`
  of the run, which `service::runtime` chooses once from the Client's command line — the unit
  or plist `service install --process-account` writes names the second service's socket there —
  and hands `build_engine`. The Engine keeps that registry behind the port `PluginRegistry` of
  `supervisor::ports`, so a Supervisor a Supervisor-set apply starts later starts its program
  the same way, and a Supervisor it purges has the same `Spawner` remove first what its
  programs wrote under their own account, in the directory the purge first moved aside to a name
  no Supervisor carries. What the Client changes where the process account
  writes, it reaches through `storage::handle`, one directory handle at a time from a root of its
  own — the state directory, the Supervisors' root, a Supervisor's directory — beneath which no
  link is followed. On Windows that is `win32::files`, which sets a DACL on one handle at a time
  and walks a tree itself, never into a link, with the ACE inheritance rules in `win32::acl`; the
  install's hand-over and its take-over of the data root walk the same way.
  `supervisor::agent`, an Agent's decisions, names four more ports there: `AgentStorage`, which
  `storage::Storage` implements on the state directory, `HighWaterMarks`, which
  `storage::HighWaterFile` implements on `state_dir` and every Supervisor of the Client shares
  ([ADR-0035](adr/0035-package-versions-high-water-mark-and-floor.md)),
  `PackageFloors`, which `floors::FloorFile` implements on `<state_dir>/package-floors.json` and
  fetches from the endpoint, every Agent of the Client holding to it, the self-update included
  ([ADR-0035](adr/0035-package-versions-high-water-mark-and-floor.md)),
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
  ([ADR-0021](adr/0021-the-accounts-the-service-runs-under.md)). `nofollow` reads every file one
  way: a regular file within a bound the caller names, into a buffer sized before it reads and
  wiped when dropped ([ADR-0014](adr/0014-secrets-in-memory-are-wiped-on-drop.md)). A path a
  privileged step did not choose itself is resolved by `nofollow::read_privileged`, described
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
  (`config_init::Offered`). A run that answered the questionnaire or
  `--endpoint` hands the enrolment the endpoint and the CA certificates it copied
  (`config_init::Answered`), which stand in for what the file says by then; the answered CA file
  itself is read as the enrolment reads one, and only its certificates are kept. `--endpoint` over a
  kept file refuses the enrolment where the file names another endpoint. What else runs under the
  Client's or the process account — the self-update's and a Supervisor's program staging through
  `install`, the Gateway's package cache, and the second service's purge on Windows, which goes
  by path — works by path in directories that account owns and no other account may write.
- **Client configuration** — `config` is what `supervisor.toml` may say and the rules it must meet
  ([ADR-0008](adr/0008-five-crates-on-tokio-and-axum.md)); `config_file` reads it from disk, makes
  its
  directories absolute, finds the identity the state directory holds, and redacts the text the
  Client reports as its effective configuration.
- **Client connection** — `tls` decides which CA and which identity are in force, and `transport`
  describes the upstream `Connection` from `supervisor.toml` and runs the Engine over it. The
  settings verification in `connection`, the Gateway's upstream pool (`gateway::pool`) and its
  downstream listener, and the Supervisor Endpoint (`supervisor::endpoint`) use the same `opamp`
  building blocks
  ([ADR-0008](adr/0008-five-crates-on-tokio-and-axum.md)). The Gateway's
  `revocations` keeps the list it fetches from its Server and refuses what that list names
  ([ADR-0016](adr/0016-client-modes-and-the-gateway.md)),
  and its `floors` fetches the floors and hands the hosts behind it the ones it last fetched,
  behind the same verdict
  ([ADR-0035](adr/0035-package-versions-high-water-mark-and-floor.md)).
  The Gateway's `cache` sees every offer `registry` hands down, fetches each artifact offered on
  the Server's route once into the state directory, and serves it on the downstream download route
  to the hosts it was offered to ([ADR-0034](adr/0034-packages-signatures-and-delivery.md)).
  Which host the Gateway takes to speak for an `instance_uid` is decided in the core module
  `gateway::policy`; the cache holds the bindings and keeps them across restarts in the journal
  `gateway::store` reads and writes beside the artifacts; while that journal cannot be read the
  Gateway carries no downstream Agent
  ([ADR-0016](adr/0016-client-modes-and-the-gateway.md)).
  With a bootstrap CA the Gateway tells an
  enrolling peer from a member and relays its one plain-HTTP enrolment request without a binding
  ([ADR-0016](adr/0016-client-modes-and-the-gateway.md)).
  `gateway::rate` holds a token bucket per downstream host, or per enrolling peer's address, in a
  table of `fleet_core::rate`; the downstream endpoint and the download route take a token before
  they do anything else
  ([ADR-0022](adr/0022-a-gateway-rate-limits-each-downstream-host-as-the-server-limits-a-host.md)).
- **Client engine** — `engine` routes the Server's replies to the Agents over one connection
  ([ADR-0016](adr/0016-client-modes-and-the-gateway.md)). The transports, the Gateway, telemetry
  (`telemetry`) and the service runtime are adapters around it.
- **Client service** — `service` is what makes the Client an OS service, but for its log on disk.
  `service::manager` registers, starts, stops and deregisters the Client's
  service and, where a process account is named, the second service `<PRODUCT_NAME>-agents` beside
  it, and writes both systemd units, or both launchd plists, itself
  ([ADR-0021](adr/0021-the-accounts-the-service-runs-under.md)). `service::run_as`
  decides and checks the Client's account and the process account, and hands the roots over,
  leaving what the process account owns. On Windows `service::windows_config` sets the SCM's
  accounts, recovery actions and required privileges, `service::windows_rights` restricts the data
  root, `service::run_as` also grants the process account its share of the layout and withdraws a
  former account's grants, and `service::withdrawal` keeps the record
  of what an install left to finish — the former accounts to withdraw and the services to start
  again — under `HKLM\SOFTWARE\<PRODUCT_NAME>\WithdrawalPending`; the install lock that serialises
  `service install`, `uninstall`, `start` and `stop` is in `win32`.
- **Client self-update** — `update` is the Client updating itself
  ([ADR-0019](adr/0019-the-client-updates-itself-from-a-signed-package.md)). It owns the port
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
the Supervisor's `config/` directory and restarts its process; the Agent reports `APPLIED` or
`FAILED` with the reason, and the Server stops offering once the reported hash matches
([ADR-0032](adr/0032-configurations-and-the-rest-api.md),
[ADR-0033](adr/0033-deployments-and-rollouts.md)). A
Configuration typed for the Client
itself carries `[[supervisor]]` blocks, which `reconfigure` checks and writes into
`supervisor.toml` before it starts or stops anything
([ADR-0046](adr/0046-supervisor-mode-and-its-kinds-and-a-repeated-offer-is-applied-once.md)).

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
([ADR-0030](adr/0030-the-fleet-record-of-agents-and-hosts.md)).

**A package reaches an Agent.** An operator uploads an artifact into `packages` and signs its
entry there — `packages` checks the signature against `[packages] verification_key` when the
Server holds one, and only reports — then puts the Package into a Deployment with a Selector and
releases it, together with the Configurations the Deployment holds. The Agent is offered the package
with its entry's signature; the Client downloads it
from its own Server's origin and nowhere else, and `api` serves it only when `fleet` finds it
offered to an Agent the certificate's host speaks for, by the test `packages` shares with the
offer ([ADR-0034](adr/0034-packages-signatures-and-delivery.md)); behind a Gateway the
Client downloads it from the Gateway's cache, which `gateway` fetched once from the Server. The
Client checks hash and signature in `packages`, and the
Supervisor swaps the program, keeps the previous one for its grace period and rolls back if the
new one does not stay up. Before anything is downloaded, an offer below the floor the operator
signed for its Agent type is refused: the Client fetches the floors from its endpoint as they fall
due and keeps the highest that verifies
([ADR-0035](adr/0035-package-versions-high-water-mark-and-floor.md)). The Client's own package goes
through `update` instead: a version
directory beside the running one, a self-check, the `current` pointer moved, and a restart on
probation that commits or rolls back.

**A host joins and stays.** The Server issues bootstrap certificates on an operator's request:
`bootstrap` checks the subject and the life, records what was issued, and asks its
`BootstrapSigner` port — the bootstrap CA with its key in `ca` — to make the key, sign, and
seal both into the PKCS#12 file it keeps, the key's only way out
([ADR-0027](adr/0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
A fresh host presents a bootstrap certificate while an operator has
the enrolment window open — to the Server, or to a Gateway that relays its request — and proves it
in its CSR ([ADR-0016](adr/0016-client-modes-and-the-gateway.md));
the request is filed under the fingerprint of the key it asks a certificate for and waits until
an operator approves or rejects it — a rejected key stays refused beyond the window, and may be
rejected before its request arrives — and the certificate it
gets names a new host and carries the subject values `tls` read from the bootstrap certificate
and `ca` checked again, never the CSR's; every renewal carries the predecessor's
([ADR-0027](adr/0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
`service install` — asked through its questionnaire, or unasked with the bootstrap identity found
beside the program or in `--fleet-files` — and `service config` do this before the service is
registered: `service::enrol` runs the Client's own Agent over the ordinary `transport` with the
bootstrap pair, or the identity a `bootstrap.p12` opened to, until the issued certificate is in
the state directory
([ADR-0027](adr/0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)).
From then on the Client renews at two thirds of the certificate's life,
proving with the old key which certificate it renews. A revocation closes the sessions it
concerns at once, on every instance by its notification, and follows every renewal.

**An instance is lost.** Killed, its connections are cut; frozen, its Agents end their
WebSockets after one heartbeat interval of silence. Each Agent reconnects after a delay drawn over
`[0, 30 s)`, through the balancer, to an instance that still holds its lease, which reads the
Agent's record at its first report and takes it as its holder. Once the lost instance's lease has
lapsed, the instance that removes it closes its audit chain. The others serve from what the lost
instance committed to the database
([ADR-0045](adr/0045-several-server-instances-share-one-postgresql-database-and-a-group-is-placed-again-at-once.md)).

**A Supervisor starts its program.** Without a process account the runner starts the Managed
Process, its version probe and a package's preflight as the Client. Where `service install
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
([ADR-0021](adr/0021-the-accounts-the-service-runs-under.md) clause 19).
Both ends refuse a peer of another build, and the one that `current` no longer names exits so its
service manager starts it from the new version. While the second service is away, a delivered
package is staged and
waits, and the Supervisor retries with backoff
([ADR-0021](adr/0021-the-accounts-the-service-runs-under.md)).

**Where state lives.** The Server keeps all of its state under one `data_dir`, one directory per
domain and owner-only: `configurations/`, `deployments/`, `packages/` (metadata) and `blobs/` (the
artifacts, named by SHA-256, unless a bucket keeps them), `agents/` (each Agent's record, its labels
inside), `hosts/`,
`certificates/` (the register), `revocations.json`, `bootstrap/`, `enrolment.json` (the window and
its queue), `package-floors.json` (the floor of each Agent type) and `audit/`
([ADR-0023](adr/0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)).
With `[database] store = "postgres"` the records are rows of one PostgreSQL database instead, and
`data_dir` keeps, with the filesystem blob store, `blobs/` and an archived `audit/`; with the blobs
in S3 too there is no `data_dir`, and an instance keeps nothing on its disk
([ADR-0023](adr/0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)).
On files the Server loads every record at startup and answers from memory; on PostgreSQL an
instance holds only the Agents connected to it, each read at its first report, and the Operator
plane reads the fleet from the database.
A restart on files restores the fleet from them and shows each Agent disconnected until it
reports; an
installation an older Server wrote is moved there once by `server migrate`. The Client keeps its
state
under `state_dir`: the issued certificate and key, the connection settings the Server offered,
and per Supervisor a directory with `config/` and `program/`. An installed Client runs from
`versions/<version>/` through the `current` pointer
([ADR-0021](adr/0021-the-accounts-the-service-runs-under.md)).
