# ADR-0057: Several Server instances share one PostgreSQL database and coordinate through it

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the in-memory fleet and its push in `crates/fleet-server/src/fleet.rs`, the sessions and their guard in `crates/fleet-server/src/transport.rs`, the change signals of `crates/fleet-server/src/enrolment.rs` and `crates/fleet-server/src/revocation.rs`, the per-instance tables of `crates/fleet-server/src/agent_rate.rs` and `crates/fleet-server/src/throttle.rs`, the PostgreSQL adapters and their schema, the `[database]` section of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, `docs/ARCHITECTURE.md`, and the operator manual's pages on replication and load balancing
- **Supersedes:** [ADR-0024](0024-the-fleet-record.md) clauses 4, 5, 7 and 9; [ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md) clause 40

## Context

The Server runs as one process. It loads the whole fleet into memory at startup and holds it there
(`AppState` in `crates/fleet-server/src/fleet.rs`). Three in-process signals tell live sessions
that something changed: the fleet's `push` channel, a `watch::Sender<u64>` bumped by a rollout, a
publication or a restart; the revocation list's `subscribe`, which every WebSocket session's guard
waits on to end itself when its certificate is revoked
([ADR-0021](0021-admission-by-a-client-certificate-alone-and-a-revocation-list-that-follows-renewal.md)
clause 40); and the enrolment queue's `subscribe`, which hands an approved certificate to the
connection that asked for it. `connected` is what one process knows about its own connections and
is never persisted ([ADR-0024](0024-the-fleet-record.md) clauses 4 and 7). The rate limits and the
admission throttle are tables in memory
([ADR-0013](0013-tls-1-3-plaintext-on-the-loopback-alone-and-bounded-planes-bodies-and-messages.md)
clause 26, ADR-0021 clause 24). Duplicate detection tells two connections of one `instance_uid`
apart by a connection id that only this process hands out.

One process is one point of failure and one machine's worth of connections. The specification's
goal G-20 asks for a fleet of about 100 000 Agents served with no single Server instance whose loss
stops it. The working note "server split", recorded outside the ADRs, sets the target in its
section D7: several instances, active-active, sharing state only through PostgreSQL and the object
store, with presence written in batches, events broadcast to every instance, and a lease per
instance. [ADR-0050](0050-the-servers-records-may-live-in-postgresql.md) (the PostgreSQL store) puts every store behind the ports into one database for a
single instance; [ADR-0055](0055-the-audit-record-is-one-hash-chain-per-server-process.md) (audit chains per instance) gives each writing instance its own chain;
[ADR-0056](0056-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md) (paging) lets the Operator plane read the fleet a page at a time from the database; and
[ADR-0053](0053-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md) (keys in an HSM) moves the CA and audit keys out of the instance, so that every instance
can sign. This ADR is what turns those into several instances.

At a 30-second heartbeat, 10 000 Agents send about 330 messages a second and 100 000 about 3 300.
Writing `last_seen_ms` on every heartbeat would make the database the bottleneck for messages that
change nothing else.

A reader would expect two things this ADR decides against: a routing table that says which instance
carries which Agent, so an event goes only there; and rate limits shared across instances.

## Decision

We will let any number of Server instances serve one fleet from one PostgreSQL database, each
holding a lease and the sessions it carries, writing presence in batches and telling the others of
every change that concerns a live session by one `NOTIFY`.

1. **Every Server on PostgreSQL is an instance, and there is no switch for "several".** With
   `[database] store = "postgres"` ([ADR-0050](0050-the-servers-records-may-live-in-postgresql.md)) a Server takes an instance id at startup — a random
   UUID that is not persisted — and a lease in the database before it opens either listener. How
   many instances run is whatever the operator starts. With `store = "fs"` there is one instance
   and no lease, as today.

2. **Several instances need shared blobs and shared keys.** An instance whose `[blobs] store` is
   `"fs"`, or whose CA or audit key is a file rather than a key on the PKCS#11 token of [ADR-0053](0053-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md)
   (keys on a token), takes, besides its lease, an exclusive lock that says it is the only
   instance; a second instance on the same database then fails startup, naming the key. Instances
   that share keys reach the same ones: a network HSM all of them reach, or a token that the HSM's
   own replication keeps identical on each host. SoftHSM2 serves one host and therefore one
   instance. The Server does not copy keys between tokens. With `store = "s3"`
   every instance uses the same bucket and prefix. This is the later decision that clause 2 of
   ADR-0049 leaves the sharing of a bucket to: the startup sweep and the staging cleanup run under
   an advisory lock, so one instance does them at a time.

3. **The lease.** Each instance renews its lease every `lease_renew_secs` (default 10) and it lapses
   `lease_secs` (default 30, at least three renewals) after the last renewal. An instance that
   cannot renew before its own lease lapses ends every session it carries, refuses admission on
   both planes with `503`, and takes a new lease, under a new instance id, once the database
   answers again. It never serves on a lease it cannot prove, because it could no longer learn of a
   revocation. On graceful shutdown an instance writes its presence, releases its lease and drains.

4. **Restates ADR-0024 clause 4. What a report or an operator decided persists; so does where an
   Agent is connected.** Persisted: `sequence_num`, `capabilities`, `description`, `health`,
   `effective_config`, `remote_config_status`, `connection_settings_status`, `package_statuses`,
   `available_components`, `transport` (informational), `last_seen_ms`, `restart_pending`, and the
   assignments. On PostgreSQL the Agent's presence is persisted beside them: `connected` and the
   id of the instance that holds its connection. The owning connection itself is not persisted.
   Labels are not duplicated into the record; their own store is the single authority and fills
   the mirror on restore.

5. **Restates ADR-0024 clause 5. The write discipline lives above the port, so no backend can get it
   wrong.** The fleet keeps a digest of each record's durable content as last written and calls
   `put` only when it changed. `last_seen_ms` and `sequence_num` are outside the digest. On the
   filesystem they ride along with whatever write happens, so a heartbeat reaches no adapter. On
   PostgreSQL each instance writes the presence of the Agents it carries — `connected`,
   `last_seen_ms`, the holding instance — in one batch every `presence_interval_secs` (default 30,
   from 10 to 60), so a heartbeat reaches the database at most once per interval and Agent. A
   change of the holding instance and a disconnect are written in the next batch, except as clause
   8 says. On graceful shutdown every record and every presence is flushed. A failed write is
   logged and retried with the next batch, never fatal.

6. **Restates ADR-0024 clause 7. `connected` says a connection carrying the Agent is open — nothing
   more.** Every report sets it; only the owning WebSocket closing, or the Agent's
   `agent_disconnect`, clears it. Behind a Gateway it is the Gateway's connection; an Agent that
   stops polling plain HTTP without saying goodbye stays connected. On PostgreSQL, an Agent whose
   presence names an instance whose lease has lapsed reads as disconnected, whatever its row says.
   A restored record is disconnected with no owning connection until live evidence says
   otherwise.

7. **Restates ADR-0024 clause 9. The budget is three heartbeat intervals, and on PostgreSQL the
   presence interval besides.** When the Server offers a heartbeat interval (`[connection_offer]
   heartbeat_interval_secs`, [ADR-0014](0014-connection-settings-offered-without-a-credential-and-server-capabilities.md)),
   the budget is three times it; otherwise `stale_after_secs` in `server.toml`, default **90** —
   three times the Baseline's default heartbeat of 30 seconds — and never zero. On PostgreSQL the
   budget is that plus `presence_interval_secs`, since `last_seen_ms` in the database may be one
   interval old. One missed heartbeat is a lost packet; a view that flickers on every hiccup is one
   nobody trusts.

8. **An instance holds only the Agents it carries, and the newest connection takes an Agent.**
   Each instance keeps in memory the records of the Agents whose connection it holds, loaded from
   the database when their first report arrives, and nothing of the rest. When an instance begins
   to carry an Agent whose presence names another instance with a live lease, it writes itself as
   the holder at once, outside the batch, and announces it (clause 9); the other instance stops
   carrying that Agent, as it would on a closed connection. Duplicate detection and the rekeying
   of a newcomer stay within one instance. An Agent whose holder changes more than ten times in a
   minute is logged as a likely duplicate.

9. **Every change that concerns a live session is one `NOTIFY` to every instance.** The writing
   instance commits the change and, in the same transaction, sends one `NOTIFY` on one channel,
   naming the kind and the identifier and nothing else: an assignment changed, a Package or
   Deployment published, a restart requested, a revocation added or lifted, an enrolment request
   decided, the enrolment window opened or closed, an Agent forgotten, an Agent taken by another
   instance. Each instance listens on a connection of its own and acts on the sessions it holds: it
   composes a new offer, ends a revoked session (clause 11), offers an approved certificate,
   closes its enrolment connections, or drops the Agent. There is no table of which instance holds
   which session, and no message addressed to one instance.

10. **A missed notification counts as every notification.** `NOTIFY` reaches only the sessions
    listening when it is committed. When an instance's listening connection is lost and
    re-established, it acts as if everything had changed: it drops its caches, re-reads the
    revocation list and checks every session against it, and re-reads the assignments of every
    Agent it carries. It does the same when a lease renewal finds that the revocation list's
    version in the database is newer than the one it holds.

11. **Restates ADR-0021 clause 40. A revocation ends the sessions it concerns, and only those, on
    every instance.** Each instance records, per WebSocket session, the issuer and serial of the
    certificate that admitted it. A revocation, committed on any instance, reaches every instance
    by its notification or by clause 10, and is checked against every open session there at once;
    a session checks itself once more as it starts, so a revocation landing during its admission is
    not missed. A session it concerns is closed with WebSocket close code `1008` (policy violation)
    and the reason `revoked`. Other sessions are not touched. On plain HTTP every request is
    admitted anew, against the list the instance holds, so the next poll is refused.

12. **Caches are dropped, not updated.** Each instance may hold Configurations, Deployments,
    Packages and the revocation list in memory, as the ports allow
    ([ADR-0037](0037-the-servers-state-lives-under-one-data-directory-one-store-per-domain.md)
    clause 3). A notification of their kind drops the entry, and the next use reads it again.

13. **The Operator plane reads the fleet from the database.** What it shows of an Agent — its
    record, its presence, its staleness — comes from the database, a page at a time ([ADR-0056](0056-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)),
    never from the memory of the instance that serves the request.

14. **Rate limits and the admission throttle stay per instance.** Their buckets live in each
    instance's memory as today; a host that reconnects to another instance starts there with a full
    bucket. The manual tells operators to give the Agent plane's load balancer source-address
    affinity, which keeps a peer on one instance and the throttle effective.

15. **TLS ends at the instance.** In front of the Agent plane sits a layer-4 load balancer with TLS
    passthrough: the client certificate in the handshake is admission, so it never terminates at the
    balancer. The Operator plane may sit behind one in the same way; every request carries its
    credential, so no session state is shared.

16. **Two consecutive versions run side by side.** Each schema change is expand-then-contract over
    two releases, so a rolling update runs version N and N+1 against one database. An instance
    refuses to start on a schema older than its version expects or newer than the next one,
    naming both versions.

17. **The database's durability is the operator's.** The manual states synchronous replication as
    an operating requirement: a failover must lose no audit entry and no revocation. Failover
    itself is the operator's infrastructure; an instance only reconnects, and clause 3 decides
    what it does in between.

**Out of scope:** the PostgreSQL schema and adapters themselves ([ADR-0050](0050-the-servers-records-may-live-in-postgresql.md)); the audit chains and
their checkpoints ([ADR-0055](0055-the-audit-record-is-one-hash-chain-per-server-process.md), [ADR-0053](0053-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md)); paging and filtering ([ADR-0056](0056-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)); sharing rate limits across
instances; a routing table or instance-to-instance messages; several databases or regions; the
Agent plane's load balancer and the database cluster, which the operator runs.

## Alternatives considered

- **A routing table of which instance holds which Agent.** An event would reach only the instance
  that needs it. The table must be written on every connect and reconnect and goes stale when an
  instance dies, and a missed route is a missed revocation. A broadcast costs every instance one
  look per event. Rejected.
- **Presence on every heartbeat.** It keeps `last_seen_ms` exact, at about 3 300 writes a second
  for 100 000 Agents, for a value the view shows to the second. Rejected for batches.
- **Shared rate limits in the database.** A token per message in PostgreSQL turns every message into
  a write. Per-instance buckets with source-address affinity keep the limit effective for the
  peers it is meant for. Rejected.
- **An external message bus** (NATS, Redis). It is one more component to run and secure, and
  `NOTIFY` already commits with the change it announces, so no event leaves before its data.
  Rejected.
- **A designated leader that alone handles events.** It brings back the single point of failure
  this ADR removes. Only the audit checkpoint has a single signer, by an advisory lock that any
  instance can take. Rejected.
- **Keep serving through a database outage.** An instance would admit with the revocation list it
  last saw and could not learn of a new revocation. Rejected: clause 3 fails closed once the lease
  lapses.
- **Duplicate detection across instances.** It would rekey an Agent that only reconnected to
  another instance after a lost connection, before the old instance noticed. Rejected; the newest
  connection takes the Agent.

## Sources / Prior art

- The working note "server split", section D7, recorded outside the ADRs.
- PostgreSQL documentation: [`NOTIFY`](https://www.postgresql.org/docs/current/sql-notify.html)
  (delivered on commit, only to sessions listening at that moment, payload below 8000 bytes) and
  [advisory locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS).
- [Patroni](https://patroni.readthedocs.io/) and [CloudNativePG](https://cloudnative-pg.io/) for
  synchronous replication and failover run beside the application.
- The code as it stands: `AppState`, its `push` channel and the duplicate detection in
  `crates/fleet-server/src/fleet.rs`; the session guard in `crates/fleet-server/src/transport.rs`;
  `subscribe` in `crates/fleet-server/src/revocation.rs` and `crates/fleet-server/src/enrolment.rs`.

## Consequences

- Positive: G-20. Losing an instance loses its sessions, whose Agents reconnect to another; the
  fleet is served on. Capacity grows by adding instances.
- Positive: no routing state to keep correct; every instance runs the same code on the same data.
- Negative / trade-offs: a database outage longer than a lease stops admission on every instance.
  That is the price of never admitting against a stale revocation list.
- Negative / trade-offs: `last_seen_ms` in the view may be one presence interval old, and the
  staleness budget grows by that interval.
- Negative / trade-offs: every instance wakes on every event, and an instance whose listening
  connection drops re-reads everything it holds.
- Negative / trade-offs: a peer spread across instances meets a rate limit per instance; the
  manual's source-address affinity is what keeps the limit as tight as today.
- Follow-ups: revising ADR-0049 so that its startup sweep and staging cleanup take an advisory lock;
  the operator manual's pages on replication, load balancing and rolling updates; whether the
  Operator plane should name the instance that holds an Agent.

## Enforcement

Tests that will carry `Verifies: ADR-0057`, run against the PostgreSQL that
`FLEET_TEST_POSTGRES_URL` names, with two instances in one test process:

- A rollout on instance A reaches an Agent connected to instance B, and a revocation on A closes
  B's session of that certificate with `1008` and `revoked`.
- An enrolment approved on A is offered on B's enrolment connection.
- When B's lease lapses, B's Agents read as disconnected through A's Operator plane; B ends its
  sessions and answers `503` until it holds a new lease.
- After B's listening connection is dropped and restored, a revocation committed in between still
  ends B's session.
- A second instance on a database where the first runs with `[blobs] store = "fs"`, or with a
  `[client_ca] key_file`, fails startup naming the key.
- One hundred heartbeats of one Agent within one presence interval write its presence once.
- An Agent that reconnects from B to A is held by A, and B stops carrying it.
- An instance refuses to start on a schema two versions ahead, naming both versions.
