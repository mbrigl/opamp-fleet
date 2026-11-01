# ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it: Several Server instances share one PostgreSQL database and coordinate through it

- **Status:** 🟡 proposed
- **Date:** 2026-10-22
- **Deciders:** Markus Brigl
- **Applies to:** the in-memory fleet and its push in `crates/fleet-server/src/fleet.rs`, the sessions and their guard in `crates/fleet-server/src/transport.rs`, the ports, the PostgreSQL adapters and the change signals of the enrolment window and its queue in `crates/fleet-server/src/enrolment.rs` and of the certificate register, the revocation list and the Hosts in `crates/fleet-server/src/revocation.rs`, the per-instance tables of `crates/fleet-server/src/agent_rate.rs` and `crates/fleet-server/src/throttle.rs`, the package floors in `crates/fleet-server/src/floors.rs` and `crates/fleet-server/src/postgres/floors.rs`, the PostgreSQL adapters of Configurations, Deployments and Packages in `crates/fleet-server/src/postgres/`, the PostgreSQL adapters and their schema, the audit port in `crates/fleet-server/src/audit.rs` and its PostgreSQL adapter where an instance closes the chain of a lapsed lease, the `[database]` section of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, `docs/ARCHITECTURE.md`, and the operator manual's pages on replication and load balancing

## Context

The Server runs as one process. It loads the whole fleet into memory at startup and holds it there
(`AppState` in `crates/fleet-server/src/fleet.rs`). Three in-process signals tell live sessions that
something changed: the fleet's `push` channel, a `watch::Sender<u64>` bumped by a rollout, a
publication or a restart; the revocation list's `subscribe`, which every WebSocket session's guard
waits on to end itself when its certificate is revoked
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md) clause 32); and the enrolment
queue's `subscribe`, which hands an approved certificate to the connection that asked for it.
`connected` is what one process knows about its own connections and is never persisted
([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md) clauses 3 and 6). The rate limits and the
admission throttle are tables in memory ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)
clause 28, ADR-0021 clause 19). Duplicate detection tells two connections of one `instance_uid`
apart by a connection id that only this process hands out.

One process is one point of failure and one machine's worth of connections. The specification's goal
G-20 asks that a fleet of up to about 100 000 Agents be served on when one of several Server
instances stops without warning: its Agents connected to another instance within a bound, and a
revocation ending its sessions on every instance within another. The working note "server split",
recorded outside the ADRs, sets the target in its section D7: several instances, active-active,
sharing state only through PostgreSQL and the object store, with presence written in batches, events
broadcast to every instance, and a lease per instance.
[ADR-0029](0029-the-servers-state-and-its-stores.md) (the PostgreSQL store) puts every store behind
the ports into one database for a single instance;
[ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance](draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)
(audit chains per instance) gives each writing instance its own chain;
[ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
(paging) lets the Operator plane read the fleet a page at a time from the database; and
[ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one](draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md)
(keys in an HSM) moves the CA keys out of the instance, so that every instance can sign, and
[ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token](draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token.md)
(signed chain heads) signs the heads of every chain with an audit key that exists only on the token.
This ADR is what turns those into several instances.

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
   `[database] store = "postgres"` ([ADR-0029](0029-the-servers-state-and-its-stores.md)) a Server
   takes an instance id at startup — a random UUID that is not persisted — and a lease in the
   database before it opens either listener, and keeps its listeners open only while it holds a
   lease it can prove (clause 3). Every line the instance logs carries its instance id, and the
   take, the loss, the release and the takeover of a lease are logged with every instance id
   involved, so the logs of several instances say which one acted. How many instances run is
   whatever the operator starts. With `store = "fs"` there is one instance, no lease, and no
   instance id in the log.

2. **Several instances need shared blobs and shared keys.** An instance whose `[blobs] store` is
   `"fs"`, or whose client CA's or bootstrap CA's key is a file rather than a key on the PKCS#11
   token of
   [ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one](draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md)
   (keys on a token), must be alone. It takes, besides its lease, an exclusive lock that says it is
   the only instance, a session-level advisory lock held on a connection of its own.
   - A second instance on the same database then fails startup, naming the key. So does an
     instance that must be alone when another live lease exists already, naming the key and the
     number of live leases. A lease records whether its holder must be alone; one that does, and
     whose exclusive lock is free, belongs to a process that is gone, and an instance that must be
     alone takes such a lease over as clause 18 does instead of failing.
   - When the lock's connection ends while the instance runs, the lock is gone with it. The
     instance logs an error naming the database, ends every session it carries and refuses every
     call from then on, so it never writes beside a second instance; it takes the lock again only
     when it is started again.

   The audit key has no
   file form
   ([ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token](draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token.md)
   clause 5), and every instance on one database names the same `[audit] key_label`, or none: each
   lease records the label, and an instance whose label differs from that of a live lease, a label
   against none included, fails startup, naming the key. Instances that share keys reach the same
   ones: a network HSM all of them reach, or a token that the HSM's own replication keeps
   identical on each host. SoftHSM2 keeps its token in files on one host, and the instances on that
   host may share it: that is redundancy of processes, not of hosts. Instances on several hosts
   need a network HSM, or no CA key on the Server at all, the certificates then being issued
   outside it. The Server cannot tell which host a token's files are on, so keeping a SoftHSM2
   token's instances on its host is the operator's. The Server does not copy keys between tokens.
   With `store = "s3"` every instance uses the same bucket and prefix, and the startup sweep and the
   staging cleanup run under an advisory lock, so one instance does them at a time;
   [ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md) clause 14
   decides it for the blob store. An instance on PostgreSQL and S3 keeps nothing on its own disk
   (that draft's clause 1): it reads its configuration, its TLS files and its secret files, all of
   them read-only, and with its CA keys on a network HSM, or none on the Server, it is stateless,
   and any instance can take its place. The Server's image names no data directory, so such an
   instance runs from it as it stands
   ([ADR-draft-releases-attestations-and-the-server-image](draft-releases-attestations-and-the-server-image.md)
   clause 16).

3. **The lease.** Each instance renews its lease every `[database] lease_renew_secs` (default 10)
   and it lapses `[database] lease_secs` (default 30, at least three renewals) after the last
   renewal. These two keys and `[database] presence_interval_secs`
   ([ADR-draft-the-fleet-record-of-agents-and-hosts](draft-the-fleet-record-of-agents-and-hosts.md)
   clause 4) apply to `store = "postgres"` alone; with `store = "fs"` each fails startup, naming it.
   An instance measures its own lease from the moment it sent the last renewal the database
   confirmed, which is no later than the database's own time of that renewal, so it deems its lease
   lapsed no later than the database and every other instance do. An instance that cannot renew
   before its own lease lapses ends every session it carries and closes both listeners. Once the
   database answers again it takes a new lease, under a new instance id, and opens them again. It
   never serves on a lease it cannot prove, because it could not learn of a revocation. A closed
   listener is what a load balancer's or an orchestrator's probe sees: the image's probe is a TCP
   connection to `4320` for liveness and readiness
   ([ADR-0034](0034-releases-attestations-and-the-server-image.md) clause 17), and an instance
   without a lease is then neither, so no Agent is sent to it.

   An orderly shutdown, on `SIGTERM` among others, drains the instance. It closes both listeners at
   once, so no new connection reaches it, and answers the requests already in flight on either
   plane. It ends the WebSocket sessions it carries with close code `1001` (going away), each at an
   instant drawn uniformly over `drain_secs` (default 20, from 0 to 300; 0 ends them all at once),
   so its Agents reconnect to the other instances without arriving all at once. Then it writes the
   presence of its Agents, closes its audit chain
   ([ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance](draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md)
   clause 3) and releases its lease. The default leaves the rest of an orchestrator's usual grace
   period of 30 seconds for those writes. `drain_secs` applies on the filesystem store as well,
   where the one Server drains the same way.

4. **[ADR-draft-the-fleet-record-of-agents-and-hosts](draft-the-fleet-record-of-agents-and-hosts.md)
   clause 3 decides it: What a report or an operator decided
   persists; so does where an Agent is connected.**

5. **[ADR-draft-the-fleet-record-of-agents-and-hosts](draft-the-fleet-record-of-agents-and-hosts.md)
   clause 4 decides it: The write discipline lives above the port,
   so no backend can get it wrong.** On PostgreSQL that clause writes presence in batches.

6. **[ADR-draft-the-fleet-record-of-agents-and-hosts](draft-the-fleet-record-of-agents-and-hosts.md)
   clause 6 decides it: `connected` says a connection carrying the
   Agent is open — nothing more.**

7. **[ADR-draft-the-fleet-record-of-agents-and-hosts](draft-the-fleet-record-of-agents-and-hosts.md)
   clause 8 decides it: The budget is three heartbeat intervals,
   and on PostgreSQL the presence interval besides.**

8. **On PostgreSQL an instance holds only the Agents it carries, and the newest connection takes
   an Agent.** On the filesystem store the one Server keeps the whole fleet in memory
   ([ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)
   clause 3). Each instance keeps in memory the records of the Agents whose connection it holds,
   loaded from the database when their first report arrives, and nothing of the rest. When an
   instance begins to carry an Agent, it takes the Agent at once, outside the batch, by one update
   under the row's lock that names itself as the holder and returns the holder it replaced, and
   announces the take where it replaced another holder, whether or not that one's lease is live
   (clause 9). An instance that receives the announcement reads the holder again and stops carrying
   the Agent, as it would on a closed connection, only when the row no longer names it; two
   instances that take one Agent at once therefore never both drop it. The newest connection is the
   one whose take commits last: where the take for an Agent's older connection commits after the
   take for its newer one, as a slower write may, the older connection's instance holds the row and
   the other stops carrying the Agent and takes it back at the Agent's next report on it. Duplicate
   detection and the rekeying of a newcomer stay within one instance. An Agent whose holder changes
   more than ten times in a minute is logged as a likely duplicate. An Agent's labels stay in its
   record ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md) clause 21), and there is no
   other copy. A label change, a rollout's assignments and a restart request made through any
   instance are written to their own columns of the record in the database and announced
   (clause 9). The instance that carries the Agent writes only what a report decides
   ([ADR-draft-the-fleet-record-of-agents-and-hosts](draft-the-fleet-record-of-agents-and-hosts.md)
   clause 2), so its next write keeps them whether or not the notification has reached it, and it
   takes them into the record it holds when the notification arrives. Where an effective description
   (ADR-0024 clause 18) needs the labels of a Client Agent this instance does not carry, or a Host's
   subject values, the instance reads them from the database.

9. **Every change that concerns a live session is one `NOTIFY` to every instance.** The writing
   instance commits the change and, in the same transaction, sends one `NOTIFY` on one channel,
   naming the kind and the identifier and nothing else: an assignment changed, a Package or
   Deployment published, a restart requested, a revocation added or lifted, a certificate
   registered or removed, a Host written or an `instance_uid` bound to it, an enrolment request
   queued or decided, the enrolment window opened or closed, a package floor set or removed, an
   Agent's labels set, an Agent forgotten, an Agent taken by another instance. Each instance listens
   on a connection of its own and acts on the sessions it holds: it composes a new offer, ends a
   revoked session (clause 11), offers an approved certificate, closes its enrolment connections,
   takes up an Agent's labels, or drops the Agent. There is no table of which instance holds which
   session, and no message addressed to one instance.

10. **A missed notification counts as every notification.** `NOTIFY` reaches only the sessions
    listening when it is committed. When an instance's listening connection is lost and
    re-established, it acts as if everything had changed: it drops its caches, re-reads the
    revocation list and checks every session against it, re-reads the record and the holder of
    every Agent it carries, stops carrying each one another instance holds, and re-reads the
    package floors, the enrolment window and the requests of its enrolment connections. A
    notification of a kind the instance does not know, as a newer version sends during a rolling
    update (clause 16), counts as a missed one. It does the same when a lease renewal finds that the
    revocation list's version in the database
    ([ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md) clause
    10) is newer than the one it holds.

11. **Clause 32 of
    [ADR-draft-admission-by-client-certificate-and-revocation](draft-admission-by-client-certificate-and-revocation.md)
    decides it: A revocation ends the sessions it
    concerns, and only those, on every instance.**

12. **Caches are dropped, not updated.** Each instance may hold Configurations, Deployments,
    Packages, the package floors, the revocation list, the certificate register and the Hosts in
    memory, as the ports allow
    ([ADR-0029](0029-the-servers-state-and-its-stores.md)
    clause 2). A notification of their kind drops the entry, and the next use reads it again.

13. **On PostgreSQL the Operator plane reads the fleet from the database.** What it shows of an
    Agent — its record, its presence, its staleness — and the lists of Hosts, certificates,
    revocations and pending enrolments come from the database, a page at a time where the list
    pages
    ([ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)),
    never from the memory of the instance that serves the request.
    - An Agent's view names the instance that holds it, `holder`: the instance id its presence
      names and the host name that instance's lease records, as read from the database. It is
      empty for an Agent whose presence names no instance with a live lease. The Agent's detail
      shows it, and the list of Agents filters by it (that draft's clause 6). Each lease records
      the host name of its instance for this.

14. **Rate limits and the admission throttle stay per instance.** Their buckets live in each
    instance's memory; a host that reconnects to another instance starts there with a full
    bucket. The manual tells operators to give the Agent plane's load balancer source-address
    affinity, which keeps a peer on one instance and the throttle effective (clause 15).

15. **TLS ends at the instance, and the balancer keeps the peer's address.** In front of the Agent
    plane sits a layer-4 load balancer with TLS passthrough: the client certificate in the handshake
    is admission, so it never terminates at the balancer. The balancer must preserve the peer's
    source address, by client-IP preservation or direct server return. The admission throttle, the
    aggregation of refusals and the affinity of clause 14 key on that address, and behind a balancer
    that rewrites it every Agent is one address, which a few refused hosts would put into back-off
    for the whole fleet. The Server reads no PROXY protocol header. The Operator plane may sit
    behind a balancer in the same way; every request carries its credential, so no session state is
    shared.

16. **Two consecutive versions run side by side.** Each schema change is expand-then-contract over
    two releases, so a rolling update runs version N and N+1 against one database. Which schema an
    instance accepts, and how it applies the migrations it lacks, is
    [ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)
    clause 22.

17. **The database's durability is the operator's.** The manual states synchronous replication as
    an operating requirement: a failover must lose no audit entry and no revocation. Failover
    itself is the operator's infrastructure; an instance only reconnects, and clause 3 decides
    what it does in between.

18. **The instance that takes over the lease of a dead process closes its audit chain.** A lease
    names the audit chain of the process that holds it
    ([ADR-draft-an-append-only-audit-record](draft-an-append-only-audit-record.md) clause 3). At
    every renewal an instance looks for a lease that lapsed. It takes such a lease over in one
    transaction that locks the lease, so one instance alone takes it, and in that transaction it
    appends `audit.chain_closed` to the chain the lease names, with `reason` `lease_lapsed` and
    `closed_by` the identifier of its own chain, writes that entry's hash and the time into the
    chain record, and deletes the lease. The store refuses an entry to a closed chain, so a process
    that was cut off cannot extend its chain afterwards. An instance whose own lease lapsed takes
    its new lease (clause 3) with a new chain and writes nothing more to the old one.

19. **On PostgreSQL every act on shared state is one transaction on the rows it names.** The
    certificate register, the revocation list, the Hosts, the enrolment window with its queue, the
    package floors, and the parts of a Configuration, a Package and a Deployment are shared by
    every instance, and no instance writes any of them whole from what it holds in memory.
    - Revoking or lifting one entry, registering or removing one certificate, writing one Host,
      binding one `instance_uid` to a Host, queueing, deciding or expiring one enrolment request,
      and opening or closing the window each change the rows they name and no other, in one
      transaction that carries the notification of clause 9
      ([ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)
      clauses 8 to 10 and 21).
    - An `instance_uid` is bound to at most one Host, by a unique constraint. A binding that meets
      another Host's fails, and the instance reads the binding that holds and decides by it.
    - The bounds of the register, the revocation list, the bootstrap entries and the enrolment
      queue are counted in the transaction that would pass them, so each holds for the fleet and
      not for one instance.
    - Admission reads the chain of a certificate to its root in the register, and the binding of an
      `instance_uid`, from the database, or from a cache that the notification of each change to
      them drops (clause 12).
    - An act that decides its write from what is stored reads it in the same transaction, under
      `SELECT … FOR UPDATE` on the row, and decides from what the database holds. A package floor
      is set only when its version is not below the stored one
      ([ADR-0028](0028-package-versions-high-water-mark-and-floor.md) clause 15), compared in the
      Server by SemVer precedence, as SQL does not order SemVer. A Configuration's revisions, a
      Package's entries and a Deployment's members are rows of their own: an act adds or removes
      the ones it names, and the revisions a Configuration keeps are reckoned in the transaction
      from the rows and from the assignments that pin them
      ([ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)
      clauses 2 and 21).

20. **A fleet that loses an instance comes back spread out.** The Agents of a lost instance —
    its connections cut, closed as it drains, or ended by a lapsed lease — reconnect as
    [ADR-draft-tls-1-3-two-planes-and-bounded-input](draft-tls-1-3-two-planes-and-bounded-input.md)
    clause 4 says: the first attempt drawn over `[0, 30 s)`, and each instance bounded to
    `max_new_connections_per_sec` new connections a second.
    - Thirty seconds is a third of G-20's 90 seconds, which leaves the rest for a lapsed lease and
      two heartbeats.
    - Of a fleet of 100 000 on three instances, the 33 000 Agents of a lost instance then arrive at
      about 1 100 a second across the two left, instead of within half a second.
    - The G-20 test sends the 3 300 Agents of one of three instances to the other two over 30
      seconds, about 56 a second each, below the bound of 200 each must absorb.

21. **A Gateway's pool keeps each downstream Agent on one instance while its connection lives.**
    - A Gateway's pool ([ADR-0015](0015-client-modes-and-the-gateway.md) clause 7) reaches the
      Agent plane through the balancer as any peer does, so its connections may land on different
      instances. A downstream Agent is assigned to one pool connection and stays on it while that
      connection lives, so every report of it reaches one instance, and the takeover of clause 8
      happens only when that connection ends. A downstream Agent therefore does not move between
      instances while its Gateway's connection stands, and the Server needs no rule of its own for
      a Gateway's connections. Under the affinity of clause 14 a Gateway's connections, which share
      its address, all land on one instance.
    - When the instance behind a pool connection is lost, that connection ends, and its downstream
      Agents stay connected to the Gateway. Each is placed anew at its next report, on a live pool
      connection or on one the pool opens for it, and is taken over there once (clause 8), so the
      Agents behind a Gateway reach another instance spread over one heartbeat interval, as they
      report. The pool opens a replacement connection when a report needs one; the Gateway's own
      Client connection reconnects as
      [ADR-draft-tls-1-3-two-planes-and-bounded-input](draft-tls-1-3-two-planes-and-bounded-input.md)
      clause 4 says.
    - Meanwhile the Gateway serves on what it holds: its revocation list, refreshed every 30
      seconds from whichever instance its fetch reaches and trusted for 300 seconds
      ([ADR-0015](0015-client-modes-and-the-gateway.md) clause 14), and its floors, fetched hourly
      ([ADR-0028](0028-package-versions-high-water-mark-and-floor.md) clause 16). The list's
      version is the same on every instance
      ([ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)
      clause 10), so a fetch that reaches another instance is answered `304` and the list stays
      young enough to trust.
    - A Gateway is not refused for sending all its pool connections from one address. The admission
      throttle counts failures only
      ([ADR-draft-admission-by-client-certificate-and-revocation](draft-admission-by-client-certificate-and-revocation.md)
      clause 19), and a Gateway's own certificate does not fail; the rate limits key on its host
      and on each `instance_uid`
      ([ADR-draft-tls-1-3-two-planes-and-bounded-input](draft-tls-1-3-two-planes-and-bounded-input.md)
      clause 28); the bound on new connections a second counts every connection alike, and a pool
      of at most `upstream_connections` (default 10) is a handful within it. A pool connection
      refused under that bound waits its `Retry-After` as any Client does.

**Out of scope:** the PostgreSQL schema and adapters themselves
([ADR-0029](0029-the-servers-state-and-its-stores.md)); the audit chains, but for closing the
chain of a process that died (clause 18), and their checkpoints
([ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance](draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance.md),
[ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token](draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token.md));
paging and filtering
([ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md));
sharing rate limits across instances; a routing table or instance-to-instance messages; several
databases or regions; the Agent plane's load balancer and the database cluster, which the operator
runs.

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
  this ADR removes. What must not run twice at once — signing the audit checkpoint, the sweep of
  unreferenced blobs and the staging cleanup, pruning the closed audit chains — runs under an
  advisory lock that any instance can take. Rejected.
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
  manual's source-address affinity is what keeps the limit as tight as on one instance.
- Negative / trade-offs: the one database and the one object store are what every instance stands
  on. G-20 holds only while they are made redundant, which is the operator's (clause 17).
- Negative / trade-offs: with SoftHSM2, the planned token, the CA keys reach the instances of one
  host only, so the instances give redundancy of processes there; redundancy of hosts needs a
  network HSM, or certificates issued outside the Server (clause 2).
- Negative / trade-offs: cloned machines that share an `instance_uid` are rekeyed when both reach
  one instance, and are only logged as a likely duplicate when they reach two (clause 8).
- Negative / trade-offs: a load balancer that rewrites the peer's address cannot be used in front
  of the Agent plane (clause 15).
- Positive: a floor set from a stale view cannot lower the stored one (clause 19), and a floor
  stored too low would still not lower a Client: each Client keeps the highest floor it verified
  ([ADR-0028](0028-package-versions-high-water-mark-and-floor.md) clause 17), so the harm of a
  floor served late is bounded by the Clients that never saw the higher one.
- Follow-ups: the operator manual's pages on replication, load balancing and rolling updates.

## Enforcement

Tests that will carry `Verifies:
ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it`, run
against the PostgreSQL that `FLEET_TEST_POSTGRES_URL` names, with two instances in one test process.
Where a token is needed, the two share one `cryptoki` context on one SoftHSM2 module, since PKCS#11
initialises a module once per process:

- A rollout on instance A reaches an Agent connected to instance B, and a revocation on A closes
  B's session of that certificate with `1008` and `revoked`.
- An enrolment approved on A is offered on B's enrolment connection.
- Every line A and B log carries the instance id of the one that wrote it, and when A takes
  over B's lapsed lease, A logs its own id and B's; a Server on the filesystem store logs no
  instance id.
- The `ETag` of the Gateways' revocation list that A answered is answered `304` by B, and a
  revocation on either moves it on both.
- When B's lease lapses, B's Agents read as disconnected through A's Operator plane; B ends its
  sessions and refuses TCP connections on both planes until it holds a new lease, then accepts
  them again, and with its renewals held back B stops serving no later than A reads its lease as
  lapsed.
- A Gateway whose pool reaches both A and B keeps the holder of each downstream Agent unchanged
  while the Agent's pool connection lives.
- When A stops, each Agent behind that Gateway that A held is taken over by B once, at its next
  report, and stays connected to the Gateway; the Gateway's revocation list fetched from B is
  answered `304` and stays trusted, and its floors are served on.
- During the 10 000-Agent failover below, a Gateway's ten pool connections from one address are
  refused by no admission throttle, and by the bound on new connections no more than any peer.
- A's Operator plane names B as the holder of an Agent B carries, with B's host name, and names
  none once B's lease has lapsed; `holder=<B>` lists exactly the Agents B holds.
- On `SIGTERM` an instance refuses new connections at once, answers a request in flight, closes its
  WebSocket sessions with `1001` spread over `drain_secs`, and exits once its presence, its
  chain's close and its lease's release are written; `drain_secs` outside 0 to 300 fails startup
  naming the key.
- After B's listening connection is dropped and restored, a revocation committed in between still
  ends B's session.
- A second instance on a database where the first runs with `[blobs] store = "fs"`, with a
  `[client_ca] key_file` or with an `[enrolment] bootstrap_ca_key_file`, fails startup naming the
  key, and so does an instance with either of them
  on a database where another instance holds a live lease; an instance that must be alone and loses
  its lock's connection logs it, ends its sessions and refuses every call; a second instance whose
  `[audit] key_label` differs from the first's, or that names none beside one that names a label,
  fails startup naming the key, and two instances without it both start.
- Two instances revoking two certificates at once leave both revoked, and two instances lifting
  and adding revocations at once leave the list both acts decided.
- A certificate renewed on A, whose predecessor is revoked later on A, is refused on B, whose
  register held neither the renewal nor the revocation before.
- Two instances binding one `instance_uid` to two Hosts at once leave one binding, and the second
  instance decides by it.
- Two instances queueing an enrolment request each keep both requests, and the queue's bound
  counts the requests of every instance.
- A floor set on A is served by B at once, and a floor below the stored one is refused on B though
  B's memory holds an older, lower floor.
- Two instances saving one Configuration at once keep both revisions; two instances uploading two
  entries of one Package keep both; two instances adding two members to one Deployment keep both.
- One hundred heartbeats of one Agent within one presence interval write its presence once.
- An Agent that reconnects from B to A is held by A, and B stops carrying it.
- An Agent no instance carries is in no instance's memory, and A's Operator plane lists it, with
  its record and presence, from the database; a Server on the filesystem store lists the same
  Agent from memory.
- Labels and an assignment set through A on an Agent B carries survive B's next write of that
  record while A's notification is held back from B, and B holds them once it arrives.
- A notification is never received before the change it announces can be read, and a change
  whose transaction rolls back sends none.
- A notification of a Configuration, a Deployment, a Package or a revocation drops the instance's
  cached entry, and the next use reads the database's.
- After B's listening connection is restored, B holds the labels set and the Agents taken in
  between, and an approval decided in between reaches B's enrolment connection; a notification of
  an unknown kind makes B re-read as after a lost connection.
- Two instances taking one Agent at once leave it carried by one of them, never by none.
- An Agent whose holder changes eleven times within a minute is logged once as a likely duplicate.
- On an orderly shutdown an instance writes the presence of its Agents and releases its lease, and
  another instance reads them as disconnected at once.
- An instance that must be alone, started within `lease_secs` of its predecessor's crash, takes the
  predecessor's lease over and starts.
- A fleet of 10 000 simulated Agents across three instances, one of them stopped without warning,
  is carried by the other two within `lease_secs` plus two heartbeat intervals, with one holder
  write per Agent taken over and no notification left unread once they are carried; a revocation
  committed during the takeover ends its session within `lease_renew_secs`, and no Agent of the
  stopped instance reconnects before the delay clause 20 drew for it. The test names its fleet
  size, its instance count and its timings, and fails when a bound is missed (G-20).
- The presence batches and notifications of 100 000 simulated Agents at the default heartbeat,
  about 3 300 reports a second, across three instances on one database, run for ten minutes with
  no presence batch written later than its interval after it was due and no notification left
  unread (G-20).
- `[database] lease_secs`, `lease_renew_secs` or `presence_interval_secs` with `store = "fs"` fails
  startup naming the key.
- When B stops without an orderly shutdown, A takes over B's lease once it lapsed and closes B's
  chain with `audit.chain_closed`, `reason` `lease_lapsed` and `closed_by` A's chain;
  `audit-verify` reports B's chain as closed, and an entry B appends to it afterwards is
  refused.

**Not mechanically decidable:** the operator's side of clauses 14, 15 and 17 — the balancer's
affinity and its preservation of the peer's address, the database's synchronous replication and
its failover. The Server cannot see them. The manual states each as an operating requirement, and
the load test above runs with a balancer that preserves the address.
