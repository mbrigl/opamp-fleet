# ADR-0045: Several Server instances share one PostgreSQL database, two versions run side by side, and a group is placed again when its Client Agent is bound or arrives elsewhere

- **Status:** 🟢 accepted
- **Date:** 2026-11-12
- **Deciders:** Markus Brigl
- **Applies to:** the in-memory fleet and its push in `crates/fleet-server/src/fleet.rs`, the sessions and their guard in `crates/fleet-server/src/transport.rs`, the ports, the PostgreSQL adapters and the change signals of the enrolment window and its queue in `crates/fleet-server/src/enrolment.rs` and of the certificate register, the revocation list and the Hosts in `crates/fleet-server/src/revocation.rs`, the per-instance tables of `crates/fleet-server/src/agent_rate.rs` and `crates/fleet-server/src/throttle.rs`, the package floors in `crates/fleet-server/src/floors.rs` and `crates/fleet-server/src/postgres/floors.rs`, the PostgreSQL adapters of Configurations, Deployments and Packages in `crates/fleet-server/src/postgres/`, the PostgreSQL adapters and their schema, the presence batches in `crates/fleet-server/src/instance.rs`, the WebSocket driver in `crates/opamp/src/client/ws.rs`, the audit port in `crates/fleet-server/src/audit.rs` and its PostgreSQL adapter where an instance closes the chain of a lapsed lease, the `[database]` section of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, `docs/ARCHITECTURE.md`, and the operator manual's pages on replication, load balancing and rolling updates
- **Supersedes:** [ADR-0025](0025-several-server-instances-share-one-postgresql-database-and-two-versions-run-side-by-side.md)

## Context

A Server process holds its fleet in memory (`AppState` in `crates/fleet-server/src/fleet.rs`): on
the filesystem store the whole fleet, loaded at startup, and on PostgreSQL the Agents it carries
(clause 8). Three in-process signals tell live sessions that something changed: the fleet's `push`
channel, a `watch::Sender<u64>` bumped by a rollout, a publication or a restart; the revocation
list's `subscribe`, which every WebSocket session's guard waits on to end itself when its
certificate is revoked ([ADR-0026](0026-admission-by-client-certificate-and-revocation.md) clause
32); and the enrolment queue's `subscribe`, which hands an approved certificate to the connection
that asked for it. On the filesystem, `connected` is what one process knows about its own
connections and is not persisted; on PostgreSQL it is persisted with the id of the instance that
holds the connection ([ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clauses 3 and 6).
The rate limits and the admission throttle are tables in memory
([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) clause 28, ADR-0026 clause 19). Duplicate
detection tells two connections of one `instance_uid` apart by a connection id that only this
process hands out.

One process alone is one point of failure and one machine's worth of connections. The
specification's goal G-20 asks that a fleet of up to about 100 000 Agents be served on when one of
several Server instances stops without warning: its Agents connected to another instance within a
bound, and a revocation ending its sessions on every instance within another. The Strategy has
several instances serve one fleet on the shared stores, any of them taking over the Agents of one
that is lost: active-active, sharing state only through PostgreSQL and the object store.
[ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
(the PostgreSQL store) puts every store behind the ports into one database, in whose schema
every Server is an instance with a lease (clause 23);
[ADR-0029](0029-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under.md)
(audit chains per instance) gives each writing instance its own chain; and
[ADR-0031](0031-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
(paging) lets the Operator plane read the fleet a page at a time from the database. This ADR is what
turns those into several instances. A CA key the Server holds is a file, which one instance alone
can hold (clause 2); CA keys every instance can reach are a decision of their own.

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
   `[database] store = "postgres"`
   ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md))
   a Server takes an instance id at startup — a random UUID that is not persisted — and a lease in
   the database before it opens either listener, and keeps its listeners open only while it holds a
   lease it can prove (clause 3). Every line the instance logs carries its instance id, and the
   take, the loss, the release and the takeover of a lease are logged with every instance id
   involved, so the logs of several instances say which one acted. How many instances run is
   whatever the operator starts. With `store = "fs"` there is one instance, no lease, and no
   instance id in the log.

2. **Several instances need shared blobs and no CA key in a file.** An instance whose
   `[blobs] store` is `"fs"`, or whose client CA's or bootstrap CA's key is a file, must be alone.
   It takes, besides its lease, an exclusive lock that says it is the only instance, a session-level
   advisory lock held on a connection of its own.
   - A second instance on the same database then fails startup, naming the key. So does an
     instance that must be alone when another live lease exists already, naming the key and the
     number of live leases. A lease records whether its holder must be alone; one that does, and
     whose exclusive lock is free, belongs to a process that is gone. The next instance that takes
     a lease takes such a lease over as clause 18 does instead of failing, whether it must be
     alone or not, and closes its chain with `reason` `holder_gone`, since the lease has not lapsed
     ([ADR-0029](0029-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under.md)
     clause 3).
   - When the lock's connection ends while the instance runs, the lock is gone with it. The
     instance logs an error naming the database, ends every session it carries and refuses every
     call from then on, so it never writes beside a second instance; it takes the lock again only
     when it is started again.

   Where several instances serve one database, none holds a CA key in a file. With `store = "s3"`
   every instance uses the same bucket and prefix, and every sweep of unreferenced blobs and every
   staging cleanup run under an advisory lock, so one instance does them at a time, and an upload
   takes it shared in the transaction that writes its entry;
   [ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
   clause 14 decides it for the blob store. An instance on PostgreSQL and S3 keeps nothing on its
   own disk (that ADR's clause 1): it reads its configuration, its TLS files and its secret files,
   all of them read-only, and with no CA key on the Server, any instance can take its place. The
   Server's image names no data directory, so such an instance runs from it as it stands
   ([ADR-0039](0039-releases-attestations-and-the-server-image.md) clause 16).

3. **The lease.** Each instance renews its lease every `[database] lease_renew_secs` (default 10)
   and it lapses `[database] lease_secs` (default 30, at least three renewals) after the last
   renewal. These two keys and `[database] presence_interval_secs`
   ([ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 4) apply to `store = "postgres"`
   alone; with `store = "fs"` each fails startup, naming it. An instance measures its own lease from
   the moment it sent the last renewal the database confirmed, which is no later than the database's
   own time of that renewal, so it deems its lease lapsed no later than the database and every other
   instance do. It measures on a clock that counts the time its host was suspended, on Linux
   `CLOCK_BOOTTIME`, so a suspend lapses its lease for it as it does for the database. It reads that
   clock at least every 100 milliseconds, and before it answers each request on either plane and
   each message of an Agent: while its lease is lapsed by that clock it answers `503` and nothing
   else, and an instance that runs again after a suspend serves nothing until it holds a new lease.
   A virtual machine that its hypervisor pauses without the guest's clocks counting the pause is
   outside this: it serves on until its next renewal, at most `lease_renew_secs` later, finds its
   lease lapsed. An instance that cannot renew before its own lease lapses ends every session it
   carries and closes both listeners. Once the database answers again it takes a new lease, under a
   new instance id, and opens them again. It never serves on a lease it cannot prove, because it
   could not learn of a revocation. A closed listener is what a load balancer's or an orchestrator's
   probe sees: the image's probe is a TCP connection to `4320` for liveness and readiness
   ([ADR-0039](0039-releases-attestations-and-the-server-image.md) clause 17), and an instance
   without a lease is then neither, so no Agent is sent to it.

   An orderly shutdown, on `SIGTERM` among others, drains the instance. It closes both listeners at
   once, so no new connection reaches it, and answers the requests already in flight on either
   plane. It ends the WebSocket sessions it carries with close code `1001` (going away), each at an
   instant drawn uniformly over `drain_secs` (default 20, from 0 to 300; 0 ends them all at once),
   so its Agents reconnect to the other instances without arriving all at once. Then it writes the
   presence of its Agents, closes its audit chain
   ([ADR-0029](0029-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under.md)
   clause 3) and releases its lease. The default leaves the rest of an orchestrator's usual grace
   period of 30 seconds for those writes. `drain_secs` applies on the filesystem store as well,
   where the one Server drains the same way.

4. **[ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 3 decides it: What a report or
   an operator decided persists; so does where an Agent is connected.**

5. **[ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md)
   clause 4 decides it: The write discipline lives above the port,
   so no backend can get it wrong.** On PostgreSQL that clause writes presence in batches. A batch
   written later than its interval after it was due is logged as a warning naming how late it was,
   so that a database that cannot keep up with the fleet's presence shows before an Agent reads as
   stale.

6. **[ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md)
   clause 6 decides it: `connected` says a connection carrying the
   Agent is open — nothing more.**

7. **[ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md)
   clause 8 decides it: The budget is three heartbeat intervals,
   and on PostgreSQL the presence interval besides.**

8. **On PostgreSQL an instance holds only the Agents it carries, and the newest connection takes an
   Agent.** On the filesystem store the one Server keeps the whole fleet in memory
   ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
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
   the other stops carrying the Agent and takes it back at the Agent's next report on it. A presence
   batch names its instance as the holder anew only of a row that names no holder or an instance
   without a live lease ([ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 2), and,
   where this instance was answered a count of takes for the Agent, only while the row's count is
   still that one: it never names its instance over a live holder, nor, knowing its count, over a
   take counted since its own. A take whose answer was lost leaves the instance with no count for
   the Agent, but for the shutdown flush: that keeps the count the instance was answered before the
   take, or 0 where it created the record, and writes the record under it
   ([ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 2). An Agent with neither is not
   flushed; its row keeps what was last written of it, and the next instance to carry it finds a gap
   in its sequence numbers and asks for a full report. A report takes nothing. A shutdown flush on
   PostgreSQL writes no record whose stored sequence number is higher than the one it writes,
   whichever holder the row names: a newer report another instance wrote meanwhile stays, also where
   the flushing instance still holds the row while it stops, its lease live, or the record was
   forgotten and created again with a count that started over. An Agent that started again counts
   its sequence numbers from the start, so a flush with a higher one can still write over the report
   of the instance that carries it now. That instance, until it is answered a count of takes for the
   Agent, writes the record at every report of it and not only where what a report decides changed,
   so a row such a flush made stale holds the carrier's record again at the Agent's next report. The
   carrier writes so while it has no count: its next presence batch answers one where no other
   instance with a live lease holds the row, and otherwise the first batch after that lease is gone,
   or the take at the Agent's next connect does; from then on a report that changes nothing durable
   writes nothing again. Where none of these comes, as with a duplicate of the Agent live on two
   live instances, the other of which holds the row, every report of it keeps writing. A flush
   refused for a higher stored sequence number leaves its own Agent's newer sequence number
   unwritten, and the next report to the instance that carries it, or the full report a gap asks
   for, writes it. The port of [ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 2 has
   `write_report_unless_held_elsewhere` write, on a shared store, only a record whose stored
   sequence number is not higher than the record's; a store whose one Server is its only writer
   writes as `write_report` does. Before it writes the disconnect of an Agent whose take lost its
   answer it reads the row's holder and count of takes through the port's `takes(uids)`, under a
   share lock on the row that waits for a take still in flight, and writes the disconnect under the
   count it read where the row names it and it took the Agent no more since the read. Duplicate
   detection and the rekeying of a newcomer stay within one instance, and take for a duplicate only
   an Agent alive on a connection of another peer. An Agent that comes back on a new connection of
   the host whose certificate its old connection presented is taken by the newest connection, and
   the older connection's reports, its goodbye among them, no longer take it back or end it, nor is
   a queued restart sent on that connection, while a newer one carries it; a connection that
   presented no host has no such peer. A newer connection that has carried no report of the Agent
   for one heartbeat interval, the one this Server offers or 30 seconds without an offer, carries it
   no more: the next report on an older connection of the host takes the Agent back, its goodbye
   ends it there, and it is asked for what it is due there. A Gateway's pool moves an Agent between
   connections that all stay open, so this is how a Gateway's Agent follows it. One host is one
   peer, so two Agents of one `instance_uid` on one host, a clone with the host's certificate, share
   one record. An Agent whose holder changes more than ten times in a minute is logged as a likely
   duplicate. An Agent's labels stay in its record
   ([ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 21), and there is no other copy.
   A label change, a rollout's assignments and a restart request made through any instance are
   written to their own columns of the record in the database and announced (clause 9). The instance
   that delivers a queued restart clears it before the send, and puts it back when the command was
   not sent: a WebSocket frame the connection does not take, or a plain-HTTP answer past the size
   limit. It does so on a lease that lapsed meanwhile too, since setting the restart again only
   restores what the Operator asked for. A put-back the store does not take is kept in the
   instance's memory and tried again, at intervals that grow to a minute, until it holds; a process
   that ends before then loses the restart. A request or a delivery of a restart for that Agent
   through this instance meanwhile drops it, as two requests before a delivery are one restart, and
   so does the take-up of a request through another instance, which that instance announces (clause
   9). A delivery through another instance is not announced; the request it delivered was. After a
   missed notification a put-back is dropped where the record shows a restart queued. The Agent's
   forget through this instance, or through another while this one holds the Agent, drops it too. A
   re-key of the Agent through this instance moves it to the new identity; the re-key of a duplicate
   newcomer moves no row and leaves it. Each try is decided under the lock that requests, deliveries
   and forgets of this instance take, so none of them is overtaken by a try. Three cases are not
   seen. A forget through another instance after this one stopped holding the Agent is seen only as
   its record gone when the put-back is tried, so an Agent that came back under the same identity
   before that try is restarted for it. A request through another instance delivered before this
   instance took up its announcement is not seen either, and the Managed Process is then restarted
   once more. A re-key of the Agent through another instance while this one tries is seen only as
   its record gone too, so the restart is not tried again; the instance logs a warning naming the
   Agent whenever a put-back finds its record gone. So on a WebSocket the restart is delivered at
   least once wherever the Server sees the send fail, but for such a re-key. A frame lost after the
   connection took it is not seen. On plain HTTP the answer goes out with the response's head, so a
   poll whose answer is lost is not seen either, and the Operator requests the restart again. A
   command that reached the Agent although its send failed is sent again and restarts the Managed
   Process twice, and the manual says so. The instance that carries the Agent writes only what a
   report decides ([ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 2), so its next
   write keeps them whether or not the notification has reached it, and it takes them into the
   record it holds when the notification arrives. Where an effective description (ADR-0030 clause
   18) needs the labels of a Client Agent this instance does not carry, or a Host's subject values,
   the instance reads them from the database. It finds that Client Agent among the Agents the host
   register binds to the Host. The register binds none to a Gateway's Host, so behind a Gateway the
   instance finds it by a read of the Agents that arrived over that Host, through the port's
   `query_page` filtered on that Host
   ([ADR-0031](0031-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
   clause 9), so an Agent behind a Gateway inherits the labels of a Client Agent another instance
   carries in the memory of its own instance too. An instance places such a group when it places one
   of its Agents, again when the labels of the Client Agent it placed the group under change, again
   when a notice that a binding of the group's Host changed reaches it, again when a notice that a
   Client Agent first arrived over the group's Gateway reaches it, and every group after a missed
   notification. So a fellow Agent inherits the labels of a Client Agent that another instance binds
   to the Host, or takes behind the Gateway, in this instance's memory too, once the notice of it
   arrives. What the Operator plane shows is read from the database and inherits them.

9. **Every change that concerns a live session is one `NOTIFY` to every instance.** The writing
   instance commits the change and, in the same transaction, sends one `NOTIFY` on one channel,
   naming the kind and the identifier, and the instance and the call that sent it, and nothing else:
   an assignment changed, a Package or Deployment published, a restart requested, a revocation added
   or lifted, a certificate registered or removed, a Host written or an `instance_uid` bound to it,
   an enrolment request queued or decided, the enrolment window opened or closed, a package floor
   set or removed, an Agent's labels set, an Agent forgotten, an Agent taken by another instance, a
   Client Agent first arrived over a Gateway's Host.
   Three writes announce their change right after its commit instead, on the same connection, in a
   transaction of their own that does not wait for its WAL flush: a binding a report makes, a Host
   row made or whose valid until moves forward, by a report, an issuance or a prune, and the record
   of a Client Agent that first arrives over a Gateway's Host, named by that Host. PostgreSQL lets
   one transaction that sent a `NOTIFY` commit at a time, database-wide, and holds that turn until
   the commit's flush ends, so a fleet whose new Agents each announced in their writing commit would
   connect no faster than one flush apiece. The first two widen what a host speaks for or how long
   it does, which the database decides at every binding (clause 19), and the third only has the
   instances place the Gateway's groups again (clause 8), so an instance that hears of them late
   admits nothing it should not. Every write that narrows trust, a revocation added, a
   binding released or moved, every binding of a Host released at once, a Gateway's mark set or
   taken away, a Host forgotten, and every register entry keep their `NOTIFY` in their transaction.
   A notice sent after its commit that fails is logged and sent again a few times, and the call that
   wrote the change answers as it would without it; one never sent is taken up at the next lease
   renewal (clause 10), but for the placement it asks for: a group a binding's or an arrival's lost
   notice was to place again (clause 8) is placed again at the next event that places it. Each
   instance listens on a connection of its own and acts on the sessions it
   holds: it composes a new offer, ends a revoked session (clause 11), offers an approved
   certificate, closes its enrolment connections, takes up an Agent's labels, or drops the Agent.
   There is no table of which instance holds which session, and no message addressed to one
   instance.

10. **A missed notification counts as every notification.** `NOTIFY` reaches only the sessions
    listening when it is committed. When an instance's listening connection is lost and
    re-established, it acts as if everything had changed: it drops its caches, re-reads the
    revocation list and checks every session against it, re-reads the record and the holder of every
    Agent it carries, stops carrying each one another instance holds, and re-reads the package
    floors, the enrolment window and the requests of its enrolment connections. A notification of a
    kind the instance does not know, as a newer version sends while two versions run side by side
    (clause 16), counts as a missed one. It does the same when a lease renewal finds that the
    revocation list's version in the database
    ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
    clause 10) is newer than the one it holds. At every lease renewal it reads the ledger again
    besides, the Hosts and their bindings among it, so a notice sent after its commit that never
    arrived, as when its instance ended between the two (clause 9), is taken up within one renewal.
    That reads the ledger, not the groups: a group such a notice was to place again (clause 8) waits
    for the next event that places it, and a lost notice of an arrival over a Gateway's Host is not
    taken up at a renewal at all.
    A write whose answer is lost counts as a notification of its own. Where a call that sent a
    `NOTIFY` fails, on a lost connection, its bound or an error of the database, the instance cannot
    tell whether it committed: it reads again what each notification of the call names, as it would
    on hearing it, and hears once more the notifications that call sent, told by the call they name
    from any other of its own, so a commit that lands after that read is taken up when it lands. A
    notice that would follow its commit (clause 9) is read again at once where its call fails, as
    the others are, but not heard once more, as none may be sent: a commit of it that lands after
    that read is taken up by the instance at its next report of the Agent, which the database
    answers, and by every instance at its next lease renewal, but for the placement it asks for
    (clause 9). A version that does not know the call
    a notification names reads past it. A re-key reads back where the row is before it answers:
    under a share lock on the old row, which waits for the transaction that moves it, and then the
    new one. Moved, the re-key stands. Where the read back fails, the instance keeps the pair, and
    the next read of the old identity reads it back: at a report, at a notification of it, and
    wherever the instance finds its row gone. A read back at a notification that fails is tried
    again until it holds, and until then the instance keeps the Agent under its old identity. Where
    the move landed, the instance moves what it holds to the new identity, keeps the Agent's
    session, and assigns the new identity at the next report under the old one; a session that ended
    meanwhile has its disconnect written to the row the move landed in. While it is not known, a
    report under the old identity of which the instance holds no record is answered `Unavailable`
    and creates none.

11. **Clause 32 of [ADR-0026](0026-admission-by-client-certificate-and-revocation.md) decides it: A
    revocation ends the sessions it concerns, and only those, on every instance.**

12. **Caches are read again, not updated.** Each instance may hold Configurations, Deployments,
    Packages, the package floors, the revocation list, the certificate register and the Hosts in
    memory, as the ports allow
    ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
    clause 2). A notification of their kind has the instance read again at once what it
    concerns, the whole ledger for the revocation list, the register and the Hosts; nothing held
    is updated from the notification itself.

13. **On PostgreSQL the Operator plane reads the Agents from the database.** What it shows of an
    Agent — its record, its presence, its staleness — and the Agents each Host lists come from the
    database, never from the memory of the instance that serves the request. The Hosts, the
    certificates, the revocations and the pending enrolments it lists are the instance's copies,
    read again on each notification of their kind and after a missed one (clauses 9, 10 and 12).
    The four lists that grow with the fleet are answered a page at a time where a request asks for
    one
    ([ADR-0031](0031-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)).
    - An Agent's view names the instance that holds it: `holder`, the instance id its presence
      names, and `holder_host`, the host name that instance's lease records, as read from the
      database. Both are empty for an Agent whose presence names no instance with a live lease.
      The Agent's detail shows them, and the list of Agents filters by `holder` (that ADR's
      clause 6). Each lease records the host name of its instance for this.

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

16. **Two consecutive versions run side by side.** A rolling update replaces the instances one by
    one, so versions N and N+1 serve one database at once.
    - Each schema change is expand-then-contract over two releases. The migrations of N+1 name N
      as the oldest Server version that understands the schema they leave, so the instances of N
      that still run serve on it, and what N+1 stops using is removed by N+2 at the earliest
      ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
      clause 22).
    - An instance fails its start, naming both versions and changing nothing, on a schema that
      names a version newer than its own as the oldest that understands it.
    - Each lease records the release of its instance. Every lease taken before the migration that
      adds this record is one of 0.8.0, the release in which several instances first share a
      database, and an instance of 0.8.0 writes no release, so the record names 0.8.0 by default.
    - An instance also fails its start, naming its own release and the older one and changing
      nothing, while a live lease names a release older than the oldest Server version the schema
      names once the instance's own migrations are applied: that lease's instance would serve on a
      schema it does not understand. A lease that clause 2 takes over, whose process is gone,
      counts as none. With no such lease, as when every instance of an older release has stopped,
      the instance applies every migration it lacks, so a Server on PostgreSQL may skip releases
      while nothing older runs on its database.
    - An instance checks the schema and the live leases under the migrations' advisory lock
      ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
      clause 22) before it migrates. The transaction that takes its lease takes that lock too and
      checks the schema again, at its start and whenever it takes a new lease after its own
      lapsed (clause 3). So of two instances that start at once, the later sees the lease or the
      schema of the earlier. On a schema that names a newer oldest Server version than its own,
      a start fails. An instance that finds one at a new lease stops as at a failed start, naming
      both versions. Its sessions ended with the lease it lost, and its Agents read as
      disconnected since that lease lapsed. Holding no lease, it writes nothing more, and it
      exits with a failure.
    - Instances of N and N+1 tell each other of their changes as clause 9 says; a kind N does not
      know counts as a missed notification (clause 10).
    - The one exception is the release in which several instances first share a database. The
      release before it holds no lease, and the migrations of that release name that release as the
      oldest version, so a Server of the release before is refused, and an instance that shares the
      database neither migrates nor starts while another Server holds the database alone, as a
      Server of 0.7.1 does for its whole run (that ADR's clause 22). That upgrade stops every Server
      of the release before first.

    How an instance applies the migrations it lacks is that ADR's clause 22.

17. **The database's durability is the operator's.** The manual states synchronous replication as
    an operating requirement: a failover must lose no audit entry and no revocation. Failover
    itself is the operator's infrastructure; an instance only reconnects, and clause 3 decides
    what it does in between.

18. **The instance that takes over the lease of a dead process closes its audit chain.** A lease
    names the audit chain of the process that holds it
    ([ADR-0028](0028-an-append-only-audit-record-whose-failed-writes-on-postgresql-are-kept-and-tried-again.md)
    clause 3). At every renewal an instance looks for a lease that lapsed. It takes such a lease
    over in one transaction that locks the lease, so one instance alone takes it, and in that
    transaction it appends `audit.chain_closed` to the chain the lease names, with `reason`
    `lease_lapsed` (or `holder_gone` for a lease clause 2 takes over before it lapsed) and
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
      transaction that carries the notification of clause 9, or that it follows for a binding a
      report makes and a Host row made or moved forward by a report, an issuance or a prune
      ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
      clauses 8 to 10 and 21). An approval registers its certificate, writes the Host it mints
      and decides its request in one such transaction
      ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
      clause 9). An instance of 0.8.0 reads no rejected key
      ([ADR-0027](0027-bootstrap-certificates-and-enrolment-and-a-rejected-key-stays-refused-beyond-its-window.md)
      clause 16), so through it a rejected key waits again and may be approved, until the last
      instance of 0.8.0 is replaced.
    - An `instance_uid` is bound to at most one Host, by a unique constraint. A binding that meets
      another Host's fails, and the instance reads the binding that holds and decides by it.
    - The bounds of the register, the revocation list, the bootstrap entries and the enrolment
      queue are counted in the transaction that would pass them, so each holds for the fleet and
      not for one instance.
    - The ceiling on the Agents' records, `max_agents`, is counted from one row in the statement
      that creates a record
      ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
      clause 22), so it holds for the fleet too. An instance of 0.8.0 counts under a lock of its
      own, so while one runs beside the instances of this release the ceiling may be passed by the
      records created at the same moment.
    - Admission reads the chain of a certificate to its root in the register, and the binding of an
      `instance_uid`, from the database, or from a cache that the notification of each change to
      them drops (clause 12).
    - An act that decides its write from what is stored reads it in the same transaction, under
      `SELECT … FOR UPDATE` on the row, and decides from what the database holds. A package floor
      is set only when its version is not below the stored one
      ([ADR-0035](0035-package-versions-high-water-mark-and-floor.md) clause 15), compared in the
      Server by SemVer precedence, as SQL does not order SemVer. A Configuration's revisions, a
      Package's entries and a Deployment's members are rows of their own: an act adds or removes
      the ones it names, and the revisions a Configuration keeps are reckoned in the transaction
      from the rows and from the assignments that pin them
      ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
      clauses 2 and 21).
    - A Gateway's mark, set or taken away, is written only for a host the database knows, by its
      Host row or a register entry naming it, decided in the transaction that writes it, and is
      refused otherwise, whatever the instance holds in memory
      ([ADR-0030](0030-the-fleet-record-of-agents-and-hosts.md) clause 16). So a Host another
      instance wrote is marked before this one hears of it, and a host the database no longer
      knows is not marked from a stale copy. An instance that held no copy of the Host reads the
      ledger again before it answers, so it holds the Host as stored. An instance of 0.8.0 still
      decides from its memory and writes the mark without the database's check, so beside the
      instances of this release it may refuse a Host it has not heard of, and may mark from its
      stale copy a host the database has forgotten, whose Gateway row then stays until its mark is
      taken away; both end with the last instance of 0.8.0.
    - A rollout holds the row of every Configuration, Package and Deployment its assignments pin
      `FOR KEY SHARE` until it commits, and fails when one is not stored, so it never writes a pin
      on what another instance deleted. The deletion of a Configuration, a Package or a Deployment,
      and every write to a Package's entries, holds its row `FOR UPDATE` and counts the stored
      assignments that pin it, so it waits for such a rollout and is refused once it committed.
    - An act that adds a Configuration or a Package to a Deployment holds the member's row
      `FOR KEY SHARE` and is refused when it is not stored. The deletion of a Configuration or a
      Package takes it out of every Deployment that holds it in the transaction that deletes it.
      So among the instances of this release no Deployment names a member that another instance
      deleted, and no crash leaves one. An instance of 0.8.0 beside them still adds without that
      read and deletes in two transactions, so until the last one is replaced a Deployment may
      keep a member deleted meanwhile; it stays until it is taken out of the Deployment.

20. **A fleet that loses an instance comes back spread out.** The Agents of a lost instance — its
    connections cut, closed as it drains, or ended by a lapsed lease — reconnect as
    [ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) clause 4 says: the first attempt drawn
    over `[0, 30 s)`, and each instance bounded to `max_new_connections_per_sec` new connections a
    second.
    - Thirty seconds is a third of G-20's 90 seconds, a lease time and two heartbeat intervals at
      the defaults, which leaves the rest for a lapsed lease and two heartbeats.
    - Of a fleet of 100 000 on three instances, the 33 000 Agents of a lost instance then arrive at
      about 1 100 a second across the two left, instead of within half a second.
    - The G-20 test sends the 3 300 Agents of one of three instances to the other two over 30
      seconds, about 56 a second each, below the bound of 200 each must absorb.
    - The WebSocket driver of `crates/opamp` logs the delay it drew when a connection is lost, and
      may connect from a local address the application names. A Client names none; the G-20 test
      names one per simulated host, so that a balancer keyed on the peer's address tells the hosts
      apart and the test sees that no Agent reconnected before its delay.

21. **A Gateway's pool keeps each downstream Agent on one instance while its connection lives.**
    - A Gateway's pool ([ADR-0016](0016-client-modes-and-the-gateway.md) clause 7) reaches the
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
      Client connection reconnects as [ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md)
      clause 4 says.
    - Meanwhile the Gateway serves on what it holds: its revocation list, refreshed every 30
      seconds from whichever instance its fetch reaches and trusted for 300 seconds
      ([ADR-0016](0016-client-modes-and-the-gateway.md) clause 14), and its floors, fetched hourly
      ([ADR-0035](0035-package-versions-high-water-mark-and-floor.md) clause 16). The list's
      version is the same on every instance
      ([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
      clause 10), so a fetch that reaches another instance is answered `304` and the list stays
      young enough to trust.
    - A Gateway is not refused for sending all its pool connections from one address. The admission
      throttle counts failures only
      ([ADR-0026](0026-admission-by-client-certificate-and-revocation.md) clause 19), and a
      Gateway's own certificate does not fail; the rate limits key on its host and on each
      `instance_uid` ([ADR-0012](0012-tls-1-3-two-planes-and-bounded-input.md) clause 28); the bound
      on new connections a second counts every connection alike, and a pool of at most
      `upstream_connections` (default 10) is a handful within it. A pool connection refused under
      that bound waits its `Retry-After` as any Client does.

**Out of scope:** the PostgreSQL schema and adapters themselves
([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md));
the audit chains, but for closing the chain of a process that died (clause 18)
([ADR-0029](0029-the-audit-record-is-one-hash-chain-per-lease-and-names-the-lease-each-decision-was-taken-under.md)),
and their checkpoints, a decision of their own; paging and filtering
([ADR-0031](0031-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md));
CA keys that every instance can reach; sharing rate limits across instances; a routing table or
instance-to-instance messages; several databases or regions; the Agent plane's load balancer and the
database cluster, which the operator runs.

## Alternatives considered

- **A kept put-back follows a re-key through another instance.** The instance that keeps it cannot
  learn the new identity: the store records no move, a notice names the old identity alone
  (clause 9), and only the instance that made a re-key holds both. A record of the move, a notice
  naming both, or the put-back on the Agent's row would each need a migration or a wider notice.
  Rejected; the case stays not seen and is logged (clause 8).
- **Leave a group to its next placement** (ADR-0025 as accepted). A fellow Agent then inherits a
  Client Agent bound or taken through another instance only once the group is placed again for a
  reason of its own: a label change, a report that changes, a missed notification. Its effective
  description in that instance's memory lags behind the fleet view for as long, and the manual had
  to say so under "Labels on several instances". Rejected now that the notices exist to end it.
- **A routing of the Client Agent's labels in the notice.** The notice would carry the labels, so
  no read follows it. It makes the notice the carrier of data, which clause 9 keeps out of it: the
  notice names a kind and an identifier, and what changed is read. Rejected.

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
  this ADR removes. What must not run twice at once — the sweep of unreferenced blobs and the
  staging cleanup, pruning the closed audit chains — runs under an
  advisory lock that any instance can take. Rejected.
- **Keep serving through a database outage.** An instance would admit with the revocation list it
  last saw and could not learn of a new revocation. Rejected: clause 3 fails closed once the lease
  lapses.
- **Duplicate detection across instances.** It would rekey an Agent that only reconnected to
  another instance after a lost connection, before the old instance noticed. Rejected; the newest
  connection takes the Agent.
- **An update replaces every instance: the instances of the old version stop before the first of
  the new one starts.** Simpler, since no instance ever meets a record or a notification of the
  other version, but an update stops the whole fleet's Server for as long as the new version takes
  to start. Rejected; clause 16 lets two consecutive versions serve one database side by side.
- **No instance holds a CA key on the Server, rather than in a file.** While a file is the only
  form of a CA key the two rules are one; the wider rule would also forbid a key every instance
  reaches without holding it in a file. Rejected; clause 2 forbids the file.
- **A notification drops the cached entry, and its next use reads it again.** The revocation list
  ends the sessions it concerns as it changes
  ([ADR-0026](0026-admission-by-client-certificate-and-revocation.md) clause 32); a list that is
  read only on its next use would leave a revoked session open until some admission or request
  reads it. Rejected; clause 12 reads again at once what a notification concerns.

## Sources / Prior art

- PostgreSQL documentation: [`NOTIFY`](https://www.postgresql.org/docs/current/sql-notify.html)
  (delivered on commit, only to sessions listening at that moment, payload below 8000 bytes) and
  [advisory locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS).
- [Patroni](https://patroni.readthedocs.io/) and [CloudNativePG](https://cloudnative-pg.io/) for
  synchronous replication and failover run beside the application.
- The code as it stands: `AppState`, its `push` channel and the duplicate detection in
  `crates/fleet-server/src/fleet.rs`; the session guard in `crates/fleet-server/src/transport.rs`;
  `subscribe` in `crates/fleet-server/src/revocation.rs` and `crates/fleet-server/src/enrolment.rs`.

## Consequences

- Positive: G-20. Losing an instance loses its sessions, whose Agents reconnect to another, spread
  out; the fleet is served on. Capacity grows by adding instances.
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
- Negative / trade-offs: a Server that holds a CA key in a file runs alone. Several instances
  serve a fleet whose certificates are issued outside the Server, or by keys every instance
  reaches, which are a decision of their own; a Server with a CA key in a file gains a standby at
  most: an instance its service manager starts again until the database
  has ended the session of the one it replaces, whose lease it then takes over (clause 2).
- Negative / trade-offs: every schema change takes two releases, and an instance does not start
  while a lease is live whose release is older than the oldest Server version its migrations leave,
  so an upgrade that skips a release waits until those leases are gone (clause 16). The upgrade onto
  the first release of several instances still stops every Server.
- Negative / trade-offs: cloned machines that share an `instance_uid` are rekeyed when both reach
  one instance, and are only logged as a likely duplicate when they reach two (clause 8).
- Negative / trade-offs: a load balancer that rewrites the peer's address cannot be used in front
  of the Agent plane (clause 15).
- Positive: a floor set from a stale view cannot lower the stored one (clause 19), and a floor
  stored too low would still not lower a Client: each Client keeps the highest floor it verified
  ([ADR-0035](0035-package-versions-high-water-mark-and-floor.md) clause 17), so the harm of a
  floor served late is bounded by the Clients that never saw the higher one.
- Follow-ups: the operator manual's pages on replication, load balancing and rolling updates; CA
  keys every instance can reach.

## Enforcement

Tests that carry `Verifies: ADR-0025` run against the PostgreSQL that the `FLEET_TEST_POSTGRES_*`
variables name
([ADR-0023](0023-the-servers-state-and-its-stores-and-an-approval-records-its-certificate-with-its-decision.md)
clause 25), with two instances in one test process, or three where a bullet says so. They decide:

- A rollout on instance A reaches an Agent connected to instance B, and a revocation on A closes
  B's session of that certificate with `1008` and `revoked`.
- An enrolment approved on A is offered on B's enrolment connection.
- A fellow Agent B placed before A bound the Host's Client Agent inherits that Client Agent's
  labels in B's memory once the binding's notice reaches B; and a fellow Agent behind a Gateway
  that B placed inherits the labels of a Client Agent that A takes behind the same Gateway
  afterwards, once that notice reaches B (clauses 8 and 9). A Client Agent that first arrives over
  a Gateway's Host is announced after its commit.
- Every line A and B log carries the instance id of the one that wrote it, and when A takes
  over B's lapsed lease, A logs its own id and B's; a Server on the filesystem store logs no
  instance id.
- The `ETag` of the Gateways' revocation list that A answered is answered `304` by B, and a
  revocation on either moves it on both.
- When B's lease lapses, B's Agents read as disconnected through A's Operator plane; B ends its
  sessions and refuses TCP connections on both planes until it holds a new lease, then accepts
  them again, and with its renewals held back B stops serving no later than A reads its lease as
  lapsed.
- An instance whose clock jumps past its lease's lapse while it was not running answers its next
  request `503`, closes both listeners, and opens them again under a new lease; a message its
  open session carries then is not answered. A request on either plane and an Agent's message are
  answered `503` while the lease is lapsed by the instance's clock, though no listener is closed
  yet, and nothing is pushed then: a restart requested meanwhile stays queued.
- A queued restart whose command a WebSocket connection does not take, or whose plain-HTTP answer
  is past the size limit, stays queued, and the Agent's next connection is sent it; a put-back the
  store refuses is tried again, at growing intervals, until it holds.
- A Gateway whose pool reaches both A and B keeps the holder of each downstream Agent unchanged
  while the Agent's pool connection lives.
- When A stops, each Agent behind that Gateway that A held is taken over by B once, at its next
  report, and stays connected to the Gateway; the Gateway's revocation list fetched from B is
  answered `304` and stays trusted, and its floors are served on.
- During the 10 000-Agent failover below, a Gateway's ten pool connections from one address are
  refused by no admission throttle, and by the bound on new connections no more than any peer.
- A's Operator plane names B as the holder of an Agent B carries, with B's host name, and names
  none once B's lease has lapsed; `holder=<B>` lists exactly the Agents B holds.
- An Agent behind a Gateway that B carries inherits in B's memory the labels of its Client Agent,
  which A carries.
- On `SIGTERM` an instance refuses new connections at once, answers a request in flight, closes its
  WebSocket sessions with `1001` spread over `drain_secs`, and exits once its presence, its
  chain's close and its lease's release are written; `drain_secs` outside 0 to 300 fails startup
  naming the key.
- After B's listening connection is dropped and restored, a revocation committed in between still
  ends B's session.
- A second instance on a database where the first runs with `[blobs] store = "fs"`, with a
  `[client_ca] key_file` or with an `[enrolment] bootstrap_ca_key_file`, fails startup naming the
  key, and so does an instance with either of them on a database where another instance holds a live
  lease; an instance that must be alone and loses its lock's connection logs it, ends its sessions
  and refuses every call.
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
- A Configuration whose revision a rollout through A pins is not deleted through B, a Package A
  pins is neither changed nor deleted through B, and a Deployment A released through is not
  deleted through B, though B's memory holds no such pin; a deletion through B while A's rollout
  has not committed waits for it and is then refused.
- A Configuration or a Package deleted through B is refused when A adds it to a Deployment before
  taking up the deletion, and an add through A that holds the Deployment while B deletes the
  member leaves no Deployment naming it.
- A blob an entry written through A names is kept by B's sweep, though B has not read the entry;
  the staging object of a lease released at an orderly shutdown goes with the next sweep on any
  instance, and two instances sweeping at once delete it once.
- One hundred heartbeats of one Agent within one presence interval write its presence once.
- An Agent that reconnects from B to A is held by A, and B stops carrying it.
- An Agent no instance carries is in no instance's memory, and A's Operator plane lists it, with
  its record and presence, from the database; a Server on the filesystem store lists the same
  Agent from memory.
- Labels and an assignment set through A on an Agent B carries survive B's next write of that
  record while A's notification is held back from B, and B holds them once it arrives.
- A notification is never received before the change it announces can be read, and a change
  whose transaction rolls back sends none.
- A binding a report makes, and a Host row a report, an issuance or a prune makes or moves forward,
  are announced after their commit: another instance hears of them; one whose notice was lost
  between the commit and the `NOTIFY` holds them after its next lease renewal; a notice that fails
  is sent again, and the write answers as made. A revocation, a release, a re-key, the release of
  every binding of a Host, a Gateway's mark, a Host forgotten and a register entry are heard
  although every notice sent after a commit is lost. A call that fails after a binding has its
  instance read the ledger again at once.
- A Gateway's mark through A of a Host B wrote, which A has not taken up, is written, A holds the
  Host as stored, and B holds the mark once it hears of it; a host neither the Hosts nor the
  register names is refused through any instance, one that still holds it in memory included, and
  nothing is written.
- A notification of a Configuration, a Deployment, a Package or a revocation has the instance
  read the database's again at once.
- After B's listening connection is restored, B holds the labels set and the Agents taken in
  between, and an approval decided in between reaches B's enrolment connection; a notification of
  an unknown kind makes B re-read as after a lost connection.
- Two instances taking one Agent at once leave it carried by one of them, never by none.
- An instance whose take of an Agent another instance's take overtakes before it holds the Agent
  in memory stops carrying it once it hears of that take.
- A report whose call to the database is held leaves the fleet lock free: another Agent's report
  is answered meanwhile, and another peer's TLS handshake completes.
- An Agent whose report's call to the database is held is answered a ping on the same connection
  meanwhile. The messages of one connection are handled one at a time and answered in the order
  they arrived, and a connection with eight messages, or the message limit in bytes, waiting is
  read no further until one is handled. Of the 10 000-Agent fleet below, connecting locally, every
  Agent is held within 60 seconds of the last connection opened, at least 150 new Agents a second,
  and none is re-keyed as a duplicate, as an Agent whose connection was ended for silence is.
- Many Agents created at once through twelve instances leave no more records than `max_agents`.
- A presence row ends with its connection: of an Agent whose take's answer was lost, of one
  re-keyed, of one taken by B and disconnected there before A hears of the take, whose holder's
  lease may have lapsed, and of one whose batch is held between its commit and its answers while
  it is taken again, the row names no holder once the Agent disconnected and the next batch ran;
  an Agent that disconnected under a lease that was then lost has its record flushed at shutdown.
- An Agent that B carried and wrote the disconnect of, and that A then carries with a take that was
  not written, keeps the report A wrote at B's shutdown flush, also where the record was forgotten
  and created again through B, and where B still holds the row with a live lease as it stops. An
  Agent that started again with lower sequence numbers, whose older record B's flush wrote over
  A's report, whether or not B still holds the row, has A's record again at A's next report.
- An Agent whose holder changes eleven times within a minute is logged once as a likely duplicate.
- A peer of ten Agents whose first reports wait longer than one heartbeat interval keeps its
  Agents' identities when it comes back on a new connection, and the older connection's reports
  and goodbye do not take them back or end them while the newer one carries them; a duplicate on
  a connection of another peer, or of a connection that presented no host, is re-keyed. A queued
  restart is handed out, in a reply or pushed, only on the connection that carries the Agent.
  On two connections of one Gateway, an Agent that moved to the older one is taken by its report
  there once the newer one has carried none of its reports for a heartbeat interval, and not
  before: its goodbye there ends it, and it is asked for its component map there.
- A re-key, a revocation, a label and a floor whose commit lands after the call's answer was lost
  are held in the instance's memory once the commit lands: the re-keyed Agent is one row under
  its new identity, no record is created for its old one, and a re-key whose read back failed
  keeps the Agent's session when its own notification arrives, when its read back there fails
  first and when a report or a rollout finds the old row gone, writes the disconnect of a session
  that ended meanwhile to the new row, and assigns the same new identity at the next report, while
  a report under the old identity is refused as long as the move is not known. A notification of
  an earlier call of the instance's own is not taken for that of a call whose answer was lost. The
  read back of a take still in flight waits for it, so the row ends with the connection once the
  Agent disconnected.
- On an orderly shutdown an instance writes the presence of its Agents and releases its lease, and
  another instance reads them as disconnected at once.
- An instance that must be alone, started within `lease_secs` of the crash of the process it
  replaces, takes that process's lease over and starts.
- Instances of version N and N+1 on one database both serve the Agents that report to either, and a
  notification of a kind only N+1 sends makes the instance of N re-read as after a lost connection.
- An instance fails its start, naming both versions and leaving the schema as it was, on a schema
  whose oldest Server version is newer than its own, and while a live lease names a release older
  than the oldest Server version the schema names once its own migrations are applied, whether
  those migrations are applied already or not; a lease of 0.8.0 records 0.8.0 by the default; a
  lease clause 2 takes over counts as none; with no such lease it migrates the schema it finds and
  starts.
- An instance of N that starts while one of N+2 migrates either is seen by it, which then fails, or
  sees N+2's schema and fails; an instance of N whose lease lapsed while N+2 migrated stops when it
  takes its new lease.
- The two G-20 tests run three Server processes at the defaults, `max_connections` and `max_agents`
  raised for the fleet, behind a layer-4 balancer of the test's own: it hashes the peer's address
  over the instances that answer its probe, and it connects to the instance from the peer's own
  address, which on the loopback every test address is, so the instances see the peer's address.
  Each simulated host has a certificate and an address of its own and carries `k` Agents on one
  connection through the `crates/opamp` client; the tests name `k`, and the time the fleet took to
  connect, which neither bound counts.
- A fleet of 10 000 simulated Agents, one on each connection, across three instances, one of them
  killed (`SIGKILL`), is carried by the other two within `lease_secs` plus two heartbeat intervals,
  with one holder write per Agent taken over and no notification left unread once they are carried;
  a revocation committed during the takeover ends its session within `lease_renew_secs`, and no
  Agent of the stopped instance reconnects before the delay clause 20 drew for it. The test names
  its fleet size, its instance count and its timings, and fails when a bound is missed (G-20).
- The presence batches and notifications of 100 000 simulated Agents at the default heartbeat,
  ten on each connection, so that one machine holds the simulated fleet, about 3 300 reports a
  second, across three instances on one database, run for ten minutes with no presence batch
  written later than its interval after it was due and no notification left unread (G-20).
  Meanwhile one Agent in a hundred reconnects each minute, labels are set on 100 Agents every five
  seconds, a Configuration is rolled out to 10 000 Agents and a certificate revoked once a minute,
  and a page of the Agents is read every ten seconds.
- `[database] lease_secs`, `lease_renew_secs` or `presence_interval_secs` with `store = "fs"` fails
  startup naming the key.
- When B stops without an orderly shutdown, A takes over B's lease once it lapsed and closes B's
  chain with `audit.chain_closed`, `reason` `lease_lapsed` and `closed_by` A's chain;
  `audit-verify` reports B's chain as closed, and an entry B appends to it afterwards is
  refused.

**Not mechanically decidable:** the operator's side of clauses 14, 15 and 17 — the balancer's
affinity and its preservation of the peer's address, the database's synchronous replication and
its failover. The Server cannot see them. The manual states each as an operating requirement, and
the load test above runs with a balancer that preserves the address. Clause 16 is decided at each
instance's start and at each new lease, by the schema and by the releases the live leases record.
