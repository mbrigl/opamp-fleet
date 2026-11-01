# Spec: Several Server instances serve one fleet

## Problem

An operator runs one Server process for the whole fleet. When its host fails, is patched or is
replaced, every Agent loses its Server until the process is back: no configuration moves, no
enrolment is decided, no revocation reaches a session. One process is also one machine's worth of
connections, and a fleet that grows towards 100 000 Agents outgrows it. Running a second Server
against the same database today fails at startup, because one Server holds the schema alone.

## Solution

An operator starts as many Server instances as the fleet needs against one PostgreSQL database and
one S3-compatible bucket, with the CA keys on a network HSM or not on the Server at all, behind a
layer-4 balancer that keeps the peer's address. Any instance admits any Agent, serves any
Operator-plane call, and takes over the Agents of an instance that is lost. A revocation made on one
instance ends the sessions it concerns on every instance within one lease renewal. A rolling update
replaces the instances one by one while version N and N+1 share the database. The single Server, on
files or on PostgreSQL, works as it does today and gains the per-row writes, the paging and the
spread-out reconnect along the way.

## Existing behaviour

- One Server per database schema holds a lock while it runs; a second one fails startup naming the
  database, and a Server that loses the lock logs it and writes nothing more. Pinned by
  `a_second_server_against_the_same_database_fails_startup_naming_it` and
  `a_server_that_loses_its_lock_logs_it_and_writes_nothing_more`. Both change: the lock becomes the
  exclusive lock of an instance that must be alone, and the lease takes the place of the lock for
  every other instance.
- The Server loads the whole fleet and every store into memory at startup and saves several of them
  whole. The port contracts pin the behaviour that must stay for both stores:
  `the_ledger_port_contract_holds_on_the_filesystem` and `…_on_postgres`, likewise for the
  enrolment, floor, configuration, package, deployment and agent ports. The whole-or-nothing tests
  (`a_save_that_fails_part_way_leaves_nothing_of_itself`,
  `a_put_that_fails_part_way_leaves_nothing_of_itself_on_postgres`,
  `a_package_put_is_whole_or_nothing_on_the_filesystem`,
  `a_rollout_that_fails_leaves_neither_revisions_nor_assignments_on_postgres` and their siblings)
  keep holding when the writes become per row.
- The schema is applied by the Server and a newer one is refused:
  `the_server_applies_its_schema_and_refuses_a_newer_one`,
  `the_migrations_are_numbered_and_this_server_understands_them`, `a_migration_only_adds`. The
  refusal widens to "older than expected or newer than the next".
- The Operator plane answers its lists as whole arrays; the REST tests in the Server's integration
  suite pin the shapes, and a request without `limit` keeps answering the whole array up to a bound.
- The audit record is one hash chain in files under the data directory, on either store; its
  rotation and `server audit-verify` are pinned by the audit writer's tests. On the filesystem store
  this stays as it is.
- The Client reconnects with exponential backoff from one second with equal jitter, pinned by the
  backoff tests of the protocol crate; the first step changes to a draw from zero.
- The Server image sets the data directory in its environment; the Containerfile check pins the
  variables it may set, and it loses that one.
- Shutdown is one bounded drain for both planes (`shutdown_signal`); it gains the spread close of
  the sessions.

Existing data moves by what is already there: a Server on files moves to PostgreSQL offline with
`server migrate-to-postgres`, which records the end of the file audit chain so that the database's
first chain names it; nothing is moved online.

## Goals served

- G-20 — the fleet is served on when a Server instance is lost.
- G-19 — the Server's state lives outside its host; this work makes that state the only thing the
  instances share.
- G-17 — the connection stays secured and the Agent identified across instances, revocation
  included.
- Q-1 — secure by default: an instance that cannot prove its lease serves nothing, and no instance
  writes beside another without a transaction.
- Q-4 — a host receives only what is released to it, whichever instance answers.
- Q-6 — no downgrade: a floor is never lowered by an instance with a stale view.

## User stories

1. As an operator, I want to start a second Server instance against the same database and bucket, so
   that the fleet keeps being served when one host fails.
2. As an operator, I want an instance that loses its database to stop serving and say so, so that no
   Agent is admitted against a revocation list it cannot see.
3. As an operator, I want a revocation made on any instance to end the session it concerns on every
   instance within seconds, so that a stolen certificate is useless everywhere at once.
4. As an operator, I want the Agents of a lost instance to come back spread out over half a minute,
   so that the remaining instances are not flattened by the reconnect.
5. As an operator, I want an instance on PostgreSQL, S3 and a token to need no writable disk, so
   that it runs as a stateless container with read-only secrets.
6. As an operator, I want to replace instances one by one during an update, so that an update needs
   no fleet-wide outage.
7. As an operator, I want two operators acting on two instances at once never to lose each other's
   revocation, label, floor or revision, so that what I did stays done.
8. As an operator, I want the fleet list to page and filter on the Server, so that the UI stays
   usable at 100 000 Agents.
9. As an operator, I want to see which instance holds an Agent and to filter by it, so that I can
   tell what a lost instance took with it.
10. As an operator, I want the audit record to stay one verifiable record across instances and
    across the move from files to the database, so that no decision goes unrecorded.
11. As an operator, I want the CA keys on a PKCS#11 token every instance reaches, so that every
    instance can sign renewals and no key sits on an instance's disk.
12. As an operator, I want a Gateway's downstream Agents to stay put while its connections live and
    to move once when an instance is lost, so that a Gateway never makes Agents flap.
13. As a single-Server operator, I want nothing of this to change what I run, so that I can ignore
    it until I need it.
14. As a maintainer, I want a test that kills an instance under 10 000 simulated Agents and one that
    carries 100 000 Agents' presence, so that G-20 is decided by a test and not by belief.

## Implementation decisions

- Every Server on PostgreSQL is an instance: a random instance id per start, a lease taken before
  either listener opens, renewed every 10 s and lapsing after 30 s. An instance that must be alone
  (file blobs, file CA keys) additionally holds an exclusive lock; the schema lock of today becomes
  that lock.
- Without a lease it can prove, an instance closes both listeners; on SIGTERM it drains over
  `drain_secs`, closing sessions spread over that time, then writes presence, closes its audit chain
  and releases its lease.
- Every act on shared state is one transaction on the rows it names, under a row lock where it
  decides from what is stored: certificates, revocations, Hosts, enrolment, floors, Configuration
  revisions, Package entries, Deployment members. Bounds count the whole fleet.
- The fleet record has two modes behind its port: whole in memory on files, per Agent on PostgreSQL.
  A report writes only what a report decides; labels, a requested restart and a rollout write their
  own columns. Presence is written in batches.
- Every change that concerns a live session is one `NOTIFY` on one channel in the same transaction.
  A lost listening connection or an unknown kind counts as every notification. Caches are dropped,
  never patched.
- An Agent's holder is its presence row's instance; the newest connection takes an Agent by a
  conditional update. A Gateway keeps each downstream Agent on one pool connection, so the Server
  needs no Gateway rule.
- The Operator plane reads the fleet from the database on PostgreSQL, pages four lists by cursor and
  filters on the Server, `holder` among the filters.
- The audit record is one chain per lease on PostgreSQL; the instance that takes over a lapsed lease
  closes its chain; a record moved from files stays linked through `predecessor`.
- With PostgreSQL and S3 there is no data directory; upload staging is keyed by lease; the image
  stops setting the data directory.
- Reconnects follow the restated transport rule for every Server: a first reconnect drawn over [0,
  30 s), a new-connection bound of 200 a second answered with a spread `Retry-After`, honoured by
  both transports.
- Schema changes are expand-then-contract over two releases, applied under an advisory lock.
- The CA keys and the audit key may live on one PKCS#11 token; SoftHSM2 is the planned one and
  serves one host.
- New glossary terms land with the ticket that settles them: Server instance, lease, holder.

## Testing decisions

- The port contracts stay the seam for the stores: every per-row change is a new case in the
  contract both adapters run, and the PostgreSQL side runs through the existing store check against
  a real database.
- Coordination is tested with two or three instances in one test process against one database,
  through the Server's real listeners, copying the prior art of the transport and mutual-TLS
  integration tests; a notification held back is the seam for the races.
- The Client's reconnect is tested at the protocol crate's backoff and WebSocket driver, as its
  backoff tests do today.
- G-20 is decided by two tests in a workflow of their own, weekly and on demand: a failover with 10
  000 simulated Agents on three instances, and 100 000 Agents' presence and notification load for
  ten minutes. They need PostgreSQL, S3 and a layer-4 balancer that keeps the peer address; locally
  one command starts them. An ordinary `check-all` skips them, saying so, as it skips the service
  smoke test.
- Every test of a draft clause carries `Verifies: ADR-draft-<slug>`, as the drafts' Enforcement
  sections name them; the marker becomes the number when the draft is numbered.

## Out of scope

- OIDC on the Operator plane, the MCP endpoint, the two use-case layers and the restatement of the
  operator program; they are drafts of their own.
- Backing up and restoring PostgreSQL and the bucket, and making them redundant: the operator's. The
  manual says what to do with revocations after a restore.
- Kubernetes manifests, a Helm chart, several regions or databases.
- Sharing rate limits across instances, a routing table, messages addressed to one instance.
- Rekeying cloned Agents across instances; they are logged.
- **Open question:** whether an Agent should download a Package directly from the bucket rather than
  through the instance. No draft decides it; it needs an ADR before anyone builds it.

## Stages

The plan lands on `main` in three stages, each a release of its own; the tickets of a later stage
go on on this branch.

1. **Failover on PostgreSQL and S3.** Tickets 01 to 06, 08, 09, 10, 13, 18, 19, 20 and 21, and 22,
   which states the stage's bounds. Several instances share one database and bucket, every change
   is decided in its transaction and reaches every instance, the newest connection takes an
   Agent, the audit record is one chain per lease, and an instance needs no disk. Its bounds:
   instances are upgraded together, since two versions side by side are stage 2's; no load is
   proven; and with a CA key as a file on the Server an instance runs alone, so failover is a
   standby that takes over once the lease has lapsed. The ADR drafts are split for it: the clauses
   it implements are numbered and accepted, the rest stay drafts on this branch.
2. **G-20: the fleet at scale, and an upgrade without a stop.** Tickets 07, 11, 14 and 15. The
   lists page, two versions run side by side, and the load test decides G-20, which lands with it.
3. **The keys on a token.** Tickets 12, 16 and 17. The CA keys live on a PKCS#11 token, so
   instances holding a CA run side by side, the heads of the audit chains are signed, and the
   record may be exported.

## ADR impact

- Binding ADRs in force: 0002 (tests cite decisions), 0006 (this plan), 0007 (the harness: nothing
  is worked unattended), 0008 and 0009 (crates, ports and adapters), 0012, 0015, 0021, 0022, 0023,
  0024, 0026, 0027, 0028, 0029, 0030 and 0034.
- Implemented here, all proposed and released by the human for implementation:
  ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it,
  ADR-draft-the-audit-record-is-one-hash-chain-per-lease-of-a-server-instance,
  ADR-draft-an-append-only-audit-record, ADR-draft-the-fleet-record-of-agents-and-hosts,
  ADR-draft-the-servers-state-and-its-stores,
  ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server,
  ADR-draft-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one,
  ADR-draft-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token,
  ADR-draft-tls-1-3-two-planes-and-bounded-input,
  ADR-draft-admission-by-client-certificate-and-revocation and
  ADR-draft-releases-attestations-and-the-server-image. Each restatement supersedes its accepted ADR
  when the human accepts it.
- The specification change G-20 is drafted on the drafts' branch; only the human lands it.
- No new ADR is needed beyond these. The instance id in the Server's own log lines is decided in
  clause 1 of the instance draft.
