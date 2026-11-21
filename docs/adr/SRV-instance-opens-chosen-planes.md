# SRV-instance-opens-chosen-planes: Several Server instances share one PostgreSQL database, two versions run side by side, a group is placed again when its Client Agent is bound or arrives elsewhere, and each instance opens the planes its configuration names

- **Status:** 🟡 proposed
- **Date:** 2026-11-21
- **Deciders:** Markus Brigl
- **Applies to:** the in-memory fleet and its push in `crates/fleet-server/src/fleet.rs`, what one instance holds of it in `crates/fleet-server/src/fleet/held.rs`, the sessions and their guard in `crates/fleet-server/src/transport.rs`, the ports, the PostgreSQL adapters and the change signals of the enrolment window and its queue in `crates/fleet-server/src/enrolment.rs` and of the certificate register, the revocation list and the Hosts in `crates/fleet-server/src/revocation.rs`, the per-instance tables of `crates/fleet-server/src/agent_rate.rs` and `crates/fleet-server/src/throttle.rs`, the package floors in `crates/fleet-server/src/floors.rs` and `crates/fleet-server/src/postgres/floors.rs`, the PostgreSQL adapters of Configurations, Deployments and Packages in `crates/fleet-server/src/postgres/`, the PostgreSQL adapters and their schema, the presence batches in `crates/fleet-server/src/instance.rs`, the WebSocket driver in `crates/opamp/src/client/ws.rs`, the audit port in `crates/fleet-server/src/audit.rs` and its PostgreSQL adapter where an instance closes the chain of a lapsed lease, the `[database]` section of `server.toml` and its parsing in `crates/fleet-server/src/config.rs`, `config/server.toml`, `docs/ARCHITECTURE.md`, the operator manual's pages on replication, load balancing and rolling updates, the `planes` key of `server.toml` and its parsing and startup checks in `crates/fleet-server/src/config.rs`, the binding, serving, closing and draining of the planes in `crates/fleet-server/src/instance.rs`, `crates/fleet-server/src/listen.rs` and `main.rs`, the use-case modules the composition root builds, the lease record in `crates/fleet-server/src/postgres/lease.rs` and its schema, the startup log, `scripts/check-g20.sh` and `crates/fleet-server/tests/g20.rs`, and every document that tells an operator how to run several instances or how to probe one
- **Supersedes:** [SRV-instances-share-one-postgresql](SRV-instances-share-one-postgresql.md)

## Context

This ADR carries every clause of
[SRV-instances-share-one-postgresql](SRV-instances-share-one-postgresql.md) and changes two of
them, so that each instance opens the Agent plane, the Operator plane or both: the lease taken
before the listeners open (clause 1), and the lease's gate, close, probe and drain (clause 3), now
concern the listeners and planes the instance opens. It adds five clauses: the `planes` key, both
planes on an instance that must be alone, the use cases an instance builds, one configuration
checked whole on every instance, and the warning while a plane goes unserved. Two successors
change with it, each proposed with this ADR:

- [SEC-tls13-bounded-input-chosen-planes](SEC-tls13-bounded-input-chosen-planes.md) has its
  clauses 7 and 14 to 17 follow the planes an instance opens;
- [OPS-releases-image-probed-by-plane](OPS-releases-image-probed-by-plane.md) states the
  documented probe of the Server's image by plane (its clause 17).

**Clause map.** Every clause of SRV-instances-share-one-postgresql keeps its number, and the new
ones follow as 22 to 26. On acceptance, a citation of SRV-instances-share-one-postgresql §*n* moves
to §*n* of this ADR.

| Old clauses | Clauses here | Change |
|---|---|---|
| SRV-instances-share-one-postgresql §1 | 1 | the lease is taken before any listener of the planes the instance opens, and those are the listeners it keeps open |
| SRV-instances-share-one-postgresql §2 | 2 | none |
| SRV-instances-share-one-postgresql §3 | 3 | the `503` before each request, the close on a lease it cannot renew and the drain concern the listeners and planes the instance opens; the probe is the port of a plane it opens, citing OPS-releases-image-probed-by-plane §17, and nothing is sent to an instance without a lease |
| SRV-instances-share-one-postgresql §4–§21 | 4–21 | none |
| none | 22–26 | new: the `planes` key and when it may be set, both planes on an instance that must be alone, the use cases of the planes an instance opens, one configuration checked whole on every instance, the warning while a plane goes unserved |

A Server process holds its fleet in memory (`AppState` in `crates/fleet-server/src/fleet.rs`). On
the filesystem store that is the whole fleet, loaded at startup. On PostgreSQL it is the Agents
the process carries (clause 8). Three in-process signals tell live sessions that something changed:

- the fleet's `push` channel, a `watch::Sender<u64>` bumped by a rollout, a Package or Deployment
  change, or a restart;
- the revocation list's `subscribe`, which every WebSocket session's guard waits on to end itself
  when its certificate is revoked
  ([SEC-admission-by-client-certificate §32](SEC-admission-by-client-certificate.md));
- the enrolment queue's `subscribe`, which hands an approved certificate to the connection that
  asked for it.

On the filesystem, `connected` is what one process knows about its own connections and is not
persisted. On PostgreSQL it is persisted with the id of the instance that holds the connection
([SRV-fleet-record-agents-hosts §3 and §6](SRV-fleet-record-agents-hosts.md)). The rate limits and
the admission throttle are tables in memory
([SEC-tls13-two-planes-bounded-input §28](SEC-tls13-two-planes-bounded-input.md),
SEC-admission-by-client-certificate §19). Duplicate detection tells two connections of one
`instance_uid` apart by a connection id that only this process hands out.

One process alone is one point of failure and one machine's worth of connections. G-20 asks that a
fleet of up to about 100 000 Agents be served on when one of several Server instances stops without
warning. Its Agents connect to another instance within a bound, and a revocation ends its sessions
on every instance within another. The Strategy and the Vision have several instances serve one
fleet on the stores the operator runs, any of them that opens the Agent plane taking over the
Agents of one that is lost: active-active, sharing state only through PostgreSQL and the object
store. Three ADRs prepare it:

- [SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md) puts every store behind the
  ports into one PostgreSQL database, in whose schema every Server is an instance with a lease
  (its clause 23);
- [SEC-append-only-audit-record](SEC-append-only-audit-record.md) gives each writing instance its
  own audit chain;
- [API-lists-paged-by-cursor](API-lists-paged-by-cursor.md) lets the Operator plane read the fleet a
  page at a time from the database.

This ADR turns those into several instances. A CA key the Server holds is a file, which one
instance alone can hold (clause 2). CA keys every instance can reach are a decision of their own.

At a 30-second heartbeat, 10 000 Agents send about 330 messages a second and 100 000 about 3 300.
Writing `last_seen_ms` on every heartbeat would make the database the bottleneck for messages that
change nothing else.

A reader would expect two things this ADR decides against: a routing table that says which instance
carries which Agent, so an event goes only there; and rate limits shared across instances.

On PostgreSQL several Server instances serve one fleet, and under
[SRV-instances-share-one-postgresql](SRV-instances-share-one-postgresql.md), which this ADR
supersedes, every instance serves both planes: the Agent plane on `4320` and the Operator plane on
`4321`
([SEC-tls13-bounded-input-chosen-planes §7](SEC-tls13-bounded-input-chosen-planes.md)). The two
planes share an instance's CPU, memory and database pool (`[database] pool_size`); only their
connection caps are apart
([SEC-tls13-bounded-input-chosen-planes §19](SEC-tls13-bounded-input-chosen-planes.md)).

Goal G-20 asks that a fleet of about 100 000 Agents is served on when an instance is lost. Its
Agents connect to the others within 90 seconds, and a revocation committed on any instance ends its
sessions on every instance within 10 seconds. That is the moment the two planes compete most: the
Agents of the lost instance reconnect to the rest, and an operator may have to act at once, to
revoke a certificate or to hold a rollout. Every instance that takes the reconnecting Agents also
serves the operator, through the same pool. A heavy Operator-plane read slows the sessions the same
way. Where the planes run on instances of their own, G-20 also bounds the Operator plane while the
Agents reconnect. The Operator plane of an instance that did not stop then answers each page of up
to 100 Agents and each revocation successfully within 2 seconds. Roles make that bound possible.
Instances of one plane each keep the reconnecting fleet off the instances an operator acts through.

The planes already meet only through the database. An operator's act on one instance reaches the
sessions on another by a notification (clause 9) or by the re-read after a missed one (clause 10),
and the Operator plane reads the Agents from the database (clause 13). An instance that opens one
plane alone therefore needs no new channel between instances. What it needs is code in which each
plane reaches state through its own module, so that an instance can leave the other module
unbuilt; [SRV-two-use-case-layers](SRV-two-use-case-layers.md), proposed, decides that split. This
decision follows it and is implemented only after it.

A certificate is signed where its request is decided. A renewal's host certificate is signed on
the Agent plane, on the connection its CSR came over. The certificate of an approved enrolment
request and a bootstrap certificate are signed through the REST API, on the Operator plane. Where
several instances run, clause 2 lets none of them hold a CA key in a file. Every instance reads and
checks `[client_ca]` and `[enrolment]` as an instance that opens both planes does (clause 25). It
therefore reaches the same CA key, whichever planes it opens. `planes` is set per instance.

Three ADRs in force mean "both planes" in clauses that stop being true for such an instance:

- SRV-instances-share-one-postgresql: the lease's listeners, which open, close and drain together,
  and the documented probe, a TCP connection to `4320` (clauses 1 and 3). This ADR carries them
  changed.
- [SEC-tls13-two-planes-bounded-input](SEC-tls13-two-planes-bounded-input.md).
  [SEC-tls13-bounded-input-chosen-planes](SEC-tls13-bounded-input-chosen-planes.md) carries these
  clauses changed:
  - two listeners (clause 7), always, bound before either serves (clause 14);
  - both planes served by `listen::plane` (clause 15);
  - the handshake deadline on both planes' acceptors (clause 16);
  - one drain of both planes on one `Handle` (clause 17).
- [OPS-releases-attestations-server-image](OPS-releases-attestations-server-image.md): the image's
  own statement of the probe (clause 17).
  [OPS-releases-image-probed-by-plane](OPS-releases-image-probed-by-plane.md) carries it changed.

## Decision

We will let any number of Server instances serve one fleet from one PostgreSQL database, each
holding a lease and the sessions it carries, writing presence in batches, telling the others of
every change that concerns a live session by one `NOTIFY`, and opening the Agent plane, the
Operator plane or both, as the `planes` key of its configuration names.

1. **Every Server on PostgreSQL is an instance, and there is no switch for "several".**
   - With `[database] store = "postgres"`
     ([SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md)) a Server takes an
     instance id at startup, a random UUID that is not persisted.
   - It takes a lease in the database before it opens any listener of the planes it opens. It
     keeps those listeners open only while it holds a lease it can prove (clause 3).
   - Every line the instance logs carries its instance id. The take, the loss, the release and the
     takeover of a lease are logged with every instance id involved. The logs of several instances
     therefore say which one acted.
   - How many instances run is whatever the operator starts.
   - With `store = "fs"` there is one instance, no lease, and no instance id in the log.

2. **Several instances need shared blobs and no CA key in a file.**
   - An instance must be alone when its `[blobs] store` is `"fs"`, or when its client CA's or
     bootstrap CA's key is a file. Besides its lease, it takes an exclusive lock that says it is
     the only instance: a session-level advisory lock held on a connection of its own.
   - A second instance on the same database then fails startup, naming the key.
   - An instance that must be alone fails startup when another live lease exists already. It names
     the key and the number of live leases.
   - A lease records whether its holder must be alone. One that does, and whose exclusive lock is
     free, belongs to a process that is gone. The next instance that takes a lease takes such a
     lease over as clause 18 does, instead of failing, whether it must be alone or not. It closes
     the chain with `reason` `holder_gone`, since the lease has not lapsed
     ([SEC-append-only-audit-record §7](SEC-append-only-audit-record.md)).
   - When the lock's connection ends while the instance runs, the lock is gone with it. The
     instance logs an error naming the database, ends every session it carries, and refuses every
     call from then on. It therefore never writes beside a second instance. It takes the lock again
     only when it is started again.

   Where several instances serve one database, none holds a CA key in a file.
   - With `store = "s3"` every instance uses the same bucket and prefix.
   - Every sweep of unreferenced blobs and every removal of stale staging objects runs under an
     advisory lock, so one instance does them at a time. An upload takes that lock shared in the
     transaction that writes its entry.
     [SRV-state-stores-contract-pair §14](SRV-state-stores-contract-pair.md) decides it for the
     blob store.
   - An instance on PostgreSQL and S3 keeps nothing on its own disk (that ADR's clause 1). It reads
     its configuration, its TLS files and its secret files, all of them read-only. With no CA key
     on the Server, any instance can take its place.
   - The Server's image names no data directory, so such an instance runs from it as it stands
     ([OPS-releases-attestations-server-image §16](OPS-releases-attestations-server-image.md)).

3. **The lease.**
   - Each instance renews its lease every `[database] lease_renew_secs` (default 10). The lease
     lapses `[database] lease_secs` (default 30, at least three renewals) after the last renewal.
   - These two keys and `[database] presence_interval_secs`
     ([SRV-fleet-record-agents-hosts §4](SRV-fleet-record-agents-hosts.md)) apply to
     `store = "postgres"` alone. With `store = "fs"` each fails startup, naming it.
   - An instance measures its own lease from the moment it sent the last renewal the database
     confirmed. That moment is no later than the database's own time of that renewal. The instance
     therefore deems its lease lapsed no later than the database and every other instance do.
   - It measures on a clock that counts the time its host was suspended, on Linux
     `CLOCK_BOOTTIME`. A suspend therefore lapses its lease for it as it does for the database.
   - It reads that clock at least every 100 milliseconds, and before it answers each request on a
     plane it opens and each message of an Agent. While its lease is lapsed by that clock it answers
     `503` and nothing else.
   - An instance that runs again after a suspend serves nothing until it holds a new lease.
   - A virtual machine that its hypervisor pauses without the guest's clocks counting the pause is
     outside this. It serves on until its next renewal, at most `lease_renew_secs` later, finds its
     lease lapsed.
   - An instance that cannot renew before its own lease lapses ends every session it carries and
     closes the listeners it opens. Once the database answers again, it takes a new lease under a
     new instance id and opens them again.
   - It never serves on a lease it cannot prove, because it cannot learn of a revocation.
   - A closed listener is what a load balancer's or an orchestrator's probe sees. The image's probe
     is a TCP connection to the Agent plane's port when the instance opens that plane, and to the
     Operator plane's port otherwise, for liveness and readiness
     ([OPS-releases-image-probed-by-plane §17](OPS-releases-image-probed-by-plane.md)).
     An instance without a lease is then neither live nor ready, so neither an Agent nor an
     operator's request is sent to it.

   An orderly shutdown, on `SIGTERM` among others, drains the instance:
   - It closes the listeners it opens at once, so no new connection reaches it. It answers the
     requests already in flight on the planes it opens.
   - It ends the WebSocket sessions it carries with close code `1001` (going away). It ends each at
     an instant drawn uniformly over `drain_secs` (default 20, from 0 to 300; 0 ends them all at
     once). Its Agents therefore reconnect to the other instances without arriving all at once.
   - Then it writes the presence of its Agents, closes its audit chain
     ([SEC-append-only-audit-record §7](SEC-append-only-audit-record.md)), and releases its lease.
   - The default leaves the rest of an orchestrator's usual grace period of 30 seconds for those
     writes.
   - `drain_secs` applies on the filesystem store as well, where the one Server drains the same
     way.

4. **[SRV-fleet-record-agents-hosts §3](SRV-fleet-record-agents-hosts.md) decides it: What a report
   or an operator decided persists; so does where an Agent is connected.**

5. **[SRV-fleet-record-agents-hosts §4](SRV-fleet-record-agents-hosts.md) decides it: The write
   discipline lives above the port, so no backend can get it wrong.** On PostgreSQL that clause
   writes presence in batches. A batch written later than its interval after it was due is logged
   as a warning naming how late it was. A database that cannot keep up with the fleet's presence
   therefore shows before an Agent reads as stale.

6. **[SRV-fleet-record-agents-hosts §6](SRV-fleet-record-agents-hosts.md) decides it: `connected`
   says a connection carrying the Agent is open, nothing more.**

7. **[SRV-fleet-record-agents-hosts §8](SRV-fleet-record-agents-hosts.md) decides it: The budget
   is three heartbeat intervals, and on PostgreSQL the presence interval besides.**

8. **On PostgreSQL an instance holds only the Agents it carries, and the newest connection takes an
   Agent.**
   - **What an instance holds.** On the filesystem store the one Server keeps the whole fleet in
     memory ([SRV-state-stores-contract-pair §3](SRV-state-stores-contract-pair.md)). On
     PostgreSQL each instance keeps in memory the records of the Agents whose connection it holds,
     loaded from the database when their first report arrives, and nothing of the rest.
   - **The take.** When an instance begins to carry an Agent, it takes the Agent at once, outside
     the batch. The take is one update under the row's lock that names the instance as the holder
     and returns the holder it replaced. The instance announces the take where it replaced another
     holder, whether or not that holder's lease is live (clause 9).
   - **Hearing of a take.** An instance that receives the announcement reads the holder again. It
     stops carrying the Agent, as it would on a closed connection, only when the row no longer
     names it. Two instances that take one Agent at once therefore never both drop it.
   - **The newest connection.** The newest connection is the one whose take commits last. A slower
     write may commit the take for an Agent's older connection after the take for its newer one.
     Then the older connection's instance holds the row. The other instance stops carrying the
     Agent and takes it back at the Agent's next report on it.
   - **The presence batch.** A batch names its instance as the holder anew only of a row that names
     no holder or an instance without a live lease
     ([SRV-fleet-record-agents-hosts §2](SRV-fleet-record-agents-hosts.md)). Where this instance
     was answered a count of takes for the Agent, the batch names it only while the row's count is
     still that one. It never names its instance over a live holder, nor, knowing its count, over a
     take counted since its own.
   - **A take whose answer was lost.** Such a take leaves the instance with no count for the Agent,
     except for the shutdown flush. The flush keeps the count the instance was answered before the
     take, or 0 where it created the record, and writes the record under it
     ([SRV-fleet-record-agents-hosts §2](SRV-fleet-record-agents-hosts.md)). An Agent with neither
     count is not flushed. Its row keeps what was last written of it, and the next instance to
     carry it finds a gap in its sequence numbers and asks for a full report.
   - A report takes nothing.
   - **The flush and sequence numbers.** A shutdown flush on PostgreSQL writes no record whose
     stored sequence number is higher than the one it writes, whichever holder the row names. A
     newer report another instance wrote meanwhile therefore stays. That holds also where the
     flushing instance still holds the row while it stops, its lease live. It holds where the
     record was forgotten and created again with a count that started over.
   - **An Agent that started again.** It counts its sequence numbers from the start, so a flush with
     a higher one can still write over the report of the instance that carries it now.
     - Until that instance is answered a count of takes for the Agent, it writes the record at every
       report of it, not only where what a report decides changed. A row such a flush made stale
       therefore holds the carrier's record again at the Agent's next report.
     - The carrier is answered a count by its next presence batch where no other instance with a
       live lease holds the row. Otherwise the first batch after that lease is gone answers one, or
       the take at the Agent's next connect does. From then on a report that changes nothing
       durable writes nothing again.
     - Where none of these comes, every report of the Agent keeps writing. That is the case with a
       duplicate of the Agent live on two live instances, the other of which holds the row.
     - A flush refused for a higher stored sequence number leaves its own Agent's newer sequence
       number unwritten. The next report to the instance that carries it writes it, or the full
       report a gap asks for does.
   - **The port.** The port of [SRV-fleet-record-agents-hosts §2](SRV-fleet-record-agents-hosts.md)
     has `write_report_unless_held_elsewhere` write, on a shared store, only a record whose stored
     sequence number is not higher than the record's. A store whose one Server is its only writer
     writes as `write_report` does.
   - **The disconnect after a lost take answer.** Before the instance writes the disconnect of an
     Agent whose take lost its answer, it reads the row's holder and count of takes through the
     port's `takes(uids)`. It reads them under a share lock on the row that waits for a take still
     in flight. It writes the disconnect under the count it read, where the row names it and it
     took the Agent no more since the read.
   - **Duplicates and the newest connection of a host.**
     - Duplicate detection and the rekeying of a newcomer stay within one instance. They take for a
       duplicate only an Agent alive on a connection of another peer.
     - An Agent may come back on a new connection of the host whose certificate its old connection
       presented. The newest connection takes it. While a newer connection carries it, the older
       connection's reports, its goodbye among them, no longer take it back or end it. Nor is a
       queued restart sent on that connection.
     - A connection that presented no host has no such peer.
     - A newer connection that has carried no report of the Agent for one heartbeat interval carries
       it no more. The interval is the one this Server offers, or 30 seconds without an offer. The
       next report on an older connection of the host then takes the Agent back, its goodbye ends it
       there, and it is asked there for what it is due.
     - A Gateway's pool moves an Agent between connections that all stay open, so this is how a
       Gateway's Agent follows it.
     - One host is one peer. Two Agents of one `instance_uid` on one host, a clone with the host's
       certificate, therefore share one record.
     - An Agent whose holder changes more than ten times in a minute is logged as a likely
       duplicate.
   - **Acts through another instance.**
     - An Agent's labels stay in its record
       ([SRV-fleet-record-agents-hosts §21](SRV-fleet-record-agents-hosts.md)), and there is no
       other copy.
     - A label change, a rollout's assignments and a restart request made through any instance are
       written to their own columns of the record in the database and announced (clause 9).
     - The instance that carries the Agent writes only what a report decides
       ([SRV-fleet-record-agents-hosts §2](SRV-fleet-record-agents-hosts.md)). Its next write
       therefore keeps them whether or not the notification has reached it. It takes them into the
       record it holds when the notification arrives.
   - **Delivering a queued restart.**
     - The instance that delivers a queued restart clears it before the send. It puts it back when
       the command was not sent: a WebSocket frame the connection does not take, or a plain-HTTP
       answer past the size limit.
     - It puts it back on a lease that lapsed meanwhile too, since setting the restart again only
       restores what the operator asked for.
     - A put-back the store does not take is kept in the instance's memory and tried again, at
       intervals that grow to a minute, until it holds. A process that ends before then loses the
       restart.
     - These drop a kept put-back:
       - a request or a delivery of a restart for that Agent through this instance meanwhile, as two
         requests before a delivery are one restart;
       - the take-up of a request through another instance, which that instance announces
         (clause 9);
       - after a missed notification, a record that shows a restart queued;
       - the Agent's forget through this instance, or through another while this one holds the
         Agent.
     - A delivery through another instance is not announced; the request it delivered was.
     - A re-key of the Agent through this instance moves a kept put-back to the new identity. The
       re-key of a duplicate newcomer moves no row and leaves it.
     - Each try is decided under the lock that requests, deliveries and forgets of this instance
       take, so a try overtakes none of them.
     - Three cases are not seen:
       - A forget through another instance after this one stopped holding the Agent is seen only as
         its record gone when the put-back is tried. An Agent that came back under the same identity
         before that try is restarted for it.
       - A request through another instance delivered before this instance took up its
         announcement is not seen either. The Managed Process is then restarted once more.
       - A re-key of the Agent through another instance while this one tries is seen only as its
         record gone too, so the restart is not tried again.

       The instance logs a warning naming the Agent whenever a put-back finds its record gone.
     - On a WebSocket the restart is therefore delivered at least once wherever the Server sees the
       send fail, except for such a re-key. A frame lost after the connection took it is not seen.
     - On plain HTTP the answer goes out with the response's head. A poll whose answer is lost is
       not seen either, and the operator requests the restart again.
     - A command that reached the Agent although its send failed is sent again and restarts the
       Managed Process twice. The manual says so.
   - **Effective descriptions across instances.**
     - An effective description (SRV-fleet-record-agents-hosts §18) may need the labels of a Client
       Agent this instance does not carry, or a Host's subject values. The instance then reads them
       from the database.
     - It finds that Client Agent among the Agents the host register binds to the Host. The
       register binds none to a Gateway's Host. Behind a Gateway the instance therefore finds it by
       a read of the Agents that arrived over that Host, through the port's `query_page` filtered
       on that Host ([API-lists-paged-by-cursor §9](API-lists-paged-by-cursor.md)).
     - So an Agent behind a Gateway inherits, in the memory of its own instance too, the labels of
       a Client Agent another instance carries.
   - **Placing a group.** An instance places such a group:
     - when it places one of its Agents;
     - again when the labels of the Client Agent it placed the group under change;
     - again when a notice that a binding of the group's Host changed reaches it;
     - again when a notice that a Client Agent first arrived over the group's Gateway reaches it;
     - every group, after a missed notification.

     Another instance may bind a Client Agent to the Host, or take one behind the Gateway. Once the
     notice of it arrives, a fellow Agent inherits that Client Agent's labels in this instance's
     memory too. What the Operator plane shows is read from the database and inherits them.

9. **Every change that concerns a live session is one `NOTIFY` to every instance.**
   - The writing instance commits the change and, in the same transaction, sends one `NOTIFY` on
     one channel. The notice names the kind and the identifier, and the instance and the call that
     sent it, and nothing else.
   - The kinds:
     - an assignment changed;
     - a Configuration, Package or Deployment changed;
     - a restart requested;
     - a revocation added or lifted;
     - a certificate registered or removed;
     - a Host written or an `instance_uid` bound to it;
     - an enrolment request queued or decided;
     - the enrolment window opened or closed;
     - a package floor set or removed;
     - an Agent's labels set;
     - an Agent forgotten;
     - an Agent taken by another instance;
     - a Client Agent first arrived over a Gateway's Host.
   - Three writes announce their change right after its commit instead. They announce on the same
     connection, in a transaction of their own that does not wait for its WAL flush:
     - a binding a report makes;
     - a Host row made, or whose valid until moves forward, by a report, an issuance or a prune;
     - the record of a Client Agent that first arrives over a Gateway's Host, named by that Host.
   - The reason: PostgreSQL lets one transaction that sent a `NOTIFY` commit at a time,
     database-wide, and holds that turn until the commit's flush ends. A fleet whose new Agents
     each announced in their writing commit would connect no faster than one flush apiece.
   - The first two widen what a host speaks for, or how long it does, which the database decides at
     every binding (clause 19). The third only has the instances place the Gateway's groups again
     (clause 8). An instance that hears of them late therefore admits nothing it must not.
   - Every write that narrows trust keeps its `NOTIFY` in its transaction:
     - a revocation added;
     - a binding released or moved;
     - every binding of a Host released at once;
     - a Gateway's mark set or taken away;
     - a Host forgotten.

     Every register entry keeps it too.
   - A notice sent after its commit that fails is logged and sent again a few times. The call that
     wrote the change answers as it would without it. One never sent is taken up at the next lease
     renewal (clause 10), except for the placement it asks for. A group that a binding's or an
     arrival's lost notice was to place again (clause 8) is placed again at the next event that
     places it.
   - Each instance listens on a connection of its own and acts on the sessions it holds. It composes
     a new offer, ends a revoked session (clause 11), offers an approved certificate, closes its
     enrolment connections, takes up an Agent's labels, or drops the Agent.
   - There is no table of which instance holds which session, and no message addressed to one
     instance.

10. **A missed notification counts as every notification.** `NOTIFY` reaches only the sessions
    listening when it is committed.
    - When an instance's listening connection is lost and re-established, the instance acts as if
      everything had changed:
      - it drops its caches;
      - it re-reads the revocation list and checks every session against it;
      - it re-reads the record and the holder of every Agent it carries, and stops carrying each
        one another instance holds;
      - it re-reads the package floors, the enrolment window and the requests of its enrolment
        connections.
    - A notification of a kind the instance does not know counts as a missed one. A newer version
      sends such kinds while two versions run side by side (clause 16).
    - The instance does the same when a lease renewal finds the revocation list's version in the
      database ([SRV-state-stores-contract-pair §10](SRV-state-stores-contract-pair.md)) newer than
      the one it holds.
    - At every lease renewal it reads the ledger again besides, the Hosts and their bindings among
      it. A notice sent after its commit that never arrived is therefore taken up within one
      renewal, as when its instance ended between the two (clause 9).
    - That reads the ledger, not the groups. A group such a notice was to place again (clause 8)
      waits for the next event that places it. A lost notice of an arrival over a Gateway's Host is
      not taken up at a renewal at all.
    - **A write whose answer is lost counts as a notification of its own.**
      - A call that sent a `NOTIFY` may fail on a lost connection, its bound or an error of the
        database. The instance then cannot tell whether it committed.
      - It reads again what each notification of the call names, as it would on hearing it. It
        hears once more the notifications that call sent, told by the call they name from any other
        of its own. A commit that lands after that read is therefore taken up when it lands.
      - A notice that would follow its commit (clause 9) is read again at once where its call
        fails, as the others are. It is not heard once more, as none may be sent. A commit of it
        that lands after that read is taken up by the instance at its next report of the Agent,
        which the database answers. Every instance takes it up at its next lease renewal, except
        for the placement it asks for (clause 9).
    - **A re-key reads back where the row is before it answers.**
      - It reads under a share lock on the old row, which waits for the transaction that moves it,
        and then the new one. Moved, the re-key stands.
      - Where the read back fails, the instance keeps the pair. The next read of the old identity
        reads it back: at a report, at a notification of it, and wherever the instance finds its row
        gone.
      - A read back at a notification that fails is tried again until it holds. Until then the
        instance keeps the Agent under its old identity.
      - Where the move landed, the instance moves what it holds to the new identity and keeps the
        Agent's session. It assigns the new identity at the next report under the old one. A session
        that ended meanwhile has its disconnect written to the row the move landed in.
      - While the move is not known, a report under the old identity of which the instance holds no
        record is answered `Unavailable` and creates none.

11. **[SEC-admission-by-client-certificate §32](SEC-admission-by-client-certificate.md) decides it:
    A revocation ends the sessions it concerns, and only those, on every instance.**

12. **Caches are read again, not updated.**
    - Each instance may hold in memory Configurations, Deployments, Packages, the package floors,
      the revocation list, the certificate register and the Hosts, as the ports allow
      ([SRV-state-stores-contract-pair §2](SRV-state-stores-contract-pair.md)).
    - A notification of their kind has the instance read again at once what it concerns. For the
      revocation list, the register and the Hosts that is the whole ledger.
    - Nothing held is updated from the notification itself.

13. **On PostgreSQL the Operator plane reads the Agents from the database.**
    - What it shows of an Agent comes from the database, never from the memory of the instance that
      serves the request: its record, its presence, its staleness. So do the Agents each Host lists.
    - The Hosts, the certificates, the revocations and the pending enrolments it lists are the
      instance's copies. The instance reads them again on each notification of their kind and
      after a missed one (clauses 9, 10 and 12).
    - The four lists that grow with the fleet are answered a page at a time where a request asks
      for one ([API-lists-paged-by-cursor](API-lists-paged-by-cursor.md)).
    - An Agent's view names the instance that holds it, as read from the database:
      - `holder`, the instance id its presence names;
      - `holder_host`, the host name that instance's lease records.

      Both are empty for an Agent whose presence names no instance with a live lease. The Agent's
      detail shows them, and the list of Agents filters by `holder` (that ADR's clause 6). Each
      lease records the host name of its instance for this.

14. **Rate limits and the admission throttle stay per instance.** Their buckets live in each
    instance's memory. A host that reconnects to another instance starts there with a full bucket.
    The manual tells operators to give the Agent plane's load balancer source-address affinity.
    That affinity keeps a peer on one instance and the throttle effective (clause 15).

15. **TLS ends at the instance, and the balancer keeps the peer's address.**
    - In front of the Agent plane sits a layer-4 load balancer with TLS passthrough. The client
      certificate in the handshake is admission, so TLS never terminates at the balancer.
    - The balancer must preserve the peer's source address, by client-IP preservation or direct
      server return.
    - The admission throttle, the aggregation of refusals and the affinity of clause 14 key on that
      address. Behind a balancer that rewrites it, every Agent is one address. A few refused hosts
      would then put the whole fleet into back-off.
    - The Server reads no PROXY protocol header.
    - The Operator plane may sit behind a balancer in the same way. Every request carries its
      credential, so no session state is shared.

16. **Two consecutive versions run side by side.** A rolling update replaces the instances one by
    one, so versions N and N+1 serve one database at once.
    - Each schema change is expand-then-contract over two releases. The migrations of N+1 name N
      as the oldest Server version that understands the schema they leave. The instances of N that
      still run therefore serve on it. What N+1 stops using is removed by N+2 at the earliest
      ([SRV-state-stores-contract-pair §22](SRV-state-stores-contract-pair.md)).
    - An instance fails its start, naming both versions and changing nothing, on a schema that
      names a version newer than its own as the oldest that understands it.
    - Each lease records the release of its instance. Every lease taken before the migration that
      adds this record is one of 0.8.0, the release in which several instances first share a
      database. An instance of 0.8.0 writes no release, so the record names 0.8.0 by default.
    - An instance also fails its start while a live lease names a release older than the oldest
      Server version the schema names once the instance's own migrations are applied. It names its
      own release and the older one, and changes nothing. That lease's instance would serve on a
      schema it does not understand.
    - A lease that clause 2 takes over, whose process is gone, counts as none. With no such lease,
      as when every instance of an older release has stopped, the instance applies every migration
      it lacks. A Server on PostgreSQL may therefore skip releases while nothing older runs on its
      database.
    - An instance checks the schema and the live leases under the migrations' advisory lock
      ([SRV-state-stores-contract-pair §22](SRV-state-stores-contract-pair.md)) before it
      migrates. The transaction that takes its lease takes that lock too and checks the schema
      again. It does so at its start and whenever it takes a new lease after its own lapsed
      (clause 3). Of two instances that start at once, the later therefore sees the lease or the
      schema of the earlier.
    - On a schema that names a newer oldest Server version than its own, a start fails. An instance
      that finds one at a new lease stops as at a failed start, naming both versions. Its sessions
      ended with the lease it lost, and its Agents read as disconnected since that lease lapsed.
      Holding no lease, it writes nothing more, and it exits with a failure.
    - Instances of N and N+1 tell each other of their changes as clause 9 says. A kind N does not
      know counts as a missed notification (clause 10).
    - The one exception is the release in which several instances first share a database.
      - The release before it holds no lease, and the migrations of that release name that release
        as the oldest version. A Server of the release before is therefore refused.
      - An instance that shares the database neither migrates nor starts while another Server holds
        the database alone. A Server of 0.7.1 holds it so for its whole run (that ADR's clause 22).
      - That upgrade stops every Server of the release before first.

    How an instance applies the migrations it lacks is that ADR's clause 22.

17. **The database's durability is the operator's.** The manual states synchronous replication as
    an operating requirement: a failover must lose no audit entry and no revocation. Failover
    itself is the operator's infrastructure. An instance only reconnects, and clause 3 decides
    what it does in between.

18. **The instance that takes over the lease of a dead process closes its audit chain.**
    - A lease names the audit chain of the process that holds it
      ([SEC-append-only-audit-record §3](SEC-append-only-audit-record.md)).
    - At every renewal an instance looks for a lease that lapsed. It takes such a lease over in one
      transaction that locks the lease, so one instance alone takes it. In that transaction it:
      - appends `audit.chain_closed` to the chain the lease names, with `reason` `lease_lapsed`
        (or `holder_gone` for a lease clause 2 takes over before it lapsed) and `closed_by` the
        identifier of its own chain;
      - writes that entry's hash and the time into the chain record;
      - deletes the lease.
    - The store refuses an entry to a closed chain. A process that was cut off therefore cannot
      extend its chain afterwards.
    - An instance whose own lease lapsed takes its new lease (clause 3) with a new chain and writes
      nothing more to the old one.

19. **On PostgreSQL every act on shared state is one transaction on the rows it names.** Every
    instance shares these:
    - the certificate register, the revocation list and the Hosts;
    - the enrolment window with its queue;
    - the package floors;
    - the parts of a Configuration, a Package and a Deployment.

    No instance writes any of them whole from what it holds in memory.
    - These acts each change the rows they name and no other:
      - revoking or lifting one entry;
      - registering or removing one certificate;
      - writing one Host;
      - binding one `instance_uid` to a Host;
      - queueing, deciding or expiring one enrolment request;
      - opening or closing the window.
      - Each runs in one transaction that carries the notification of clause 9. The notification
        follows the transaction instead for a binding a report makes. It follows it too for a Host
        row made or moved forward by a report, an issuance or a prune
        ([SRV-state-stores-contract-pair §8–§10 and §21](SRV-state-stores-contract-pair.md)).
      - An approval registers its certificate, writes the Host it mints and decides its request in
        one such transaction
        ([SRV-state-stores-contract-pair §9](SRV-state-stores-contract-pair.md)).
    - An `instance_uid` is bound to at most one Host, by a unique constraint. A binding that meets
      another Host's fails. The instance then reads the binding that holds and decides by it.
    - The bounds of the register, the revocation list, the bootstrap entries and the enrolment
      queue are counted in the transaction that would pass them. Each therefore holds for the fleet
      and not for one instance.
    - The ceiling on the Agents' records, `max_agents`, is counted from one row in the statement
      that creates a record
      ([SRV-state-stores-contract-pair §22](SRV-state-stores-contract-pair.md)).
      It therefore holds for the fleet too.
    - Admission reads the chain of a certificate to its root in the register, and the binding of an
      `instance_uid`, from the database. It may read them from a cache instead, which the
      notification of each change to them drops (clause 12).
    - An act that decides its write from what is stored reads it in the same transaction, under
      `SELECT … FOR UPDATE` on the row, and decides from what the database holds.
      - A package floor is set only when its version is not below the stored one
        ([PKG-versions-high-water-mark-floor §15](PKG-versions-high-water-mark-floor.md)). The
        Server compares them by SemVer precedence, as SQL does not order SemVer.
      - A Configuration's revisions, a Package's entries and a Deployment's members are rows of
        their own. An act adds or removes the ones it names. The transaction reckons the revisions
        a Configuration keeps from the rows and from the assignments that pin them
        ([SRV-state-stores-contract-pair §2 and §21](SRV-state-stores-contract-pair.md)).
    - A Gateway's mark, set or taken away, is written only for a host the database knows, by its
      Host row or a register entry naming it. The transaction that writes the mark decides it, and
      refuses it otherwise, whatever the instance holds in memory
      ([SRV-fleet-record-agents-hosts §16](SRV-fleet-record-agents-hosts.md)).
      - So a Host another instance wrote is marked before this one hears of it. A host the database
        no longer knows is not marked from a stale copy.
      - An instance that held no copy of the Host reads the ledger again before it answers, so it
        holds the Host as stored.
    - A rollout holds the row of every Configuration, Package and Deployment its assignments pin
      `FOR KEY SHARE` until it commits. It fails when one is not stored, so it never writes a pin on
      what another instance deleted.
    - The deletion of a Configuration, a Package or a Deployment, and every write to a Package's
      entries, holds its row `FOR UPDATE` and counts the stored assignments that pin it. It
      therefore waits for such a rollout and is refused once the rollout committed.
    - An act that adds a Configuration or a Package to a Deployment holds the member's row
      `FOR KEY SHARE`, and is refused when the member is not stored. The deletion of a Configuration
      or a Package takes it out of every Deployment that holds it, in the transaction that deletes
      it.
      - So no Deployment names a member that another instance deleted, and no crash leaves one.
      - A member that an earlier release left in a Deployment after its deletion stays until it is
        taken out of the Deployment.

20. **A fleet that loses an instance comes back spread out.** The Agents of a lost instance
    reconnect as [SEC-tls13-two-planes-bounded-input §4](SEC-tls13-two-planes-bounded-input.md)
    says, whether its connections were cut, closed as it drains, or ended by a lapsed lease. The
    first attempt is drawn over `[0, 30 s)`, and each instance is bounded to
    `max_new_connections_per_sec` new connections a second.
    - Thirty seconds is a third of G-20's 90 seconds: a lease time and two heartbeat intervals at
      the defaults, which leaves the rest for a lapsed lease and two heartbeats.
    - Of a fleet of 100 000 on three instances, the 33 000 Agents of a lost instance then arrive at
      about 1 100 a second across the two left, instead of within half a second.
    - The G-20 test sends the 3 300 Agents of one of three instances to the other two over 30
      seconds, about 56 a second each, below the bound of 200 each must absorb.
    - The WebSocket driver of `crates/opamp` logs the delay it drew when a connection is lost. It
      may connect from a local address the application names. A Client names none. The G-20 test
      names one per simulated host. A balancer keyed on the peer's address then tells the hosts
      apart, and the test sees that no Agent reconnected before its delay.

21. **A Gateway's pool keeps each downstream Agent on one instance while its connection lives.**
    - A Gateway's pool ([CLT-modes-and-gateway §7](CLT-modes-and-gateway.md)) reaches the Agent
      plane through the balancer as any peer does, so its connections may land on different
      instances.
    - A downstream Agent is assigned to one pool connection and stays on it while that connection
      lives. Every report of it therefore reaches one instance, and the takeover of clause 8
      happens only when that connection ends.
    - A downstream Agent therefore does not move between instances while its Gateway's connection
      stands. The Server needs no rule of its own for a Gateway's connections.
    - Under the affinity of clause 14 a Gateway's connections, which share its address, all land on
      one instance.
    - When the instance behind a pool connection is lost, that connection ends, and its downstream
      Agents stay connected to the Gateway.
      - Each is placed anew at its next report, on a live pool connection or on one the pool opens
        for it, and is taken over there once (clause 8).
      - The Agents behind a Gateway therefore reach another instance spread over one heartbeat
        interval, as they report.
      - The pool opens a replacement connection when a report needs one. The Gateway's own Client
        connection reconnects as
        [SEC-tls13-two-planes-bounded-input §4](SEC-tls13-two-planes-bounded-input.md) says.
    - Meanwhile the Gateway serves on what it holds:
      - its revocation list, refreshed every 30 seconds from whichever instance its fetch reaches
        and trusted for 300 seconds ([CLT-modes-and-gateway §14](CLT-modes-and-gateway.md));
      - its floors, fetched hourly
        ([PKG-versions-high-water-mark-floor §16](PKG-versions-high-water-mark-floor.md)).

      The list's version is the same on every instance
      ([SRV-state-stores-contract-pair §10](SRV-state-stores-contract-pair.md)). A fetch that
      reaches another instance is therefore answered `304`, and the list stays young enough to
      trust.
    - A Gateway is not refused for sending all its pool connections from one address:
      - the admission throttle counts failures only
        ([SEC-admission-by-client-certificate §19](SEC-admission-by-client-certificate.md)), and a
        Gateway's own certificate does not fail;
      - the rate limits key on its host and on each `instance_uid`
        ([SEC-tls13-two-planes-bounded-input §28](SEC-tls13-two-planes-bounded-input.md));
      - the bound on new connections a second counts every connection alike, and a pool of at most
        `upstream_connections` (default 10) is a handful within it.

      A pool connection refused under that bound waits its `Retry-After` as any Client does.

22. **One key, both by default.** `planes` in `server.toml` is an array of `"agent"` and
    `"operator"`, by default both.
    - An empty array, an unknown name or a name given twice fails startup, naming the key.
    - The environment sets it as every key, such as `FLEET_SERVER__PLANES='["agent"]'`
      ([SRV-configuration-toml-and-environment §3 and §4](SRV-configuration-toml-and-environment.md)).
    - A release that does not know `planes` refuses it as an unknown key
      ([DEV-six-crates-tokio-axum §23](DEV-six-crates-tokio-axum.md)).
    - The operator must not set `planes` before every instance of the fleet runs a release that
      knows it. Until then every instance opens both planes, also during a rolling update
      (clause 16).

23. **An instance that must be alone opens both planes.**
    - With `[database] store = "fs"` there is one Server, and it must open both planes.
    - On PostgreSQL an instance must be alone where clause 2 says so: its `[blobs] store` is
      `"fs"`, or its client CA's or bootstrap CA's key is a file. Such an instance must open both
      planes too.
    - A `planes` that does not name both then fails startup. The message names `planes` and the key
      that makes the instance alone.

24. **What serves a plane's requests runs only where the instance opens that plane.**
    - Without the Agent plane an instance builds no `AgentCases`. It carries no Agent and writes no
      presence.
    - Without the Operator plane it builds no `OperatorCases`. It takes no act of an operator.
    - Every other task runs on every instance as today, whichever planes it opens. Among these
      tasks are:
      - its lease, and the takeover of a dead process's lease and chain;
      - its audit chain, with the pruning, the checkpoints and the export of the chains;
      - the notifications it listens for, and the caches it reads again;
      - the startup sweep of unreferenced blobs, and the removal of stale staging objects;
      - taking back, at start, the approvals the certificate register does not hold.

25. **Every instance reads and checks the whole configuration.**
    - Every instance reads and checks `server.toml` and its environment as one that opens both
      planes does, whichever planes it opens.
    - It requires the same keys and sections, checks the same values, and refuses what such an
      instance refuses.
    - This holds for `[tls]`
      ([SEC-tls13-bounded-input-chosen-planes §9](SEC-tls13-bounded-input-chosen-planes.md)) and for
      the files of `[rest.tls]`. It holds for the refusal of an Operator plane off the loopback
      without `[rest.auth]` too
      ([SEC-tls13-bounded-input-chosen-planes §8](SEC-tls13-bounded-input-chosen-planes.md)).
    - In what it reads and checks, a plane the instance does not open changes nothing. The
      instance checks that plane's keys and files as it checks those of a plane it opens.
    - The startup log names the planes the instance opens, and any plane it does not open.

    One `server.toml` therefore serves every instance of a fleet, with `planes` set per instance.

26. **An instance warns while a plane goes unserved.**
    - The lease records the planes its instance opens.
    - A lease written by a release that does not record planes counts as opening both. That holds
      for the older release's instances during a rolling update (clause 16).
    - When an instance takes its lease, and on each renewal, it reads the planes of every live
      lease.
    - For each plane that no live lease serves, it logs a warning naming the plane. It logs it once
      until a live lease serves the plane again.
    - It refuses nothing. How many instances serve each plane is the operator's.

**Out of scope:**

- the PostgreSQL schema and adapters themselves
  ([SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md));
- the audit chains, except for closing the chain of a process that died (clause 18)
  ([SEC-append-only-audit-record](SEC-append-only-audit-record.md)), and their checkpoints, a
  decision of their own;
- paging and filtering ([API-lists-paged-by-cursor](API-lists-paged-by-cursor.md));
- CA keys that every instance can reach;
- sharing rate limits across instances;
- a routing table or instance-to-instance messages;
- several databases or regions;
- the Agent plane's load balancer and the database cluster, which the operator runs;
- a database pool of each plane's own on an instance that opens both;
- a health endpoint;
- separate programs or images per plane;
- renaming the planes;
- the balancer, the orchestrator and how many instances of each plane a fleet runs.

## Latitude

None.

## Alternatives considered

- **A routing table of which instance holds which Agent.** An event would reach only the instance
  that needs it. The table must be written on every connect and reconnect, and it goes stale when
  an instance dies; a missed route is a missed revocation. A broadcast costs every instance one
  look per event.
- **Presence on every heartbeat.** It keeps `last_seen_ms` exact, at about 3 300 writes a second
  for 100 000 Agents, for a value the view shows to the second.
- **Shared rate limits in the database.** A token per message in PostgreSQL turns every message
  into a write. Per-instance buckets with source-address affinity keep the limit effective for the
  peers it is meant for.
- **An external message bus** (NATS, Redis). It is one more component to run and secure, and
  `NOTIFY` already commits with the change it announces, so no event leaves before its data.
- **A designated leader that alone handles events.** It brings back the single point of failure
  this ADR removes. What must not run twice at once runs under an advisory lock that any instance
  can take: the sweep of unreferenced blobs, the removal of stale staging objects, and pruning the
  closed audit chains.
- **Keep serving through a database outage.** An instance would admit with the revocation list it
  last saw and could not learn of a new revocation. Clause 3 fails closed once the lease lapses.
- **Duplicate detection across instances.** It would rekey an Agent that only reconnected to
  another instance after a lost connection, before the old instance noticed. The newest connection
  takes the Agent instead.
- **An update replaces every instance: the instances of the old version stop before the first of
  the new one starts.** Simpler, since no instance ever meets a record or a notification of the
  other version. But an update would stop the whole fleet's Server for as long as the new version
  takes to start. Clause 16 lets two consecutive versions serve one database side by side.
- **No instance holds a CA key on the Server, rather than in a file.** While a file is the only
  form of a CA key, the two rules are one. The wider rule would also forbid a key every instance
  reaches without holding it in a file. Clause 2 forbids the file.
- **A notification drops the cached entry, and its next use reads it again.** The revocation list
  ends the sessions it concerns as it changes
  ([SEC-admission-by-client-certificate §32](SEC-admission-by-client-certificate.md)). A list read
  only on its next use would leave a revoked session open until some admission or request reads
  it. Clause 12 reads again at once what a notification concerns.
- **A kept put-back follows a re-key through another instance.** The instance that keeps it cannot
  learn the new identity. The store records no move, a notice names the old identity alone
  (clause 9), and only the instance that made a re-key holds both. A record of the move, a notice
  naming both, or the put-back on the Agent's row would each need a migration or a wider notice.
  The case stays not seen and is logged (clause 8).
- **Leave a group to its next placement.** No notice would name a binding of a group's Host or a
  Client Agent's first arrival over a Gateway. A fellow Agent would inherit a Client Agent bound or
  taken through another instance only once the group is placed again for a reason of its own: a
  label change, a report that changes, a missed notification. Its effective description in that
  instance's memory would lag behind the fleet view for as long, and the operator manual would have
  to explain the lag. The two notices cost one `NOTIFY` each after a commit that widens nothing an
  instance admits (clause 9), and clause 8 places the group again when they arrive.
- **Carry the Client Agent's labels in the notice.** No read would follow the notice. But the notice
  would become a carrier of data, which clause 9 keeps out of it: the notice names a kind and an
  identifier, and what changed is read.
- **Separate programs per plane.** An OpAMP server and a control server isolate the planes as a
  role per instance does, but add a second artifact, image and release path, for no isolation one
  program cannot give. Rejected, as SRV-two-use-case-layers rejects it.
- **A command-line flag**, such as `serve --plane agent`. The role would stand in the start
  command rather than in the configuration, against
  [SRV-configuration-toml-and-environment §1](SRV-configuration-toml-and-environment.md), which
  configures the Server from TOML and its environment, and the startup log of keys would not show
  it. Rejected.
- **The role read from the listen keys**, a plane opened only when its address is set. It needs no
  new key, but both addresses have defaults today, and an Operator plane that disappears because a
  key is missing is a surprise. Rejected.
- **Each instance requires and checks only the keys of the planes it opens.** An instance of the
  Agent plane alone would need no Operator-plane certificate or credential. But a table of which
  key binds which plane would have to follow every new key. A configuration one instance accepts
  could also fail another, or the same one once its role changes. Rejected; clause 25 has every
  instance check the whole configuration.
- **A database pool, or a runtime, of each plane's own inside one instance.** It keeps a
  reconnecting fleet from starving the operator of connections on every instance, without roles.
  It does not isolate memory, CPU or the loss of a host, and adds a second pool size to set.
  Rejected for now; it stays open (see *Out of scope*).
- **A health endpoint.** A probe could ask whether the lease is held rather than whether a port is
  open. The closed listener of a lapsed lease already answers that (clause 3), and an endpoint is a
  route more on a plane. Rejected;
  [OPS-releases-image-probed-by-plane §17](OPS-releases-image-probed-by-plane.md) leaves it to a
  later decision, and that ADR's *Alternatives considered* and *Consequences* defer it too.
- **No decision.** Every instance keeps serving both planes; the operator sizes the instances for
  the reconnecting fleet and the console together, and a heavy read or a reconnect storm reaches
  both. Rejected.

## Sources / Prior art

- PostgreSQL documentation: [`NOTIFY`](https://www.postgresql.org/docs/current/sql-notify.html)
  (delivered on commit, only to sessions listening at that moment, payload below 8000 bytes) and
  [advisory locks](https://www.postgresql.org/docs/current/explicit-locking.html#ADVISORY-LOCKS).
- [Patroni](https://patroni.readthedocs.io/) and [CloudNativePG](https://cloudnative-pg.io/) for
  synchronous replication and failover run beside the application.
- The code: `AppState` and its `push` channel in `crates/fleet-server/src/fleet.rs`; the duplicate
  detection in `crates/fleet-server/src/fleet/session.rs` and
  `crates/fleet-server/src/fleet/held.rs`; the session guard in
  `crates/fleet-server/src/transport.rs`; `subscribe` in `crates/fleet-server/src/revocation.rs`
  and `crates/fleet-server/src/enrolment.rs`.
- The specification's goal G-20 and its success criteria.
- The code as it stands: `instance::serve` and `Planes` in `crates/fleet-server/src/instance.rs`,
  which bind, gate and drain both planes together; `pool_size` in
  `crates/fleet-server/src/config.rs`; the signing of a CSR in
  `crates/fleet-server/src/fleet/approval.rs` and of a bootstrap certificate in
  `crates/fleet-server/src/bootstrap.rs`.
- Kubernetes, [Configure liveness, readiness and startup
  probes](https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/):
  a TCP probe per container port, which clause 3 sets per role.

## Consequences

- Positive: G-20. Losing an instance loses its sessions, whose Agents reconnect to another, spread
  out, and the fleet is served on. Capacity grows by adding instances.
- Positive: no routing state to keep correct. Every instance runs the same code on the same data.
- Positive: a floor set from a stale view cannot lower the stored one (clause 19). A floor stored
  too low would still not lower a Client: each Client keeps the highest floor it verified
  ([PKG-versions-high-water-mark-floor §17](PKG-versions-high-water-mark-floor.md)). The harm of a
  floor served late is therefore bounded by the Clients that never saw the higher one.
- Positive: the Agent plane and the Operator plane can each run on instances of their own, scaled
  and kept available apart. A reconnecting fleet does not slow the instances an operator acts
  through, and a heavy Operator-plane read does not slow the sessions.
- Positive: the Operator plane can run in an internal network only.
- Positive: one `server.toml` serves every instance, and a role changes by one key alone.
- Positive: nothing changes for an installation that sets no `planes`.
- Negative / trade-offs: a database outage longer than a lease stops admission on every instance.
  That is the price of never admitting against a stale revocation list.
- Negative / trade-offs: `last_seen_ms` in the view may be one presence interval old, and the
  staleness budget grows by that interval.
- Negative / trade-offs: every instance wakes on every event, and an instance whose listening
  connection drops re-reads everything it holds.
- Negative / trade-offs: a peer spread across instances meets a rate limit per instance. The
  manual's source-address affinity is what keeps the limit as tight as on one instance.
- Negative / trade-offs: the one database and the one object store are what every instance stands
  on. G-20 holds only while they are made redundant, which is the operator's (clause 17).
- Negative / trade-offs: a Server that holds a CA key in a file runs alone. Several instances serve
  a fleet whose certificates are issued outside the Server, or by keys every instance reaches,
  which are a decision of their own. A Server with a CA key in a file gains a standby at most: an
  instance its service manager starts again until the database has ended the session of the one it
  replaces, whose lease it then takes over (clause 2).
- Negative / trade-offs: every schema change takes two releases. An instance does not start while a
  lease is live whose release is older than the oldest Server version its migrations leave, so an
  upgrade that skips a release waits until those leases are gone (clause 16). The upgrade onto the
  first release of several instances stops every Server.
- Negative / trade-offs: cloned machines that share an `instance_uid` are rekeyed when both reach
  one instance, and only logged as a likely duplicate when they reach two (clause 8).
- Negative / trade-offs: a load balancer that rewrites the peer's address cannot be used in front of
  the Agent plane (clause 15).
- Negative / trade-offs: a fleet with roles needs at least two instances of each plane to keep
  both available, so four instances where two served before.
- Negative / trade-offs: an instance that must be alone opens both planes (clause 23). An
  installation with a CA key in a file, or with blobs on the filesystem, therefore runs no roles.
- Negative / trade-offs: one more key, and a probe that depends on it; a lease record with a new
  column, which a migration adds.
- Negative / trade-offs: every instance needs every file its configuration names, those of the
  plane it does not open among them (clause 25).
- Negative / trade-offs: a fleet takes up roles in two steps. Every instance first runs a release
  that knows `planes`, and only then may the operator set it (clause 22).
- Follow-ups: the operator manual's pages on replication, load balancing and rolling updates; CA
  keys every instance can reach; a pool of each plane's own, should combined instances need it; a
  health endpoint.

## Enforcement

The tests of the carried clauses carry `Verifies: SRV-instances-share-one-postgresql` today. They
will carry `Verifies: SRV-instance-opens-chosen-planes`, their markers moving on acceptance. They
run against the PostgreSQL that the `FLEET_TEST_POSTGRES_*` variables name
([SRV-state-stores-contract-pair §25](SRV-state-stores-contract-pair.md)). They run with two
instances in one test process, or three where a bullet says so. Most stand in
[`crates/fleet-server/tests/instances.rs`](../../crates/fleet-server/tests/instances.rs),
[`crates/fleet-server/src/fleet/held.rs`](../../crates/fleet-server/src/fleet/held.rs) and the
adapters under [`crates/fleet-server/src/postgres/`](../../crates/fleet-server/src/postgres/). They
decide:

- A rollout on instance A reaches an Agent connected to instance B. A revocation on A closes B's
  session of that certificate with `1008` and `revoked`.
- An enrolment approved on A is offered on B's enrolment connection.
- A fellow Agent B placed before A bound the Host's Client Agent inherits that Client Agent's
  labels in B's memory once the binding's notice reaches B. Afterwards A takes a Client Agent behind
  a Gateway. A fellow Agent behind that Gateway that B placed inherits its labels once that notice
  reaches B (clauses 8 and 9). A Client Agent that first arrives over a Gateway's
  Host is announced after its commit.
- Every line A and B log carries the instance id of the one that wrote it. When A takes over B's
  lapsed lease, A logs its own id and B's. A Server on the filesystem store logs no instance id.
- B answers `304` to the `ETag` of the Gateways' revocation list that A answered. A revocation on
  either moves it on both.
- When B's lease lapses, B's Agents read as disconnected through A's Operator plane. B ends its
  sessions and refuses TCP connections on the planes it opens until it holds a new lease, then
  accepts them again. With its renewals held back, B stops serving no later than A reads its lease
  as lapsed.
- An instance whose clock jumps past its lease's lapse while it was not running answers its next
  request `503`, closes the listeners it opens, and opens them again under a new lease. A message
  its open session carries then is not answered. While the lease is lapsed by the instance's clock,
  a request on a plane it opens and an Agent's message are answered `503`, though no listener is
  closed yet. Nothing is sent to an Agent unasked then: a restart requested meanwhile stays queued.
- A queued restart whose command a WebSocket connection does not take, or whose plain-HTTP answer
  is past the size limit, stays queued, and the Agent's next connection is sent it. A put-back the
  store refuses is tried again, at growing intervals, until it holds.
- A Gateway whose pool reaches both A and B keeps the holder of each downstream Agent unchanged
  while the Agent's pool connection lives.
- When A stops, B takes over each Agent behind that Gateway that A held once, at its next report,
  and the Agent stays connected to the Gateway. The Gateway's revocation list fetched from B is
  answered `304` and stays trusted, and its floors are served on.
- During the 10 000-Agent failover below, no admission throttle refuses a Gateway's ten pool
  connections from one address. The bound on new connections refuses them no more than any peer.
- A's Operator plane names B as the holder of an Agent B carries, with B's host name. It names none
  once B's lease has lapsed. `holder=<B>` lists exactly the Agents B holds.
- An Agent behind a Gateway that B carries inherits in B's memory the labels of its Client Agent,
  which A carries.
- On `SIGTERM` an instance refuses new connections at once and answers a request in flight. It
  closes its WebSocket sessions with `1001` spread over `drain_secs`. It exits once its presence,
  its chain's close and its lease's release are written. `drain_secs` outside 0 to 300 fails
  startup naming the key.
- After B's listening connection is dropped and restored, a revocation committed in between still
  ends B's session.
- A second instance fails startup naming the key on a database where the first runs with
  `[blobs] store = "fs"`, with a `[client_ca] key_file`, or with an
  `[enrolment] bootstrap_ca_key_file`. So does an instance with either of them on a database where
  another instance holds a live lease. An instance that must be alone and loses its lock's
  connection logs it, ends its sessions and refuses every call.
- Two instances revoking two certificates at once leave both revoked. Two instances lifting and
  adding revocations at once leave the list both acts decided.
- A certificate renewed on A, whose predecessor is revoked later on A, is refused on B, whose
  register held neither the renewal nor the revocation before.
- Two instances binding one `instance_uid` to two Hosts at once leave one binding, and the second
  instance decides by it.
- Two instances queueing an enrolment request each keep both requests. The queue's bound counts the
  requests of every instance.
- B serves a floor set on A at once. B refuses a floor below the stored one, though B's memory holds
  an older, lower floor.
- Two instances saving one Configuration at once keep both revisions. Two instances uploading two
  entries of one Package keep both. Two instances adding two members to one Deployment keep both.
- B does not delete a Configuration whose revision a rollout through A pins, though B's memory
  holds no such pin. B neither changes nor deletes a Package A pins, and does not delete a
  Deployment A released through. A deletion through B while A's rollout has not committed waits
  for it and is then refused.
- A Configuration or a Package deleted through B is refused when A adds it to a Deployment before
  taking up the deletion. An add through A that holds the Deployment while B deletes the member
  leaves no Deployment naming it.
- B's sweep keeps a blob that an entry written through A names, though B has not read the entry.
  The staging object of a lease released at an orderly shutdown goes with the next sweep on any
  instance. Two instances sweeping at once delete it once.
- One hundred heartbeats of one Agent within one presence interval write its presence once.
- An Agent that reconnects from B to A is held by A, and B stops carrying it.
- An Agent no instance carries is in no instance's memory. A's Operator plane lists it, with its
  record and presence, from the database. A Server on the filesystem store lists the same Agent
  from memory.
- Labels and an assignment set through A on an Agent B carries survive B's next write of that
  record while A's notification is held back from B. B holds them once it arrives.
- A notification is never received before the change it announces can be read. A change whose
  transaction rolls back sends none.
- A binding a report makes, and a Host row a report, an issuance or a prune makes or moves forward,
  are announced after their commit:
  - another instance hears of them;
  - one whose notice was lost between the commit and the `NOTIFY` holds them after its next lease
    renewal;
  - a notice that fails is sent again, and the write answers as made.

  Every notice sent after a commit may be lost. Another instance still hears each of these: a
  revocation, a release, a re-key, the release of every binding of a Host, a Gateway's mark, a Host
  forgotten, a register entry.
  A call that fails after a binding has its instance read the ledger again at once.
- A Gateway's mark through A of a Host B wrote, which A has not taken up, is written. A then holds
  the Host as stored, and B holds the mark once it hears of it. A host neither the Hosts nor the
  register names is refused through any instance, one that still holds it in memory included, and
  nothing is written.
- A notification of a Configuration, a Deployment, a Package or a revocation has the instance read
  the database's again at once.
- After B's listening connection is restored, B holds the labels set and the Agents taken in
  between. An approval decided in between reaches B's enrolment connection. A notification of an
  unknown kind makes B re-read as after a lost connection.
- Two instances taking one Agent at once leave it carried by one of them, never by none.
- An instance whose take of an Agent another instance's take overtakes, before it holds the Agent in
  memory, stops carrying it once it hears of that take.
- A report whose call to the database is held leaves the fleet lock free. Another Agent's report is
  answered meanwhile, and another peer's TLS handshake completes.
- An Agent whose report's call to the database is held is answered a ping on the same connection
  meanwhile.
  - The messages of one connection are handled one at a time and answered in the order they
    arrived.
  - A connection with eight messages, or the message limit in bytes, waiting is read no further
    until one is handled.
  - Of the 10 000-Agent fleet below, connecting locally, every Agent is held within 60 seconds of
    the last connection opened, at least 150 new Agents a second. None is re-keyed as a duplicate,
    as an Agent whose connection was ended for silence is.
- Many Agents created at once through twelve instances leave no more records than `max_agents`.
- A presence row ends with its connection. The row names no holder once the Agent disconnected and
  the next batch ran, for each of these Agents:
  - one whose take's answer was lost;
  - one re-keyed;
  - one taken by B and disconnected there before A hears of the take, whose holder's lease may have
    lapsed;
  - one whose batch is held between its commit and its answers while it is taken again.

  An Agent that disconnected under a lease that was then lost has its record flushed at shutdown.
- B carried an Agent and wrote its disconnect. A then carries it with a take that was not written.
  The Agent keeps the report A wrote at B's shutdown flush. That holds also where the record was
  forgotten and created again through B. It holds where B still holds the row with a live lease as
  it stops.
- An Agent may start again with lower sequence numbers, and B's flush may write its older record
  over A's report. The Agent has A's record again at A's next report, whether or not B still holds
  the row.
- An Agent whose holder changes eleven times within a minute is logged once as a likely duplicate.
- A peer of ten Agents whose first reports wait longer than one heartbeat interval keeps its Agents'
  identities when it comes back on a new connection.
  - The older connection's reports and goodbye do not take them back or end them while the newer
    one carries them.
  - A duplicate on a connection of another peer, or of a connection that presented no host, is
    re-keyed.
  - A queued restart is handed out, in a reply or sent unasked, only on the connection that carries
    the Agent.
  - An Agent on two connections of one Gateway may move to the older one. Its report there takes it
    once the newer one has carried none of its reports for a heartbeat interval, and not before.
    Its goodbye there ends it, and it is asked for its component map there.
- A re-key, a revocation, a label and a floor whose commit lands after the call's answer was lost
  are held in the instance's memory once the commit lands.
  - The re-keyed Agent is one row under its new identity, and no record is created for its old one.
  - A re-key whose read back failed keeps the Agent's session in three cases:
    - when its own notification arrives;
    - when its read back there fails first;
    - when a report or a rollout finds the old row gone.
  - It writes the disconnect of a session that ended meanwhile to the new row. It assigns the same
    new identity at the next report.
  - A report under the old identity is refused as long as the move is not known.
  - A notification of an earlier call of the instance's own is not taken for that of a call whose
    answer was lost.
  - The read back of a take still in flight waits for it, so the row ends with the connection once
    the Agent disconnected.
- On an orderly shutdown an instance writes the presence of its Agents and releases its lease.
  Another instance reads them as disconnected at once.
- An instance that must be alone, started within `lease_secs` of the crash of the process it
  replaces, takes that process's lease over and starts.
- Instances of version N and N+1 on one database both serve the Agents that report to either. A
  notification of a kind only N+1 sends makes the instance of N re-read as after a lost connection.
- An instance fails its start, naming both versions and leaving the schema as it was, in two cases:
  - on a schema whose oldest Server version is newer than its own;
  - while a live lease names a release older than the oldest Server version the schema names once
    its own migrations are applied, whether those migrations are applied already or not.

  A lease of 0.8.0 records 0.8.0 by the default. A lease clause 2 takes over counts as none. With
  no such lease the instance migrates the schema it finds and starts.
- An instance of N that starts while one of N+2 migrates either is seen by it, which then fails, or
  sees N+2's schema and fails. An instance of N whose lease lapsed while N+2 migrated stops when it
  takes its new lease
  ([`stateless_instance.rs`](../../crates/fleet-server/tests/stateless_instance.rs)
  `an_instance_whose_lease_lapsed_on_a_newer_schema_exits_naming_both_versions`).
- The two G-20 tests of [`g20.rs`](../../crates/fleet-server/tests/g20.rs) run three Server
  processes at the defaults, `max_connections` and `max_agents` raised for the fleet.
  - They run behind a layer-4 balancer of the test's own. It hashes the peer's address over the
    instances that answer its probe. It connects to the instance from the peer's own address, which
    on the loopback every test address is, so the instances see the peer's address.
  - Each simulated host has a certificate and an address of its own and carries `k` Agents on one
    connection through the `crates/opamp` client.
  - The tests name `k`, and the time the fleet took to connect, which neither bound counts.
- A fleet of 10 000 simulated Agents, one on each connection, across three instances, one of them
  killed (`SIGKILL`), is carried by the other two within `lease_secs` plus two heartbeat intervals
  (`g20_a_killed_instance_s_agents_are_carried_by_the_others_within_the_bounds`).
  - Each Agent taken over costs one holder write, and no notification is left unread once they are
    carried.
  - A revocation committed during the takeover ends its session within `lease_renew_secs`.
  - No Agent of the stopped instance reconnects before the delay clause 20 drew for it.
  - The test names its fleet size, its instance count and its timings, and fails when a bound is
    missed (G-20).
- The presence batches and notifications of 100 000 simulated Agents run for ten minutes
  (`g20_three_instances_carry_the_presence_and_notifications_of_100_000_agents`).
  - The Agents report at the default heartbeat, ten on each connection, so that one machine holds
    the simulated fleet. That is about 3 300 reports a second, across three instances on one
    database.
  - No presence batch is written later than its interval after it was due, and no notification is
    left unread (G-20).
  - Meanwhile one Agent in a hundred reconnects each minute, and labels are set on 100 Agents every
    five seconds. Once a minute a Configuration is rolled out to 10 000 Agents and a certificate is
    revoked. A page of the Agents is read every ten seconds.
- `[database] lease_secs`, `lease_renew_secs` or `presence_interval_secs` with `store = "fs"` fails
  startup naming the key.
- When B stops without an orderly shutdown, A takes over B's lease once it lapsed. A closes B's
  chain with `audit.chain_closed`, `reason` `lease_lapsed` and `closed_by` A's chain.
  `audit-verify` reports B's chain as closed, and the store refuses an entry B appends to it
  afterwards.

Tests that will carry `Verifies: SRV-instance-opens-chosen-planes` decide clauses 22 to 26, and
what clauses 1 and 3 say of the planes an instance opens:

- The configuration's tests:
  - An empty `planes`, an unknown name and a duplicate fail startup. Without the key both planes
    are opened (clause 22).
  - `FLEET_SERVER__PLANES='["agent"]'` in the environment opens the Agent plane alone, as
    `planes = ["agent"]` in `server.toml` does (clause 22).
  - A `planes` without both fails startup on the filesystem store. It fails too on PostgreSQL with
    `[blobs] store = "fs"`, with a `[client_ca] key_file`, or with an
    `[enrolment] bootstrap_ca_key_file`. Each message names `planes` and that key (clause 23).
  - An instance with `["agent"]`, and one with `["operator"]`, refuses each invalid configuration
    that an instance with both planes refuses, with the same message (clause 25).
  - Among these configurations are one without `[tls]`, one whose `[rest.tls]` key file cannot be
    read, and one with `[rest] listen` off the loopback and no `[rest.auth]`.
  - Each kind starts with the same valid configuration as an instance with both planes. Its
    startup log names the plane it opens and the plane it leaves closed (clause 25).
- An instance test on PostgreSQL:
  - With `["agent"]` nothing listens on the Operator plane's address. With `["operator"]` nothing
    listens on the Agent plane's.
  - A lapsed lease closes only the listeners opened, and a drain ends only those (clauses 1 and 3).
- A test of the composition root decides clause 24:
  - An instance without the Agent plane builds no `AgentCases`. It carries no Agent and writes no
    presence batch.
  - An instance without the Operator plane builds no `OperatorCases`.
  - Both kinds hold a lease and an audit chain, and take over a lapsed lease.
- An instance test on PostgreSQL runs one instance of each plane. A revocation committed through
  the Operator-plane instance ends the session on the Agent-plane instance within one lease
  renewal interval, as G-20 asks.
- A G-20 test in [`g20.rs`](../../crates/fleet-server/tests/g20.rs) runs another topology than the
  two G-20 tests above, whose arithmetic clause 20 states:
  - Three Server processes open the Agent plane alone, and a fourth opens the Operator plane
    alone. They carry the 10 000 simulated Agents of the failover test.
  - One Agent-plane instance is killed (`SIGKILL`), and its Agents reconnect to the other two.
  - The window opens with the kill. It closes once the lost instance's Agents are all carried
    again.
  - In the window the test walks the list of the Agents through the Operator-plane instance, 100
    Agents a page. A walk reads the list from its first page to its last.
  - In the same window it sends revocations to the Operator-plane instance.
  - Each page must be answered `2xx` with its entries within 2 seconds.
  - Each revocation must be committed and answered `2xx` within 2 seconds.
  - At least one whole walk and one revocation must complete inside the window. The test fails
    when no whole walk or no revocation does.
  - The test fails when any of these is missed (G-20).
- A lease test:
  - An instance logs the warning of clause 26 while no live lease serves a plane. It does not log
    it again until one has served it.
  - A lease of a release that records no planes counts as serving both.

**Not mechanically decidable:** the operator's side of clauses 14, 15 and 17: the balancer's
affinity and its preservation of the peer's address, the database's synchronous replication and
its failover. The Server cannot see them. The manual states each as an operating requirement, and
the load test above runs with a balancer that preserves the address. Clause 16 is decided at each
instance's start and at each new lease, by the schema and by the releases the live leases record.
Clause 3 names the probe the documentation tells an operator to use, and review holds it. The
instance tests above hold that an instance listens on the port of each plane it opens and on no
other. Clause 22 says that the operator must not set `planes` before every instance of the fleet
runs a release that knows it. The Server cannot decide it. The manual states it, and review holds
it.
