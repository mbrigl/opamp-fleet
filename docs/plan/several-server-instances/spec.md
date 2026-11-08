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

Several instances already share one PostgreSQL database and one bucket: each holds a lease, every
change is decided in its transaction and reaches every instance by a notification, the newest
connection takes an Agent, the audit record is one chain per lease, an instance on PostgreSQL and
S3 needs no disk, and a lost instance's Agents come back spread out. What the remaining work
changes, and what pins it:

- The Operator plane answers its four growing lists as whole arrays, and an Agent's view names no
  holder. The REST tests in the Server's integration suite pin the shapes, and a request without
  `limit` keeps answering the whole array up to a bound. On PostgreSQL the Operator plane reads the
  Agents from the database through the port's unpaged `query`, pinned by
  `the_query_reads_the_stored_presence_and_opening_reads_the_assignments_alone`; it stays.
- In the memory of the instance that carries it, an Agent behind a Gateway does not inherit the
  labels of a Client Agent another instance carries; the manual says so under "Labels on several
  instances".
- An update stops every instance: a Server refuses a schema that names a newer oldest Server than
  itself, and an instance that shares the database neither migrates nor starts while another Server
  holds the database alone. Pinned by `the_server_applies_its_schema_and_refuses_a_newer_one`,
  `a_server_of_0_7_1_refuses_the_schema_once_a_migration_of_0_8_0_is_applied` and
  `a_shared_instance_migrates_nothing_while_a_server_holds_the_database_alone`, which stay. The
  refusal widens to a start while a live lease names a release older than the one before the
  instance's own.
- A CA key is a file, and an instance with one runs alone, pinned by
  `an_instance_that_must_be_alone_is_alone`; it stays for key files.
- The audit record's chains are unsigned and leave the Server only as the log copy;
  `server audit-verify` checks the chains alone. A record without a token stays so.
- No load is proven on several instances; G-20 is drafted and lands with the test that decides it.

The single Server, on files or on PostgreSQL, keeps working as it does: the integration suite and
the store checks pass unchanged.

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
  filters on the Server, `holder` among the filters, through a read the fleet record's port gains
  beside its unpaged `query`.
- The audit record is one chain per lease on PostgreSQL; the instance that takes over a lapsed lease
  closes its chain; a record moved from files stays linked through `predecessor`.
- With PostgreSQL and S3 there is no data directory; upload staging is keyed by lease; the image
  stops setting the data directory.
- Reconnects follow the restated transport rule for every Server: a first reconnect drawn over [0,
  30 s), a new-connection bound of 200 a second answered with a spread `Retry-After`, honoured by
  both transports.
- Schema changes are expand-then-contract over two releases, applied under an advisory lock. Each
  lease records its release, 0.8.0 for a lease taken before the record existed. An instance
  refuses its start while a live lease names a release older than the oldest Server the schema
  names once the instance's own migrations are applied; there is no notion of a predecessor, so a
  patch release in between changes nothing. With no such lease, a single Server on PostgreSQL may
  skip releases as it does today (user story 13).
- The order of that check: an instance takes its lease first, then checks the schema and the live
  leases under the migrations' advisory lock, and checks the schema again whenever it takes a new
  lease after an outage. So an instance never serves on a schema newer than it understands, neither
  after a start race with a newer instance nor after its own lease lapsed.
- The CA keys and the audit key may live on one PKCS#11 token; SoftHSM2 is the planned one and
  serves one host.
- The glossary carries Server instance, lease and holder.

## Testing decisions

- The port contracts stay the seam for the stores: every per-row change is a new case in the
  contract both adapters run, and the PostgreSQL side runs through the existing store check against
  a real database.
- Coordination is tested with two or three instances in one test process against one database,
  through the Server's real listeners, copying the prior art of the transport and mutual-TLS
  integration tests; a notification held back is the seam for the races.
- The Client's reconnect is tested at the protocol crate's backoff and WebSocket driver, as its
  backoff tests do today.
- G-20 is decided by two tests in a workflow of their own, on demand: a failover with 10 000
  simulated Agents on three instances, and 100 000 Agents' presence and notification load for ten
  minutes. It runs weekly again once a measured run shows the rates hold on a 4-CPU runner, or on
  an 8-CPU runner; a hosted runner has the memory the check needs, but fewer CPUs. They need
  PostgreSQL, S3 and a layer-4 balancer that keeps the peer address; locally one command starts
  them. An ordinary `check-all` skips them, saying so, as it skips the service smoke test.
- Every test of a draft clause carries `Verifies: ADR-draft-<slug>`, as the drafts' Enforcement
  sections name them; the marker becomes the number when the draft is numbered.

## Out of scope

- OIDC on the Operator plane, the MCP endpoint and the two use-case layers; they are drafts of
  their own.
- Backing up and restoring PostgreSQL and the bucket, and making them redundant: the operator's. The
  manual says what to do with revocations after a restore.
- Kubernetes manifests, a Helm chart, several regions or databases.
- Sharing rate limits across instances, a routing table, messages addressed to one instance.
- Rekeying cloned Agents across instances; they are logged.
- **Open question:** whether an Agent should download a Package directly from the bucket rather than
  through the instance. No draft decides it; it needs an ADR before anyone builds it.

## Stages

The plan lands on `main` in three stages, each a release of its own.

1. **Failover on PostgreSQL and S3.** Tickets 01 to 06, 08, 09, 10, 13 and 18 to 22, done. Several
   instances share one database and bucket, every change is decided in its transaction and reaches
   every instance, the newest connection takes an Agent, the Operator plane reads the Agents from
   the database, the audit record is one chain per lease, and an instance needs no disk. Its
   bounds: instances are upgraded together, no load is proven, and with a CA key as a file on the
   Server an instance runs alone. Its decisions are the eight drafts it numbers and accepts.
2. **G-20: the fleet at scale, and an upgrade without a stop.** Tickets 07, 11, 14, 15, 34 to 41
   done, and ticket 09 of the design revision, a rejected key that stays refused beyond its
   window; no ticket is open for stage 2. Ticket 35 is closed as resolved by tickets
   36, 37 and 40: the load test at 450 connections a second and the failover on a host with every
   core busy hold exactly the Agents, and a row left held could not be reproduced. The failover
   counts the held rows of the Gateway's Agents and of the single-Agent hosts apart, and that
   count is the sensor for such a row. The lists page and name the holder, two versions run side
   by side, the manual runs three instances, and the load test decides G-20, which lands with it.
   Its ADRs are ADR-0025, ADR-0031, ADR-0015, ADR-0023, ADR-0028, ADR-0029 and ADR-0027.
3. **The keys on a token.** Tickets 12, 16 and 17. The CA keys live on a PKCS#11 token, so
   instances holding a CA run side by side, the heads of the audit chains are signed, and the
   record may be exported. Its four drafts are on the branch `feat/multi-instance-stage-3`,
   not on this one, so that the pull request of stage 2 numbers only its own:
   `0043-the-servers-signing-keys-may-live-on-a-pkcs11-token-with-softhsm2-as-the-planned-one.md`,
   `0041-the-heads-of-the-audit-chains-are-signed-with-a-key-on-the-token.md`,
   `0042-the-operator-program-opamp-fleetctl.md`, which supersedes ADR-0037, and
   `0040-admission-by-client-certificate-and-revocation-with-the-ca-keys-in-a-file-or-on-a-token.md`,
   which supersedes ADR-0026. They are accepted together.

## How the remaining work runs

- **What blocks.** A new finding blocks G-20, and the pull request of a stage, only when it
  concerns correctness or security: lost updates, revocations, the audit record, admission. A
  performance finding blocks only when the G-20 load target is missed without it. Every other
  finding becomes a ticket in a plan directory of its own, as `docs/plan/tests-wait-for-an-event/`
  is, so that this plan converges.
- **Review rounds.** Each ticket is worked in a worktree of its own and reviewed by a session that
  did not write it. A fix round gets another review only when it changed code for a blocker or a
  should-fix; a nit goes into the hand-off's doubts, and a ticket in review takes no new scope.
- **Open before stage 2 starts:**
  - Ticket 23: an approval whose decision fails to commit has already recorded its certificate,
    so approving the request again signs a second one; stage one ships it as a bound. It blocks
    G-20.
  - Tickets 24 to 32: the correctness findings of stage 1's hand-offs and reviews, on presence,
    deleted members, the sweep, the lease across a suspend, a commit whose answer is lost, one
    snapshot per read, a queued restart, the audit record's leases and the revocation list's
    version. They block G-20. The other findings are tickets in plan directories of their own.
- **Beside `feat/core-repack`.** That branch carries the `supervisor.*` label rule of the fleet
  record's clause 20 and the operator program's kept-link rule, both left out of the drafts here.
  Whichever of the two reaches `main` second takes the other's rule into its restatement.

## ADR impact

- Binding ADRs in force: 0002 (tests cite decisions), 0004 (the Dev Container, where SoftHSM2 is
  installed), 0006 (this plan), 0007 (the harness: nothing is worked unattended), 0008 and 0009
  (crates, ports and adapters), 0015, 0025, 0026, 0027, 0028, 0030 and 0032, the four of stage 1
  in force, ADR-0026, ADR-0039, ADR-0030 and ADR-0012, and the seven of stage 2, ADR-0028 to
  ADR-0027.
- Implemented here and accepted: the seven ADRs of stage 2 named under *Stages*; the four drafts
  of stage 3 are implemented on `feat/multi-instance-stage-3`. ADR-0031 adds to the fleet
  record's port and to the stores' columns without restating either. On that branch the PKCS#11
  draft adds the audit export under the open outbound rule of ADR-0012 clause 21, decides the
  module's own connection under that clause's rule for a library the Server loads, and the token's
  keys beside the alone rule of ADR-0025 clause 2. Each restatement supersedes the ADR
  it restates when the human accepts it.
- The specification change G-20 is drafted on this branch and lands with stage 2; only the human
  lands it.
- No new ADR is needed beyond these.
