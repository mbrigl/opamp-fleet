# SRV-instance-opens-chosen-planes: A Server instance opens the planes its configuration names

- **Status:** 🟡 proposed
- **Date:** 2026-11-20
- **Deciders:** Markus Brigl
- **Applies to:** the `planes` key of `server.toml` and its parsing and startup checks in `crates/fleet-server/src/config.rs`, `config/server.toml`, the binding, serving, closing and draining of the planes in `crates/fleet-server/src/instance.rs`, `crates/fleet-server/src/listen.rs` and `main.rs`, the use-case modules the composition root builds, the lease record in `crates/fleet-server/src/postgres/lease.rs` and its schema, the startup log, and every document that tells an operator how to run several instances or how to probe one

## Context

On PostgreSQL several Server instances serve one fleet, and every instance serves both planes
([SRV-instances-share-one-postgresql](SRV-instances-share-one-postgresql.md)): the Agent plane on
`4320` and the Operator plane on `4321`
([SEC-tls13-two-planes-bounded-input §7](SEC-tls13-two-planes-bounded-input.md)). The two planes
share an instance's CPU, memory and database pool (`[database] pool_size`); only their connection
caps are apart
([SEC-tls13-two-planes-bounded-input §19](SEC-tls13-two-planes-bounded-input.md)).

Goal G-20 asks that a fleet of about 100 000 Agents is served on when an instance is lost: its
Agents connect to the others within 90 seconds, and a revocation committed on any instance ends
its sessions on every instance within 10 seconds. That is the moment the two planes compete most:
the Agents of the lost instance reconnect to the rest, and an operator may have to act at once, to
revoke a certificate or to hold a rollout. Every instance that takes the reconnecting Agents also
serves the operator, through the same pool. A heavy Operator-plane read slows the sessions the
same way.

The planes already meet only through the database. An operator's act on one instance reaches the
sessions on another by a notification
([SRV-instances-share-one-postgresql §9](SRV-instances-share-one-postgresql.md)) or by the re-read
after a missed one
([SRV-instances-share-one-postgresql §10](SRV-instances-share-one-postgresql.md)), and the
Operator plane reads the Agents from the database
([SRV-instances-share-one-postgresql §13](SRV-instances-share-one-postgresql.md)). An instance
that opens one plane alone therefore needs no new channel between instances. What it needs is code
in which each plane reaches state through its own module, so that an instance can leave the other
module unbuilt; [SRV-two-use-case-layers](SRV-two-use-case-layers.md), proposed, decides that
split. This decision follows it and is implemented only after it.

A certificate is signed where its request arrives: a host certificate on the Agent plane's
connection the CSR came over, a bootstrap certificate through the REST API. Several instances
already keep both CA keys on a token, not in a file
([SRV-instances-share-one-postgresql §2](SRV-instances-share-one-postgresql.md)), so an instance
of either plane reaches the key it signs with as it does today.

Two clauses in force mean "both planes" and stop being true for such an instance: the lease's
listeners, which open, close and drain together
([SRV-instances-share-one-postgresql §3](SRV-instances-share-one-postgresql.md)), and the
documented probe, a TCP connection to `4320`
([OPS-releases-attestations-server-image §17](OPS-releases-attestations-server-image.md)). This
draft states the decision alone. Before it is accepted, it is restated to carry every clause of
those two ADRs, changed as the decision below asks, and names them under `Supersedes`.

## Decision

We will let each Server instance open the Agent plane, the Operator plane or both, as the `planes`
key of its configuration names.

1. **One key, both by default.** `planes` in `server.toml` is an array of `"agent"` and
   `"operator"`, by default both. An empty array, an unknown name or a name given twice fails
   startup, naming the key. The environment sets it as every key, such as
   `FLEET_SERVER__PLANES='["agent"]'`
   ([SRV-configuration-toml-and-environment §3 and §4](SRV-configuration-toml-and-environment.md)).

2. **On the filesystem store an instance opens both.** With `[database] store = "fs"` there is
   one Server, and a `planes` that does not name both fails startup, naming the key.

3. **An instance opens, closes and drains the listeners of its planes alone.** It binds only the
   addresses of the planes it names. Whatever SRV-instances-share-one-postgresql says of "both
   listeners" or "either plane" — the lease taken before they open, the `503` while the lease is
   lapsed, the close when it cannot renew, the drain at shutdown — holds for the listeners it
   opens.

4. **An instance builds the use cases of its planes alone.** Without the Agent plane it builds no
   `AgentCases`, carries no Agent and writes no presence; without the Operator plane it builds no
   `OperatorCases`. Whatever every instance does stays with every instance, whichever planes it
   opens: the lease, its audit chain, the takeover of a dead process's lease and chain, listening
   for notifications and reading its caches again, and removing the staging objects of a lapsed
   lease.

5. **The other plane's keys are ignored and named.** Startup requires of a plane the instance
   opens what it requires today, and of a plane it does not open nothing. A key that only a plane
   the instance does not open reads is ignored, and the startup log names each such key once. One
   `server.toml` therefore serves every instance of a fleet, with `planes` set per instance.

6. **The probe is the port of a plane the instance opens.** The documented probe is a TCP
   connection to the Agent plane's port when the instance opens it, and to the Operator plane's
   port otherwise.

7. **An instance warns while a plane goes unserved.** The lease records the planes its instance
   opens. When an instance takes its lease, and on each renewal, it reads the planes of every live
   lease; for each plane that none serves it logs a warning naming the plane, once until a live
   lease serves it again. It refuses nothing: how many instances serve each plane is the
   operator's.

**Out of scope:** a database pool of each plane's own on an instance that opens both; a health
endpoint; separate programs or images per plane; renaming the planes; the balancer, the
orchestrator and how many instances of each plane a fleet runs.

## Latitude

None.

## Alternatives considered

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
- **The other plane's keys refused at startup.** Each role would need a configuration of its own,
  where one file and one environment per fleet are simpler to keep alike. Rejected; clause 5 names
  them in the log instead.
- **A database pool, or a runtime, of each plane's own inside one instance.** It keeps a
  reconnecting fleet from starving the operator of connections on every instance, without roles.
  It does not isolate memory, CPU or the loss of a host, and adds a second pool size to set.
  Rejected for now; it stays open (see *Out of scope*).
- **A health endpoint.** A probe could ask whether the lease is held rather than whether a port is
  open. The closed listener of a lapsed lease already answers that
  ([SRV-instances-share-one-postgresql §3](SRV-instances-share-one-postgresql.md)), and an
  endpoint is a route more on a plane. Rejected;
  [OPS-releases-attestations-server-image §17](OPS-releases-attestations-server-image.md) leaves
  it to a later decision.
- **No decision.** Every instance keeps serving both planes; the operator sizes the instances for
  the reconnecting fleet and the console together, and a heavy read or a reconnect storm reaches
  both. Rejected.

## Sources / Prior art

- The specification's goal G-20 and its success criteria.
- The code as it stands: `instance::serve` and `Planes` in `crates/fleet-server/src/instance.rs`,
  which bind, gate and drain both planes together; `pool_size` in
  `crates/fleet-server/src/config.rs`; the signing of a CSR in
  `crates/fleet-server/src/fleet/approval.rs` and of a bootstrap certificate in
  `crates/fleet-server/src/bootstrap.rs`.
- Kubernetes, [Configure liveness, readiness and startup
  probes](https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/):
  a TCP probe per container port, which clause 6 sets per role.
- The working note "server split", recorded outside the ADRs, on which SRV-two-use-case-layers
  builds.

## Consequences

- Positive: the Agent plane and the Operator plane can each run on instances of their own, scaled
  and kept available apart. A reconnecting fleet does not slow the instances an operator acts
  through, and a heavy Operator-plane read does not slow the sessions.
- Positive: the Operator plane can run in an internal network only, and the Agent plane's
  instances need no Operator-plane certificate or credential.
- Positive: nothing changes for an installation that sets no `planes`.
- Negative / trade-offs: a fleet with roles needs at least two instances of each plane to keep
  both available, so four instances where two served before.
- Negative / trade-offs: one more key, and a probe that depends on it; a lease record with a new
  column, which a migration adds.
- Follow-ups: the restatement of SRV-instances-share-one-postgresql and
  OPS-releases-attestations-server-image that this draft needs before acceptance; a pool of each
  plane's own, should combined instances need it; a health endpoint.

## Enforcement

Tests that will carry `Verifies: SRV-instance-opens-chosen-planes`:

- The configuration's tests: an empty `planes`, an unknown name and a duplicate fail startup; on
  the filesystem store a `planes` without both fails; without the key both are opened; a key that
  only the closed plane reads is named in the startup log.
- An instance test on PostgreSQL: with `["agent"]` nothing listens on the Operator plane's
  address, with `["operator"]` nothing on the Agent plane's; a lapsed lease closes and a drain ends
  only the listeners opened.
- An instance test on PostgreSQL with one instance of each plane: a revocation committed through
  the Operator-plane instance ends the session on the Agent-plane instance within one lease
  renewal interval, as G-20 asks.
- A lease test: an instance logs the warning of clause 7 while no live lease serves a plane, and
  not again until one has served it.
