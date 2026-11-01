# ADR-draft-the-fleet-record-of-agents-and-hosts: The fleet record of Agents and Hosts

- **Status:** 🟡 proposed
- **Date:** 2026-10-27
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-server/src/fleet.rs, crates/fleet-server/src/agent_store.rs and its adapters, crates/fleet-server/src/labels.rs, the host register in crates/fleet-server/src/revocation.rs and its adapters, the /api/v1/agents and /api/v1/hosts routes of crates/fleet-server/src/api.rs, stale_after_secs and [database] presence_interval_secs in server.toml, the Hosts and Agents tabs of the bundled UI in crates/fleet-server/static/index.html, and every document that tells an operator what a Host is, when it ends, or how labels and Selectors work

## Context

The Server holds one record per Agent: what it last reported, the hashes that keep the Server
from re-offering what already runs (`sequence_num`, remote-config, connection-settings, and
package statuses — G-3), and what an operator queued or decided for it. Several questions shape
that record.

**What survives a restart.** A fleet held only in memory starts empty after every restart: an Agent
that is not reporting at that moment — a laptop that is off, a host under investigation precisely
because it went quiet — vanishes, and every other Agent is demanded `ReportFullState` at once. The
Server's state survives a restart with only each Agent's status determined dynamically, and the
storage is **replaceable** ([ADR-0029](0029-the-servers-state-and-its-stores.md)). The record's
access pattern is narrow — loaded whole at startup where one Server holds the fleet in memory, read
one Agent or one page at a time where it does not, and written and deleted one Agent at a time — and
its report-derived fields are prost-generated protobuf messages without serde. Write frequency is
the known scale trap: Elastic Fleet persists every check-in and documents the load as a problem.

**What the Server knows about liveness.** `connected` is a fact about a connection, and behind a
Gateway it is the Gateway's. A plain-HTTP Agent has no socket to close. Silence means something
only from an Agent that promised to report periodically, which is what `ReportsHeartbeat` is.

**What "forget" can mean.** A client certificate is issued to a Host, never to an Agent
([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)); nothing the Server holds
can stop one Agent from connecting again. And a re-offered configuration is not free: the
Collector plugin restarts its Managed Process when a configuration arrives, so dropping the gates
of a live Agent bounces its process.

**What a Host is.** The specification names the **Host**: what a client certificate the Server
issued identifies, one Client installation that connects directly, a Gateway included. Its id is
the SAN URI `urn:opamp-fleet:host:<id>` of its client certificate, and every renewal carries it
on ([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)). Every Agent reaching the Server
over that certificate belongs to it. The Agents behind a Gateway belong to the Gateway's Host,
because the Server sees only the Gateway's certificate. A binding guards against one thing:
another host taking over an Agent that a Host still speaks for. Once no certificate naming a host
is valid, nobody can speak as that host again — renewal needs a valid certificate, and a host
that enrols again gets a new host id. The Server cannot always tell from its register alone
whether a host still holds a valid certificate: a certificate an operator provisioned with a host
id is valid without ever having been registered. What the Server does see is every certificate a
host presents. Q-2 asks that what untrusted input makes the Server hold be bounded.

**Where rollout rings live.** A staged rollout wants an attribute an operator invents —
`rollout = "canary"`. On the host, moving between rings is a file edit plus a restart per host,
exactly the per-host wiring the project exists to remove. But reported attributes are
load-bearing: `os.type`, `host.arch`, and `service.name` decide which artifact fits a machine,
and anything that could rewrite them could hand a Windows binary to a Linux host. Reported
attributes are self-asserted, so they must never override what a certificate says either. The
human worked the model out, rejecting each more elaborate variant (see *Alternatives
considered*): the labels set on a host's Client Agent hold for all its Agents, so a Gateway's
downstream hosts are placed by labelling their Client Agent once.

The decision serves G-2 (the Server knows the fleet's state), G-9 (targeting a subset), G-17 (the
Agent is identified), Q-1 and Q-2.

This ADR restates [ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md) in full. Its clauses
1 and 2 take up the paging of
[ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md),
and its clauses 2, 3, 4, 6 and 8 the instances of
[ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it](draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md).

## Decision

We will persist each Agent's record behind a storage port, keep connectedness runtime-only and
derive staleness on read, let an operator forget an Agent that has stopped reporting without
reaching its host, keep a Host for every client certificate's host id until no certificate naming
it is valid, and build each Agent's effective description from its Host's subject values, its
report, its own labels and its Client Agent's labels, in that order of precedence.

1. **One record per Agent, keyed by Instance UID.** It holds what the Agent last reported
   (`sequence_num`, capabilities, description, health, effective configuration, remote-config,
   connection-settings, and package statuses, available components), the transport of the last
   report, `last_seen_ms`, a queued restart (`restart_pending`), the assignments rollout acts
   wrote ([ADR-0026](0026-deployments-and-rollouts.md)), its labels and the Host it last arrived
   over (clauses 16 and 21), and the runtime facts `connected` and the owning connection.
   `GET /api/v1/agents` returns the records as `AgentView`s, a page at a time or whole as
   [ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
   clause 3 says.

2. **Records persist behind a port.** The trait `AgentStore: Send + Sync` is the only thing the
   fleet logic knows about persistence, and its surface is the access pattern:
   - `load()` — every record, once, at startup, for a Server that holds the fleet in memory;
   - `get(uid)` — one record, for an instance that holds only the Agents it carries;
   - `get_many(uids)` — the records of several Agents in one read, for an instance that reads
     again under its fleet lock what another instance changed, so the lock waits for one round
     trip however many records changed;
   - `query(filters, sort, cursor, limit)` — one page of records and the number that match, for
     the Operator plane's list where the fleet is not in memory, by the filters, the order and the
     cursor of
     [ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
     clauses 5 and 6;
   - `put(uid, &PersistedAgent)` — create one record, or replace the part of it a report decides
     (clause 3: what the Agent reported, the transport, `last_seen_ms` and the Host it arrived
     over), leaving the labels, the assignments and `restart_pending` as they are: what setup and
     an import write; the fleet writes through `create_within` and `write_report`;
   - `create_within(uid, &PersistedAgent, max_records)` — create the record of an Agent first
     heard of unless the store holds `max_records` already, counted where the records are stored
     in the step that creates it, so the bound of ADR-0021 clause 12 holds for every instance; a
     record stored already is answered as it is stored, and passes no bound;
   - `write_report(uid, &PersistedAgent)` — write the part of a stored record a report decides, as
     `put` does, and create none; it answers whether there was a record, and on a shared store
     none means the Agent was forgotten through another instance, which the fleet then drops
     rather than create it again (ADR-0024 clause 14);
   - `write_report_unless_held_elsewhere(uid, &PersistedAgent, holder)` — `write_report` at the
     graceful-shutdown flush: on a shared store only of a record whose stored holder is `holder`,
     or is none and whose stored `last_seen_ms` is no newer than the copy's, so an Agent that
     moved to another instance keeps what that instance wrote of it, also once it disconnected
     there;
   - `write_presence(holder, presences)` — one batch of the presence of the Agents `holder`
     carries (clauses 3 and 4), in the order of their `instance_uid`; a disconnect is written only
     of a record whose stored holder is `holder` or none;
   - `remove_if_silent(uid, silent_before_ms)` — forget one record of a shared store when its
     stored presence lets it go, read and removed in one step (clause 11);
   - `set_labels(uid, labels)` and `set_restart_pending(uid, pending)` — the acts that decide them,
     each writing what it names alone; the assignments are written by the rollout operation of
     [ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)
     clause 2. The instance that delivers a restart clears `restart_pending` only while no later
     request set it again;
   - `remove(uid)` — forget one record; removing what is absent is not an error;
   - `rekey(old, new, &PersistedAgent)` — an identity reassignment, one operation so an adapter
     with atomic rename or transactions can make it one step.

   The port speaks the **typed** record (`PersistedAgent`: scalars plus the prost message types),
   never an encoding. On PostgreSQL each call writes the columns it names and no other, so a write
   of the instance that carries an Agent never undoes an act committed through another instance;
   the filesystem adapter, whose one Server is its only writer, may write the whole file for each.
   `AppState` takes a `Box<dyn AgentStore>`; the binary wires the adapter, and
   another backend is a new adapter plus one wiring line. Which reads a Server makes follows its
   store, by the one rule of
   [ADR-draft-the-servers-state-and-its-stores](draft-the-servers-state-and-its-stores.md)
   clause 3: on the filesystem store `load()` and the memory it fills, on PostgreSQL `get` and
   `query`.

3. **What a report or an operator decided persists; so does where an Agent is connected.**
   Persisted: `sequence_num`, `capabilities`, `description`, `health`, `effective_config`,
   `remote_config_status`, `connection_settings_status`, `package_statuses`,
   `available_components`, `transport` (informational), `last_seen_ms`, `restart_pending`, the
   assignments, the labels, and the Host the Agent last arrived over. On PostgreSQL the Agent's
   presence is persisted beside them: `connected` and the id of the instance that holds its
   connection. Not persisted: the owning connection, and on the filesystem `connected`.

4. **The write discipline lives above the port, so no backend can get it wrong.** The fleet keeps a
   digest of each record's durable content as last written and calls `write_report` only when it
   changed. `last_seen_ms` and `sequence_num` are outside the digest. On the filesystem they ride
   along with whatever write happens, so a heartbeat, which changes nothing else, reaches no
   adapter, and after a crash a record's last-seen dates from its last durable change. On
   PostgreSQL each instance writes the presence of the Agents it carries — `connected`,
   `last_seen_ms`, the holding instance — in one batch every `[database] presence_interval_secs`
   (default 30, from 10 to 60), so
   a heartbeat reaches the database at most once per interval and Agent. A change of the holding
   instance and a disconnect are written in the next batch, except as
   [ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it](draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md)
   clause 8 says. On graceful shutdown every record and every presence is flushed, so an ordinary
   restart restores current values; on PostgreSQL the flush writes only the records of the Agents
   this instance heard report, and of those only the ones whose stored holder is this instance, or
   is none and whose stored `last_seen_ms` is no newer than this instance's, and the presence in a
   last batch. A failed write is logged, never fatal; on PostgreSQL a
   presence batch that fails is retried with the next one.

5. **Stored records are secret-bearing, whatever the backend.** A reported effective
   configuration carries whatever the Managed Process runs, credentials included, and package
   statuses may carry download headers. The filesystem adapter answers with an owner-only
   directory; any other adapter must answer with its own access control.

6. **`connected` says a connection carrying the Agent is open — nothing more.** Every report sets
   it; only the owning WebSocket closing, or the Agent's `agent_disconnect`, clears it. Behind a
   Gateway it is the Gateway's connection; an Agent that stops polling plain HTTP without saying
   goodbye stays connected. On PostgreSQL, an Agent whose presence names an instance without a live
   lease reads as disconnected, whatever its row says. A restored record is disconnected with no
   owning connection until live evidence says otherwise.

7. **`stale` is derived on read, and only an Agent that promised a heartbeat can be stale.** An
   Agent is stale when `now - last_seen_ms` exceeds its budget **and** it declares
   `ReportsHeartbeat`; an Agent that never promised to report periodically is never stale.
   Nothing is stored and no timer runs. `AgentView` carries `stale` beside `connected`, never in
   place of it: `connected: true, stale: true` is precisely the gatewayed Agent that stopped
   talking.

8. **The budget is three heartbeat intervals, and on PostgreSQL the presence interval besides.**
   When the Server offers a heartbeat interval (`[connection_offer] heartbeat_interval_secs`,
   [ADR-0013](0013-connection-settings-that-never-lead-to-plaintext.md)), the budget is three times
   it; otherwise `stale_after_secs` in `server.toml`, default **90** — three times the Baseline's
   default heartbeat of 30 seconds — and never zero. On PostgreSQL the budget is that plus
   `presence_interval_secs`, since `last_seen_ms` in the database may be one interval old. One
   missed heartbeat is a lost packet; a view that flickers on every hiccup is one nobody trusts.

9. **Staleness changes nothing about how the Server treats an Agent.** A stale Agent keeps its
   configuration, its package state, and its identity, is offered what it always would be, and
   its next report clears the flag. It is an observation, not a state transition.

10. **Forgetting drops what the Server knows and reaches no host.**
    `DELETE /api/v1/agents/{instance_uid}` removes the record from the fleet and from the store,
    and releases the `instance_uid` from the Host it was bound to. No process is stopped, no
    configuration withdrawn, no package removed, no credential revoked. The word in the API
    description, the UI, and the manual is **forget**, never *delete*, *remove*, or *unenrol*. An
    Agent still running comes back as a stranger: the Server demands `ReportFullState`, as for
    any unknown Agent, and the row returns complete — re-offered its configuration, connection
    settings, and packages. Its first report binds it again, to the host whose certificate
    carried that report.

11. **Only an Agent that is not reporting may be forgotten.** The act succeeds when the record is
    **not connected**, or has been **silent for longer than the staleness budget**, and is
    refused otherwise, because forgetting a live managed Agent restarts its Managed Process — an
    operator who wants that has `POST /api/v1/agents/{instance_uid}/restart`. Silence here is the
    plain fact, **without** the `ReportsHeartbeat` gate of clause 7: with it, a polling Agent
    that declares no heartbeat could never be forgotten at all.

12. **The outcomes are `204`, `409`, `404`, `400`.** `204` when the record is gone; `409` when
    the Agent is still reporting, with the reason; `404` when no such Agent is known, as the
    restart route answers; `400` when the Instance UID does not parse.

13. **Nothing expires on its own.** No retention timer, no inactivity sweep, no ephemeral
    marking: forgetting is one explicit act on one Agent. An automatic policy needs a duration, a
    scope, and an answer for the Agent that is merely on holiday — a decision of its own.

14. **The stored record follows the identity.** Forgetting removes it (clause 10); when the
    Server reassigns an identity (`RequestInstanceUid`, or the duplicate-UID rekey), the record
    moves through `rekey`. Forgetting that left a stored record behind would be a remembering
    under another name.

15. **A Host is created when its enrolment is approved, and ends when no certificate naming it is
    valid.**
    - The approval that mints a host id writes the Host. Its record holds:
      - the subject values of the client certificate it was issued: country, state, locality,
        organization, organizational unit, organization identifier. Which of them a certificate
        carries is [ADR-0022](0022-bootstrap-certificates-and-enrolment.md)'s decision on
        carrying the bootstrap certificate's subject over; without it, none.
      - its Gateway mark;
      - the `instance_uid`s bound to it;
      - the time of the approval;
      - its **valid until**: the latest `not_after` of a certificate naming it that the Server
        issued or that it presented in a handshake. Every issuance and every admission moves it
        forward, never back.
    - A certificate an operator provisioned with a host id makes a Host at its first report.
    - A Host the approval could not write is written with the next write of the host register,
      and at the latest at its first report, from the values its certificate carries. The
      approval is not refused for it: the certificate is signed and registered by then, and the
      values are in the certificate.
    - Once its valid until has passed, a Host that is not marked as a Gateway is removed, and
      every `instance_uid` bound to it is released. A Gateway's Host stays until its mark is taken
      away, because its certificate may be one the operator provisioned and never presented. A
      Host written before it had a valid until takes the latest `not_after` the register holds
      for it, and stays until its next admission when the register holds none.

16. **What a certificate speaks for, and which Host an Agent belongs to.**
    - The host a member certificate names (`urn:opamp-fleet:host:<id>`) speaks for the
      `instance_uid`s bound to it in the host register.
    - A Host marked as a Gateway speaks for any Agent: the host register binds no `instance_uid`
      to it and lists none of the Agents a Gateway carries. A Gateway's certificate therefore may
      fetch what is offered to any Agent.
    - A certificate that names no host speaks for no Agent and fetches nothing.
    - A Client whose Server signs CSRs holds a host certificate after its first connection
      ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)). An operator who
      provisions certificates by hand names the host with that SAN URI.
    - In addition, each Agent record names the Host it last arrived over: its own Host on a
      direct connection, the Gateway's Host behind a Gateway. This is for the fleet view and for
      clause 19, and it grants nothing. A Server without `[client_ca]` has no Hosts, and its Agent
      records name none.
    - A Server started without `[client_ca]` logs one notice at startup: uploaded artifacts reach
      only hosts whose certificate names a host (`urn:opamp-fleet:host:<id>`).
    - A request that presents no certificate at all exists only behind `Admission::open`, which
      serves tests and loopback development and which no Server configuration serves
      ([ADR-0021](0021-admission-by-client-certificate-and-revocation.md)). Such a request is not
      tested for an offer, just as it is not tested at admission.

17. **`PUT /api/v1/agents/{instance_uid}/labels` sets an Agent's labels, whole.** Body
    `{"labels": {…}}`, string to string, replacing what was there; an empty map clears them. An
    empty key or value is refused (`400`), an unknown Agent `404`, and the answer is the Agent's
    view. Whole-map replacement keeps the write idempotent and lets an operator see the resulting
    state in the request sent. `AgentView` carries `labels`.

18. **Selectors match an effective description, the same for every Selector.** For each key the
    description takes the first source that has it:
    1. `fleet.host.country`, `fleet.host.state`, `fleet.host.locality`,
       `fleet.host.organization`, `fleet.host.site` (the organizational unit) and
       `fleet.host.organization_id`, from the subject values of the Host the Agent arrived over
       directly. An Agent behind a Gateway has no `fleet.host.*`: its own certificate is not
       seen.
    2. What the Agent reported: identifying attributes, then non-identifying ones.
    3. The Agent's own labels.
    4. The labels of its Client Agent (clause 19).

19. **An Agent inherits the labels of its Client Agent.**
    - The Client Agent is the Agent a Client presents for itself, recognised by the
      `supervisor.kind.*` attributes it alone reports
      ([ADR-0017](0017-supervisor-mode-and-its-kinds.md)).
    - Its labels hold for the other Agents of the same Client:
      - on a direct connection, the Agents bound to the same Host;
      - behind a Gateway, the Agents arriving over the same Gateway Host that report the same
        `host.id`. An Agent that reports no `host.id` is grouped by its `host.name`, apart from
        the groups keyed by `host.id`.
    - Where that grouping finds more than one Client Agent, nothing is inherited, and the fleet
      view marks the Agents as ambiguous.
    - The grouping behind a Gateway rests on what Agents report. It places Agents for convenience
      inside the trust the Gateway already has, and it never decides admission.

20. **`fleet.*` and `supervisor.*` are reserved, and a label never overrides a reported attribute
    or a Host value.**
    - A label whose key starts with `fleet.` is refused (`400`), naming the key.
    - A label whose key starts with `supervisor.` is refused (`400`), naming the key, the same way.
      That space is a Supervisor's own statement: the kinds its Client carries and the version of
      the Package it installed ([ADR-0016](0016-what-an-agent-reports-about-itself.md) clauses 5
      and 10). A label there would let a Selector match an Agent as if it stated one.
    - A label the record already holds in either space is ignored: it enters no effective
      description (clause 18), so no Selector and no package match reads it, no Agent inherits it
      (clause 19), and the fleet row lists it in `ignored_labels`. The next label set put without
      it removes it.
    - A label whose key the Agent reports is refused (`409`), naming it. If an Agent starts
      reporting a key it was labelled with before, the reported value wins, and the label is
      listed in `shadowed_labels` on the fleet row rather than dropped in silence.
    - A reported attribute whose key starts with `fleet.` is left out of the effective
      description and listed in `ignored_attributes` on the fleet row. An Agent therefore cannot
      claim another Host's subject values.

21. **Labels are part of the Agent's record and go with it.**
    - Labels are stored in the Agent's persisted record. There is no separate label store.
    - They follow a re-key as the record does (clause 14), and forgetting an Agent removes them.
    - They never travel to the Agent: no capability, no message and nothing the Client reports
      carries them.

22. **A label change moves what is proposed, never what is offered.** Setting labels changes
    which Configurations and Deployments are candidates for the Agent; nothing reaches it until a
    rollout act ([ADR-0026](0026-deployments-and-rollouts.md)).

23. **The bundled UI puts each remedy beside its diagnosis.** A stale Agent shows a *Stale* pill
    beside its connection pill. The forget action is offered on every row, not pre-filtered by
    the row's fields: the Server holds the one rule of clause 11 and its `409` reason is shown.
    Labels appear as their own chips, a shadowed one marked, and are searched beside the reported
    attributes.

24. **The Hosts tab.** The bundled UI has a tab that lists the Hosts:
    - Each row shows the Host's display name, its subject values, whether it is a Gateway, its
      valid certificates and their ends, and the number of its Agents.
    - The display name is the `host.name` its Client Agent reports when the Host has exactly one
      Client Agent, else the host id. A Gateway always shows its host id: its own Client Agent
      arrives over the same Host as the Client Agents behind it. The name is shown only and
      decides nothing.
    - The current Host's detail shows its Agents, grouped by their Client Agent with that
      Client's Supervisor Agents beneath it. For a Gateway this covers the Agents behind it,
      grouped as clause 19 groups them. The grouping is presentation only.
    - The Gateway mark and a revocation of the Host's certificates are acts on the row. Labels
      stay on the Agents tab, where the Client Agent's labels are marked as inherited by the
      others.
    - `GET /api/v1/hosts` lists each Host with its subject values and its Agents beside what
      else it lists.

**Out of scope:** a configuration surface for choosing a storage backend and where Agent records
and Hosts are stored ([ADR-0029](0029-the-servers-state-and-its-stores.md)); a retention policy
for Agents nobody claims; acting on staleness (alerts, webhooks, filters); bulk labelling; the
exact topology behind a Gateway; a Host for a machine the Server sees no certificate of; labels
or editable fields on a Host itself; which subject values a certificate carries
([ADR-0022](0022-bootstrap-certificates-and-enrolment.md)); writing the host register
incrementally instead of whole; a global bound on the number of Hosts, which approvals and
provisioned certificates already limit.

## Alternatives considered

- **Keep the fleet in memory.** Loses the inventory exactly when a host cannot repeat it, and
  turns every restart into a fleet-wide `ReportFullState` stampede.
- **A concrete filesystem store, no port.** The simplest thing that persists, but the storage is
  to be replaceable, and retrofitting a port rewrites the call sites it protects; a few operations
  keep the abstraction cheap.
- **A generic key-value port (`uid → bytes`).** Decides the encoding above the port, so a database
  adapter degenerates into a blob table.
- **Persist every report, heartbeats included.** Write amplification with near-zero information,
  documented as a scale problem by Elastic Fleet.
- **One periodic snapshot file.** Rewrites the world on every change or waits for a timer, and a
  crash loses the tail.
- **Serde derives on the generated protobuf types.** Welds the stored format to prost's field
  names, and JSON of generated types drops unknown fields anyway.
- **Mark a stale Agent disconnected.** `connected` is a fact about a connection, which behind a
  Gateway is up; overwriting it makes the field ambiguous.
- **The Gateway synthesises `agent_disconnect` for a vanished peer.** Asserts a goodbye that never
  happened.
- **Staleness for every Agent, heartbeat or not**, or **inferring the period from the Agent's
  rhythm.** Flags Agents that promised nothing, or guesses — and the first slow fleet-wide restart
  makes every Agent look late.
- **A background sweeper flipping a stored `stale` flag.** A task and a stored state that can
  disagree with the timestamp, for an event nobody consumes.
- **Hide the row instead of dropping the record** (Elastic's *inactive*). Frees nothing; `stale`
  is already the visible signal such a model exists to produce.
- **Forget unconditionally** (Bindplane). Turns a tidy-up into an unannounced restart on a healthy
  host.
- **Gate forgetting on the `stale` flag.** `stale` is false by construction without a heartbeat
  and `connected` never clears for a silent poller, so such an Agent would be unforgettable.
- **Keep the re-offer gates across a forget in a side table.** A record of the Agent under another
  name, which nothing ever removes.
- **Revoke on forget.** There is no per-Agent credential, and an Instance UID is not an
  authenticator — a deny-list of them would be theatre.
- **`POST …/forget` instead of `DELETE`.** The record is the resource the collection returns; the
  naming worry is answered in the description and the UI label.
- **Keep a forgotten Agent's binding to its Host.** A Host with a bound Agent would never be
  removed, even long after its last certificate expired, and the register would grow with every
  approval for the life of the installation, up to 256 bindings per host. Forgetting releases the
  binding instead.
- **Judge a Host's validity by the register alone.** A Host with a provisioned certificate that
  the register never held would lose its bindings while its certificate is still valid, and
  another host could take over its Agents.
- **Release bindings only on forget.** Bindings of hosts that are gone for good would stay until
  an operator forgets every one of their Agents. Kept as the second rule, not the only one.
- **Expire bindings after a time without a report.** A host that is off for a while would lose its
  Agents to whoever reports them first while its certificate is still valid.
- **Keep every Host and binding.** The register grows for the life of the installation.
- **Hosts from the Agents' `host.*` attributes.** These work behind a Gateway and without
  certificates. But they are self-asserted, cloned machines share `host.id`, and they cannot carry
  values a Selector may trust.
- **A host proof: the Client adds its certificate and a signature over its `instance_uid` to each
  Agent's description**, after the renewal proof. It would identify hosts behind a Gateway with a
  Server-verifiable proof. The human rejected it as more than needed.
- **The Gateway adds the downstream host's id to what it forwards**, or **Gateway-set
  attributes.** This gives an exact, verified topology. But the Gateway would process instead of
  forward, which costs efficiency and the Gateway's simplicity
  ([ADR-0015](0015-client-modes-and-the-gateway.md)). Rejected by the human.
- **Virtual hosts behind a Gateway, keyed by Gateway and `host.id`.** They would give an
  approximate host list. Label inheritance covers the need without a second kind of Host, so the
  human dropped them.
- **`fleet.gateway.*`: the Gateway's subject values on the Agents behind it.** These would be
  verified, but they describe the path, not the host. Rejected by the human.
- **Reported attributes win over Host values, as they win over labels.** This would let any Agent
  report `fleet.host.organization` and be selected by another customer's Deployment.
- **Labels that win over reported attributes** (Bindplane's bootstrapped labels). Here reported
  attributes choose the binary, so a mislabelling would become a mis-installation.
- **Labels in a store of their own, keyed by Instance UID, that outlive a forget.** A forgotten
  host comes back in its ring. But a second store can leave the labels of an Agent re-keyed on
  its own request under the old identity, and labels of a forgotten Agent cannot be cleared
  through the API, because the label route needs a known Agent. Inheritance from the Client Agent
  covers most placements.
- **A separate `labels_dir` setting.** A third path to know and back up, for data that belongs
  with the Agent's record.
- **Flat tags** (Elastic). A second matching rule beside the key/value Selector that exists.
- **Add/remove operations** (`tagsToAdd` / `tagsToRemove`). Better for bulk work, which deserves
  its own decision.
- **Push labels to the Client to report.** Makes an Agent report as observed what it was told, and
  puts a round trip between labelling a host and targeting it.
- **Key labels by `service.instance.name`.** Not unique across hosts, so labelling one host would
  label its namesakes.
- **Keep an Agent's labels in a store of their own, mirrored into its record.** Two places hold one
  fact, while the Server already keeps the labels in the record (clause 21). The record carries the
  labels and the Host the Agent last arrived over, and no separate label store exists (clauses 1, 3
  and 21).

## Sources / Prior art

- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — `ReportsHeartbeat` and its 30-second default; `ReportFullState` as the recovery for lost
  server state; silent on server-side record lifetime, and its "revoke access" is per-connection
  credentials, an operation this project cannot perform per Agent.
- [Elastic Fleet agent
  monitoring](https://www.elastic.co/docs/reference/fleet/monitor-elastic-agent)
  — a persisted inventory with *offline* derived from `last_checkin`; and
  [fleet-server#749](https://github.com/elastic/fleet-server/issues/749), the write load of
  persisting every check-in.
- [Elastic inactivity timeout](https://www.elastic.co/docs/reference/fleet/set-inactivity-timeout)
  and [unenrolment](https://www.elastic.co/docs/reference/fleet/unenroll-elastic-agent) — the
  hide-then-revoke model, whose second stage rests on per-Agent credentials.
- [Bindplane ephemeral
  collectors](https://docs.bindplane.com/how-to-guides/collector-management/golden-images-and-ephemeral-collectors)
  — a forgotten collector that returns "will just be treated as a new agent".
- [Bindplane Bolt
  Store](https://bindplane.com/docs/advanced-setup/backup-and-disaster-recovery/bolt-store)
  and [Postgres Store](https://docs.bindplane.com/how-to-guides/postgres/postgres-store) — one
  store interface, interchangeable backends.
- [Bindplane agents API](https://docs.bindplane.com/cli-and-api/api/agents) and
  [progressive rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/progressive-rollouts)
  — key/value labels, the opposite collision rule, and stage matching by "every label specified".
- [Elastic agent tags](https://www.elastic.co/docs/reference/fleet/filter-agent-list-by-tags) and
  the [bulk tag
  update](https://www.elastic.co/docs/api/doc/kibana/operation/operation-post-fleet-agents-bulk-update-agent-tags)
  — flat tags, added and removed in bulk.
- [Hexagonal architecture](https://alistair.cockburn.us/hexagonal-architecture/) (Cockburn) — the
  ports-and-adapters pattern behind clause 2
  ([ADR-0009](0009-ports-and-adapters.md)).
- [opamp-go](https://github.com/open-telemetry/opamp-go) example server — keeps its agent map in
  memory and offers no persistence design to follow.
- [ADR-0021](0021-admission-by-client-certificate-and-revocation.md) on the host a certificate
  names, renewal, and that a Gateway hides its downstream certificates;
  [ADR-0017](0017-supervisor-mode-and-its-kinds.md) on `supervisor.kind.*`;
  [ADR-0016](0016-what-an-agent-reports-about-itself.md): `host.name`, `host.id`, `host.ip` and
  `host.mac` are reported attributes.
- The code: `check_report` and `prune` in `crates/fleet-server/src/revocation.rs`, the host minted
  at approval and `forget_agent` in `crates/fleet-server/src/fleet.rs`. Read in the source.
- The working note on the Server's data model, in which the human chose each point of the Host
  model.

## Consequences

- Positive: the fleet view survives a restart, including the Agents not reporting, shown honestly
  as disconnected; restored gates mean a reconnecting Agent's compressed report is accepted and
  nothing that already runs is re-offered; a queued restart survives.
- Positive: storage is a deployment choice behind a trait of six operations, and tests get an
  in-memory fake for free.
- Positive: the fleet view stops asserting what it does not know — gatewayed, polling, and wedged
  Agents become visible by one rule — and a decommissioned host's row can be removed.
- Positive: a rollout ring is a Server-side decision, one API call instead of an edit and a
  restart on the host, at no protocol cost. Labelling a Client Agent once places all its
  Supervisors.
- Positive: an operator sees which Agents run on which Host and behind which Gateway, and acts on
  certificates and Gateway marks where the Host is shown. A Selector can target a customer or
  site by values the Server issued, which no Agent can forge.
- Positive: the host register shrinks with the fleet. A forgotten Agent can be reported by
  whichever host runs it now.
- Negative / trade-offs: a forgotten Agent that returns costs one re-apply, and for a managed
  Agent one restart of its Managed Process. A disconnected Agent is forgettable at once, so a
  flapping one can be forgotten between two connections.
- Negative / trade-offs: forgetting is not unenrolling; a still-configured Client reappears, and
  only the host can stop it. Forgetting an Agent that still runs lets the next host that reports
  its `instance_uid` bind it — the operator's act decides that, as it decides the forgetting.
- Negative / trade-offs: forgetting an Agent drops its labels. A host that comes back starts
  unlabelled unless its Client Agent is labelled.
- Negative / trade-offs: the store grows with every Agent ever seen and nothing prunes it; an
  Agent that declares no heartbeat is never stale; after a crash an idle-but-alive Agent can read
  as silent, and so forgettable, until its next report.
- Negative / trade-offs: targeting depends on Server-side state no Agent reports; an operator who
  wants to correct a badly reported attribute must do it on the host.
- Negative / trade-offs: Agents behind a Gateway carry no `fleet.host.*`. A Deployment reaches
  them by labels on their Client Agents. The grouping behind a Gateway is approximate: two
  Clients on one machine behind one Gateway are shown as ambiguous and inherit nothing.
- Follow-ups: a retention policy for Agents; bulk forgetting and bulk labelling; staged rollouts
  as a first-class object on top of labels; observability of the store's size; writing the host
  register incrementally.

## Enforcement

- [`fleet.rs`](../../crates/fleet-server/src/fleet.rs) unit tests:
  `an_agent_that_stopped_reporting_is_stale_while_its_connection_is_up`,
  `an_agent_inside_its_budget_is_not_stale`,
  `an_agent_that_promised_no_heartbeat_never_goes_stale`,
  `an_offered_heartbeat_interval_sets_the_budget`, `a_disconnected_agent_is_forgotten`,
  `an_agent_that_is_still_reporting_is_refused`, `a_connected_agent_that_went_quiet_is_forgotten`,
  `a_silent_agent_is_forgotten_although_it_can_never_be_stale`,
  `forgetting_an_agent_that_was_never_known_says_so`,
  `the_fleet_is_restored_disconnected_after_a_restart`,
  `a_restored_agent_is_not_demanded_a_full_report`, `a_heartbeat_writes_nothing`,
  `forgetting_an_agent_removes_its_stored_record`, `a_rekeyed_agent_moves_its_stored_record`,
  `a_queued_restart_survives_a_restart`, `forgetting_an_agent_releases_its_binding`,
  `a_direct_agent_matches_its_hosts_values_and_none_it_reports_itself`,
  `a_supervisor_agent_inherits_its_client_agents_labels_below_its_own`,
  `a_stored_label_in_the_supervisors_space_matches_nothing`,
  `behind_a_gateway_agents_sharing_a_host_id_inherit_unless_ambiguous`,
  `a_client_agent_rekeyed_without_a_description_keeps_its_group_inheriting`,
  `forgetting_an_agent_removes_its_labels_and_a_rekey_keeps_them`.
- [`agent_store.rs`](../../crates/fleet-server/src/agent_store.rs):
  `a_heartbeat_does_not_change_the_durable_digest`;
  [`fs/agents.rs`](../../crates/fleet-server/src/fs/agents.rs): `a_record_survives_the_round_trip`,
  `remove_deletes_and_tolerates_absence`, `rekey_moves_the_record`,
  `a_corrupt_record_fails_the_load_by_name`,
  `labels_and_host_live_in_the_record_and_an_older_envelope_is_refused`.
- [`labels.rs`](../../crates/fleet-server/src/labels.rs) unit tests:
  `a_label_is_matched_like_a_reported_attribute`,
  `a_label_never_overrides_what_the_agent_reports`,
  `an_agent_that_reported_nothing_can_still_be_labelled`,
  `empty_keys_and_values_and_reserved_keys_are_refused`,
  `a_stored_label_in_the_supervisors_space_is_ignored`,
  `the_effective_description_takes_each_key_from_the_first_source_that_has_it`.
- [`revocation.rs`](../../crates/fleet-server/src/revocation.rs) unit tests:
  `an_approval_writes_the_host_before_its_first_report`,
  `a_provisioned_certificate_makes_its_host_at_its_first_report`,
  `an_approval_whose_host_cannot_be_written_is_not_refused`,
  `a_host_whose_last_certificate_has_expired_ends_with_its_bindings`,
  `a_host_with_a_valid_presented_certificate_keeps_its_bindings`,
  `a_host_written_before_its_valid_until_takes_the_registers`,
  `a_hosts_valid_until_only_moves_forward`, `a_released_instance_uid_can_be_bound_by_another_host`;
  [`fs/revocation.rs`](../../crates/fleet-server/src/fs/revocation.rs):
  `a_host_file_without_a_valid_until_still_loads`;
  [`ca.rs`](../../crates/fleet-server/src/ca.rs):
  `a_certificates_subject_values_are_read_by_attribute`.
- [`rest_api.rs`](../../crates/fleet-server/tests/rest_api.rs):
  `forgetting_an_agent_that_is_still_reporting_is_refused`,
  `a_silent_agent_is_forgotten_and_returns_as_a_stranger`,
  `forgetting_what_is_not_there_is_reported`, `a_label_moves_an_agent_into_a_rollout_ring`,
  `a_label_may_not_restate_what_the_agent_reports`, `a_label_in_the_fleet_space_is_refused`,
  `a_label_in_the_supervisors_space_is_refused`,
  `forgetting_an_agent_removes_its_labels`, `the_ui_lists_hosts_and_marks_inherited_labels`;
  [`mutual_tls.rs`](../../crates/fleet-server/tests/mutual_tls.rs):
  `an_approval_writes_the_host_before_its_first_report`,
  `the_hosts_list_carries_subject_values_and_the_agents_behind_a_gateway`;
  [`packages.rs`](../../crates/fleet-server/tests/packages.rs)
  `a_label_aims_a_set_at_part_of_the_fleet`.
- Clause 1 as it stands here is verified by the tests
  [ADR-draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server](draft-the-lists-that-grow-with-the-fleet-are-paged-by-cursor-and-filtered-on-the-server.md)
  names, `query` of clause 2 by the same tests run against every store adapter, `get` of clause 2
  and clauses 3, 4, 6 and 8 by those of
  [ADR-draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it](draft-several-server-instances-share-one-postgresql-database-and-coordinate-through-it.md).
- Tests that will carry `Verifies: ADR-draft-the-fleet-record-of-agents-and-hosts`, for clause 2,
  run against every store adapter: `put_keeps_the_labels_the_assignments_and_a_pending_restart`,
  `set_labels_and_set_restart_pending_write_what_they_name_alone`, and
  `a_restart_requested_while_another_is_delivered_stays_pending`.
