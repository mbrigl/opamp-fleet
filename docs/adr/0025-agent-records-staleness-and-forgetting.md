# ADR-0025: Agent records persist behind a storage port — a silent Agent goes stale, and one that is not reporting can be forgotten

- **Status:** 🟢 accepted
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

## Context

The Server keeps one record per Agent, keyed by Instance UID, in a
`HashMap<InstanceUid, AgentRecord>` behind a mutex ([`fleet.rs`](../../crates/server/src/fleet.rs)).
Three questions about that record need answers: whether the Agent behind it is still there, how an
operator removes a record for a host that is gone, and whether the record survives a Server
restart.

### Liveness

[ADR-0024](0024-gateway-mode.md) names the gap: "a downstream Client that vanishes without a
goodbye stays 'connected' in the fleet view until it is noticed by hand … Server-side liveness —
marking an Agent stale after a missed heartbeat interval — is the fix." The Server marks an Agent
disconnected when the WebSocket that *owns* it drops, and it records `last_seen_ms` on every report
([`fleet.rs`](../../crates/server/src/fleet.rs)). Two shapes of Agent are invisible to the
connection rule:

- a **plain-HTTP** Agent, whose polling is stateless: it is never "connected" in the first place, so
  its going away is indistinguishable from the gap between two polls;
- an Agent **behind a Gateway**, whose owning connection belongs to the Gateway and stays up.

Without a further rule, both read as they always did while nothing arrives from them, which is the
fleet view stating something it does not know.

**The protocol supplies the promise this can be checked against.** `ReportsHeartbeat` is exactly an
Agent saying it will report periodically even when nothing changes, and `OpAMPConnectionSettings`
carries the `heartbeat_interval_seconds` the Server may set. An Agent that declares that capability
has made a promise with a period attached; an Agent that does not has promised nothing, and silence
from it means nothing at all.

**Connectedness and liveness are different facts.** `connected` answers "is a connection carrying
this Agent open" — true and useful, and behind a Gateway it is the *Gateway's* connection. Whether
the Agent itself is still there is a second question, and answering it by overwriting the first
would make a WebSocket Agent's `connected` mean one thing and a polling Agent's another.

### Forgetting

`GET /api/v1/agents` returns every Agent the Server knows, and a host that is decommissioned, a VM
that was rolled, a Supervisor that was renamed each leaves a row. An Agent that stops reporting
shows `stale`, which is the right diagnosis and, for a host that is never coming back, a permanent
one. Without a way to forget, the fleet view accumulates rows describing things that no longer
exist.

**Three forces shape what "forget" may mean here.**

- **The record holds the gates that stop re-offering.** `remote_config_status`,
  `connection_settings_status`, `package_statuses`, and `sequence_num` are what let the Server say
  "this Agent already runs the intended configuration" — success criterion 3. Dropping the record
  drops all four, so an Agent that returns is offered its configuration, its connection settings, and
  its packages again as if it were new.
- **A re-offer is not free, and one of the four is not idempotent.** Packages are: the Client
  compares the offered content hash against its persisted installed record and does not download what
  it already has ([`agent.rs`](../../crates/client/src/supervisor/agent.rs), and a test asserts it).
  A remote configuration is **not**: the Client applies whatever the Server sends, and for a managed
  Agent the Collector plugin *"restarts it when a new remote configuration arrives"*
  ([`collector.rs`](../../crates/client/src/supervisor/collector.rs)). Forgetting an Agent that is
  currently running therefore bounces its Managed Process — telemetry stops for the length of a
  restart — as a side effect of an operation that sounds like housekeeping.
- **There is no per-Agent credential to revoke, so "forget" cannot mean "unenrol".** A `[auth]`
  credential is fleet-wide and *"identifies membership, not individual Agents"*
  ([ADR-0013](0013-opamp-endpoint-admission.md)), and a client certificate *"proves fleet
  membership, never which Agent is speaking"*
  ([ADR-0013](0013-opamp-endpoint-admission.md)). Nothing this Server
  holds can stop one particular Agent from connecting again. An operation that implied otherwise
  would be lying about a security property.

### Persistence

Configurations persist as one JSON file each
([ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)), packages as
artifact-plus-metadata pairs ([ADR-0015](0015-package-delivery-for-managed-processes.md)), labels as
one file per Agent ([ADR-0027](0027-server-set-labels.md)). The requirement is that the Server's
whole state — Agents, packages, Configurations — survives a restart, with only each Agent's
*status* (connected or disconnected) determined dynamically; and that the storage itself is
**replaceable**: filesystem by default, but a database or an external store must be pluggable
without touching the rest of the implementation.

**What a restart loses when Agent records live only in memory.**

- **The inventory.** `GET /api/v1/agents` starts empty. A WebSocket Agent reappears within one
  reconnect, a plain-HTTP Agent only at its next poll, and an Agent that is *not currently
  reporting* — a laptop that is off, a host mid-maintenance, a machine the operator is
  investigating precisely because it went quiet — vanishes without trace. The fleet view's answer
  to "which build is on that host" ([ADR-0009](0009-version-from-cargo-toml-and-git.md))
  is gone exactly when the host cannot repeat it.
- **The re-offer gates, for one exchange each.** After a restart every report arrives "unknown",
  the Server demands `ReportFullState`, and the whole fleet re-describes itself at once —
  harmless, but a stampede that grows with the fleet.
- **Queued operator intent.** A restart requested through `POST .../restart` but not yet delivered
  (`restart_pending`) is silently dropped.

**Three facts about the record and its storage shape what "persist" can mean.**

- `connected` and `owner` are facts about live connections. A connection identifier (`ConnId`) is
  minted per process run and means nothing across a restart; a restored `connected: true` would be
  the fleet view asserting something it cannot know — the exact defect the `stale` flag exists to
  avoid.
- The report-derived fields are protobuf messages (`AgentDescription`, `ComponentHealth`,
  `RemoteConfigStatus`, `PackageStatuses`, …) generated by prost from the pinned Baseline schema
  ([ADR-0006](0006-proto-vendoring-and-codegen.md)), and they carry no serde implementations. Any
  persistence format either re-mirrors them by hand — a second schema free to drift from the wire —
  or uses the encoding whose evolution rules the Baseline already governs: protobuf itself.
- The store's access pattern is deliberately narrow. The fleet is loaded whole at startup and held
  in memory; at runtime the store only ever receives writes and deletions for single Agents. No
  query, no partial read, no iteration — which is what makes a small, backend-agnostic interface
  possible at all.

**Write frequency is the known scale trap.** Elastic Fleet persists every check-in to its
`.fleet-agents` index — an agent is "offline" when `last_checkin` was not updated for two minutes —
and at 250 000 endpoints that is a sustained 8 333 document updates per second against one index,
a load its own maintainers document as a problem. A heartbeat exists to change nothing; a
persistence design that writes on every one converts the fleet's idle rhythm into steady disk churn.

## Decision

We will add **staleness as a fact of its own**, computed from `last_seen_ms`, reported beside
`connected` and never in place of it, and applied **only to Agents that promised to report
periodically**; we will add **`DELETE /api/v1/agents/{instance_uid}`**, which makes the Server
forget what it knows about one Agent — no more and no less — and refuse when the Agent is still
reporting; and we will **persist each Agent record behind a storage port with a filesystem adapter
as the default**, restoring the fleet from the port at startup, while **connectedness stays
runtime-only**: a restored Agent is disconnected until a live connection or report says otherwise.

### Staleness

1. **`stale` is a field on the fleet row, derived, never stored.** It is computed when the view
   is built: an Agent is stale when `now - last_seen_ms` exceeds its staleness budget. It is not
   persisted and no timer runs — the Server already records the timestamp, and a derived field
   cannot drift from it.

2. **Only an Agent declaring `ReportsHeartbeat` can go stale.** That capability is the promise that
   makes silence meaningful. An Agent that has not declared it may legitimately say nothing for
   days, and calling that stale would be the Server inventing an expectation the Agent never
   accepted. Such an Agent is never stale, whatever its `last_seen_ms` says.

3. **The budget is the offered heartbeat interval times a tolerance, or a configured default.** When
   `[connection_offer] heartbeat_interval_secs` is set, the Server knows the period it asked for and
   uses it; otherwise it uses `stale_after_secs` in `server.toml`, default **90** — three times the
   Baseline's own default heartbeat of 30 seconds. Three intervals rather than one: a single missed
   heartbeat is a lost packet, and a fleet view that flickers on every hiccup is one nobody trusts.

4. **`connected` keeps its meaning exactly.** It stays "a connection carrying this Agent is open",
   which behind a Gateway is the Gateway's connection and on plain HTTP is always false. The two
   fields answer two questions, and an operator reading `connected: true, stale: true` is being told
   precisely the truth of the gatewayed case: the pipe is up, the Agent is not talking.

5. **The bundled UI shows it, and the REST API carries it.** `AgentView` carries `stale`, so the
   OpenAPI document does too; the fleet row marks a stale Agent visibly rather than leaving an
   operator to compare timestamps by eye.

6. **The Server changes nothing about how it treats a stale Agent.** It keeps its configuration, its
   package state, and its identity; it is offered what it always would be, and its next report
   clears the flag with no special handling. Staleness is an observation, not a state transition —
   nothing is torn down on a timer, and an Agent that comes back after an outage finds the fleet
   exactly as it left it.

### Forgetting an Agent

7. **`DELETE /api/v1/agents/{instance_uid}` removes the record, and that is the whole of it.** The
   `AgentRecord` is dropped from the fleet map and from the store (clause 19), and the row
   disappears from `GET /api/v1/agents`.

8. **It reaches no host.** No process is stopped, no configuration withdrawn, no package removed, no
   credential revoked — the third force is that there is none to revoke. The word in the API
   description, the UI, and the manual is **forget**, never *delete*, *remove*, or *unenrol*: an
   operator who reads "delete agent" may reasonably expect something to happen on the machine, and
   nothing does.

9. **A still-running Agent comes back, and that is the design.** Its next report carries an identity
   the Server does not know, the Server demands `ReportFullState` as it already does for any unknown
   Agent, and the row returns complete. Forgetting is therefore never destructive to a live fleet;
   it is only ever wrong about it for one exchange.

10. **Only an Agent that is not reporting may be forgotten.** The operation succeeds when the record
    is **disconnected** *or* has been **silent for longer than the staleness budget**, and is refused
    `409` otherwise. The gate is not ceremony: the second force is that forgetting a live managed
    Agent restarts its Managed Process, and an operator who wants that has `POST .../restart`, which
    says so.

    Both halves are needed, because each covers a case the other misses. `connected` is set by every
    report and nothing but a closing WebSocket or an `agent_disconnect` ever clears it, so an Agent
    that vanishes while polling — or one behind a Gateway, where the open connection is the
    *Gateway's* — reads as connected indefinitely; silence is the only evidence there. And silence
    alone would refuse an Agent that is disconnected but was heard from a moment ago, which is
    precisely the tidy-up case.

    **Silence here is the fact, not the flag.** It is measured against clause 3's budget but
    *without* clause 2's `ReportsHeartbeat` gate. That gate is right for the flag — calling an Agent
    late is only fair if it promised to be punctual — and wrong for this question, which is about
    evidence rather than promises. With it, an Agent that declares no heartbeat and polls plain HTTP
    could never be forgotten at all: never disconnected, never stale, its row on a long-dead host
    permanent. Forgiving the promise is what makes the feature reach the case it exists for.

11. **The outcomes are `204`, `409`, `404`, `400`.** `204 No Content` when the record is gone;
    `409 Conflict` when the Agent is still reporting (clause 10); `404` when no such Agent is known,
    matching the restart endpoint's answer to the same condition rather than pretending idempotence
    the fleet map cannot distinguish from a typo; `400` when the Instance UID does not parse. The
    OpenAPI description carries all four, since the REST contract is goal 5's deliverable.

12. **Nothing expires on its own.** No retention timer, no inactivity sweep, no `ephemeral` marking.
    Forgetting is one explicit act by one operator on one Agent, which keeps clause 1's property
    that status is derived and no timer runs. An automatic policy is a genuinely different
    decision — it needs a duration, a scope, and an answer for the Agent that is merely on holiday —
    and it belongs with the retention question [ADR-0010](0010-client-os-service-and-installation-layout.md)
    deferred.

13. **The UI puts the action where the diagnosis is.** The fleet row shows the `stale` pill
    (clause 5); the forget action sits beside it, so the signal and the remedy are in one place.
    It is offered on every row rather than pre-filtered by the row's own fields: the view carries
    `connected` and `stale`, and clause 10's rule is deliberately not either of them. The Server
    holds the one rule and answers `409` with the reason, which the UI shows — one place to be right
    rather than two places to drift apart.

### Persistence

14. **The store is a port, not a module.** A small trait — `AgentStore: Send + Sync` — is the only
    thing the fleet logic knows about persistence, mirroring the hexagonal cut the Client already
    made ([ADR-0011](0011-supervisor-mode-and-lifecycle-port.md)). Its surface is the access
    pattern the context names, and nothing more:

    - `load() -> Result<HashMap<InstanceUid, PersistedAgent>, String>` — once, at startup;
    - `put(uid, &PersistedAgent)` — create or replace one record;
    - `remove(uid)` — forget one record;
    - `rekey(old, new, &PersistedAgent)` — the identity reassignment, one operation so an adapter
      with atomic rename or transactions can make it one step.

    The port speaks the **typed record** (`PersistedAgent`: the scalars plus the prost message
    types), never an encoding: what bytes or rows a backend turns that into is the adapter's own
    business. The Server crate already builds as a library, so `AppState` takes the store as a
    `Box<dyn AgentStore>`; the binary wires the default adapter, and a database or external-store
    implementation is a new adapter plus one wiring line — the rest of the implementation is,
    by construction, unaffected.

15. **The default adapter is the filesystem: one JSON file per Agent,
    `<config_dir>/agents/<instance_uid>.json`.** It follows the `LabelStore` pattern
    ([`labels.rs`](../../crates/server/src/labels.rs)): the directory is created on open, every file
    is loaded at startup, writes go through a temp file and an atomic rename, `rekey` is a write
    plus a remove, and a file that does not parse fails startup loudly rather than being skipped
    ([ADR-0008](0008-toml-configuration.md)'s principle — a fleet that silently lost members is
    worse than one that refuses to start). It nests under `config_dir` beside `labels/`, so no new
    configuration knob is added. **The envelope format is this adapter's, not the port's:** scalars
    and the effective-config text stay readable JSON, the wire-typed fields are stored as protobuf
    bytes base64-inline — the one encoding whose compatibility rules the Baseline already defines,
    so a field upstream adds round-trips unread instead of breaking a hand-mirrored schema. The
    envelope carries a `version` field so a future shape change can migrate deliberately.

16. **Everything report-derived or operator-queued persists; everything connection-scoped does
    not.** Persisted: `sequence_num`, `capabilities`, `description`, `health`, `effective_config`,
    `remote_config_status`, `connection_settings_status`, `package_statuses`,
    `available_components`, `transport` (the last one used, informational), `last_seen_ms`, and
    `restart_pending` — the queued restart is operator intent and survives like any other. Not
    persisted: `connected` and `owner`; a restored record is disconnected with no owning connection.
    Labels are also not duplicated into the record: the `LabelStore` remains their single authority,
    and the restore fills the record's mirror from it exactly as a new record does. The record also
    carries the Agent's rollout assignment (ADR-0030 clause 2).

17. **The write discipline lives above the port, so no backend can get it wrong.** The fleet layer
    keeps a hash of each record's last-written durable state and calls `put` only when that state
    changed; a report that changes nothing durable — the common heartbeat — reaches no adapter at
    all. `last_seen_ms` is deliberately outside the dirty comparison (it changes on every report)
    and rides along with whatever write happens; in addition, the Server flushes records whose
    timestamp moved on graceful shutdown, so the ordinary restart restores current last-seen
    values. After a crash, a record's last-seen is as of its last durable change — approximate,
    visibly paired with `connected: false`, and corrected by the Agent's next report.

18. **Status stays dynamic, exactly as required.** `connected` is set by live evidence only;
    `stale` and the forget-gate's silence keep being derived on read from `last_seen_ms`
    (clauses 1–3 and 10). The restored row asserts only what is known: what the Agent last
    reported, and that no connection carries it *now*.

19. **Forgetting an Agent removes it from the store; rekeying moves it.** `DELETE
    /api/v1/agents/{uid}` (clause 7) drops the record *and* calls `remove` — forgetting that left a
    stored record behind would be the "remembering under another name" rejected below. When the
    Server reassigns an identity (`RequestInstanceUid`, or the duplicate-UID rekey), the persisted
    record follows through `rekey`. The refusal gate (clause 10) and the reach-no-host guarantee
    (clause 8) are untouched.

20. **Stored records are treated as secret-bearing, whatever the backend.** A reported effective
    configuration is whatever the Managed Process runs, credentials included, and
    `package_statuses` may carry download headers. The port's contract states it; the filesystem
    adapter answers with an owner-only directory — the same reasoning that keeps the package
    store's credential-bearing metadata owner-only
    ([ADR-0015](0015-package-delivery-for-managed-processes.md)) — and a database or external adapter must
    answer with its own access control.

## Alternatives considered

- **Mark a stale Agent disconnected.** One field instead of two, and it reads naturally in the UI.
  Rejected: `connected` is a fact about a connection, and behind a Gateway that connection *is* up.
  Overwriting it would make the field mean "connected, or maybe recently talkative", which is the
  kind of quietly ambiguous state that makes a fleet view untrustworthy.
- **Have the Gateway synthesise `agent_disconnect` for a downstream peer that vanished.** The
  smallest change, and it would need nothing on the Server. Rejected in ADR-0024 and again here: the
  message asserts the Agent said goodbye, and it did not.
- **Apply staleness to every Agent, heartbeat or not.** Simpler rule, no capability check. Rejected
  in clause 2: an Agent that never promised to report periodically is not late, and flagging it
  would train operators to ignore the flag — which costs more than not having it.
- **A background sweeper that flips a stored `stale` flag on a timer.** It would let the Server log
  or push on the transition. Rejected: it adds a task and a stored state that can disagree with the
  timestamp it derives from, to buy an event nobody consumes yet. Deriving on read is exact and free.
- **Infer the period from the Agent's own reporting rhythm** instead of the offered interval.
  Rejected: it is a guess dressed as a measurement, and the first slow fleet-wide restart would make
  every Agent look late at once.
- **Hide the row instead of dropping the record** — Elastic Fleet's model, where an inactivity
  timeout moves an Agent to *inactive*: *"still valid Elastic Agents, but are removed from the main
  Fleet UI"*, and *"when Fleet Server receives a check-in from an inactive Elastic Agent, it returns
  to healthy status"*. Attractive because it keeps the re-offer gates, so a returning Agent costs no
  re-apply and no restart. Rejected as the answer here: it frees nothing, and this project already
  has the visible signal — `stale` — that such a model exists to produce. A UI filter over `stale` is
  the cheap version of Elastic's behaviour and needs no decision; forgetting is for the host that is
  genuinely gone, and it should actually forget.
- **Delete unconditionally, live Agent or not** — what Bindplane does, where removing an agent
  removes the record and a still-running collector simply reappears. Simpler, and one fewer state to
  reason about. Rejected: with this Client, a re-offered configuration restarts the Managed Process,
  so the unconditional version turns an operator's tidy-up into an unannounced outage on a healthy
  host. The condition costs one `if` and removes the only way this operation can hurt anything.
- **Gate forgetting on the `stale` flag itself**, rather than on the silence underneath it. It is
  more legible: the operator forgets exactly the rows the UI marks. Rejected once the capability
  gate is followed through: `stale` is false by construction for an Agent that never declared
  `ReportsHeartbeat`, and `connected` is never cleared for one that polls plain HTTP and stops
  without saying goodbye. An Agent that is both — a Foreign Agent with no heartbeat, or a Client
  configured `heartbeat_interval_secs = 0` — would be permanently unforgettable, which is the exact
  defect forgetting exists to fix, reintroduced in the rule meant to make it safe.
- **Preserve the re-offer gates across a forget**, keying them by Instance UID in a side table so a
  returning Agent is not re-offered anything. Rejected: it is the thing it claims not to be — a
  record of the Agent, under another name, that nothing ever removes. Forgetting that leaves a
  remembering behind is worth neither the storage nor the explanation.
- **An automatic sweep of Agents disconnected for long enough** — Bindplane purges `ephemeral=true`
  collectors after 15 minutes, Elastic unenrols inactive Agents on a configurable timeout. Genuinely
  useful for Kubernetes and autoscaled fleets, and the right answer eventually. Rejected *now*
  (clause 12): it decides a retention policy, and deciding it as a footnote to a REST endpoint
  settles a broad question through a narrow case.
- **Revoke on forget — refuse the Agent if it connects again.** Rejected on merit: the Server holds
  no per-Agent credential to revoke (third force), so the only implementation would be a deny-list
  of Instance UIDs — and an Instance UID is not an authenticator. The Server re-keys it through
  `AgentIdentification` whenever it likes, and an Agent chooses its own. Blocking on it would be
  security theatre over a value neither side treats as a secret.
- **`POST /api/v1/agents/{uid}/forget` instead of `DELETE`.** Rejected: the record *is* the resource
  the collection returns, and `DELETE` on it is what the OpenAPI description should say. The naming
  worry clause 8 raises is answered in the description and the UI label, not by bending the method.
- **Keep the fleet in memory.** Rejected: it is the requirement's stated gap, and the losses in the
  context — the inventory above all — are real operator costs, not aesthetics.
- **A concrete filesystem store, no port** — the `LabelStore` pattern as-is, which is how every
  other store is built. The simplest thing that persists, and the strongest YAGNI candidate.
  Rejected because the second implementation is not speculative here: the requirement itself names
  a database and an external store as implementations that must be possible without touching the
  rest — and retrofitting a port under a concrete store rewrites exactly the call sites the port
  exists to protect. The port is kept to four operations so the abstraction costs little more than
  the concrete store would.
- **A generic key-value port (`uid → bytes`) instead of a typed one.** One trait that every future
  store (Configurations, labels) could share, and adapters become trivial blob stores. Rejected:
  it decides the encoding *above* the port, so every backend stores the same opaque envelope — a
  database implementation degenerates into a blob table, and an external store cannot map records
  to its own schema. The typed port keeps the encoding where the requirement wants the freedom:
  in the adapter.
- **An embedded database as the default (SQLite; Bindplane's bbolt-then-Postgres path).** One
  file, transactions, indexed queries. Rejected as the *default*: it adds a dependency and a
  second storage idiom to a Server whose idiom is file-per-item JSON, three times over — for data
  that is small, per-Agent, and loaded whole at startup. The port is precisely what lets a
  deployment that outgrows files add that adapter later; Bindplane's own trajectory (bbolt
  deprecated, Postgres for scale) is an argument for the *port*, not for shipping a database now.
- **Persist every report, heartbeats included (Elastic Fleet's model).** The most accurate
  `last_seen_ms` money can buy. Rejected: it is write amplification with near-zero information
  gain, documented as a scale problem by the system that does it (8 333 writes/second at 250 000
  agents), and the value it buys — an exact rather than approximate last-seen after a *crash* —
  is marginal next to clause 17's shutdown flush.
- **One snapshot file for the whole fleet, written periodically.** Fewer files, one loader.
  Rejected: every change rewrites the world or waits for a timer, a crash loses the tail, and
  forget/rekey become read-modify-write of a shared file instead of one record's operation.
- **Serde derives on the generated protobuf types** (prost-build `type_attribute`). Fully readable
  files. Rejected: it welds the on-disk format to prost's generated field names, so an upstream
  rename or the codegen's evolution breaks every stored record at parse time — and JSON of
  generated types drops unknown fields anyway, so it is no better at surviving schema motion than
  protobuf bytes, which are governed by compatibility rules designed for exactly this.
- **Persist only the inventory (description and friends) and drop the re-offer gates.** Smaller
  records, and the gates rebuild from the Agent's first full report anyway. Rejected: it keeps the
  restart stampede — every Agent is demanded `ReportFullState` and every gate is rebuilt by
  round-trip — when persisting four more fields makes a restart invisible to the fleet, which is
  the requirement's plain intent ("only status is dynamic").
- **One repository port over all four stores now** (Agents, Configurations, labels, packages).
  The consistent end state if a database backend is ever to hold everything. Rejected *here*:
  packages are not a record store — artifacts are streamed to disk, staged, rolled back
  ([ADR-0015](0015-package-delivery-for-managed-processes.md), [ADR-0016](0016-a-package-is-a-versioned-set.md))
  — so that port is a much wider design, and folding it into this decision settles a broad
  question through a narrow case (the mistake clause 12 declines for retention). The pattern
  decided here is deliberately the template; extending it to the other stores is named as a
  follow-up by topic.

## Sources / Prior art

- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md),
  Baseline `v0.19.0` — `ReportsHeartbeat` as the Agent's promise to report periodically, and its
  default interval of 30 seconds, which is where clause 3's default comes from. It is **silent** on
  server-side record lifetime: it says `AgentDisconnect` *"MUST be set in the last AgentToServer
  message"* without saying what a Server does with the record afterwards, and defines no retention,
  expiry, or removal. The one removal it does describe is credential-shaped — the Server *"can
  revoke access to individual Agents by marking the corresponding connection settings as 'revoked'
  and disconnecting the Client"* — which is a different operation from forgetting, and one this
  project cannot perform per Agent (third force). The absence of an oracle is the finding: this
  decision carries its own justification. The protocol's own recovery for lost server state
  (`ReportFullState`) is what makes persistence an optimisation of truth and cost rather than a
  correctness requirement.
- [Bindplane ephemeral collectors](https://docs.bindplane.com/how-to-guides/collector-management/golden-images-and-ephemeral-collectors)
  — the closest precedent for forgetting. Collectors marked `ephemeral=true` are swept once
  *"disconnected 15 or more minutes"*, the stated purpose being to *"clean up agents that aren't
  likely to ever come back"*; and on the question clause 9 answers, Bindplane is explicit:
  *"If they do come back, the system will just treat them as a new agent again while using the
  previous agent ID."* Confirmation that a forgotten-then-returning Agent is a normal state rather
  than an error to engineer away.
- [Elastic Fleet inactivity timeout](https://www.elastic.co/docs/reference/fleet/set-inactivity-timeout)
  and [unenrolment](https://www.elastic.co/docs/reference/fleet/unenroll-elastic-agent) — the
  two-stage model, and the source of the "hide the row" alternative. *Inactive* hides the row and
  is reversible on the next check-in; *unenrol* is the real removal and *"revoke[s] the API keys"*,
  after which *"unenrolled agents need to be re-enrolled to be operational again"*. The split is
  instructive and deliberately not copied: its second stage rests on a per-Agent credential this
  project does not have, and claiming the shape without the mechanism would be the lie clause 8
  avoids.
- [Elastic Fleet](https://www.elastic.co/docs/reference/fleet/monitor-elastic-agent) — the
  closest model to the persistence decided: the agent inventory is persisted (the `.fleet-agents`
  index) and *offline* is **derived** from `last_checkin` against a timeout, never stored. Also the
  cautionary half: [fleet-server issue #749](https://github.com/elastic/fleet-server/issues/749)
  documents the write load of persisting every 30-second check-in at scale, which is what clause
  17's dirty-check exists to avoid.
- [Bindplane Bolt Store](https://bindplane.com/docs/advanced-setup/backup-and-disaster-recovery/bolt-store)
  and [Postgres Store](https://docs.bindplane.com/how-to-guides/postgres/postgres-store) — a
  fleet server whose agent records live behind exactly the abstraction clause 14 decides: one store
  interface, two interchangeable backends (bbolt, deprecated in favour of Postgres for production),
  selected by configuration and invisible to the rest of the server. Prior art both for the port
  and for not shipping the database as the default.
- [Ports and adapters / hexagonal architecture](https://alistair.cockburn.us/hexagonal-architecture/)
  (Cockburn) — the pattern behind clause 14, already this project's vocabulary on the Client side
  ([ADR-0011](0011-supervisor-mode-and-lifecycle-port.md)).
- [opamp-go example server](https://github.com/open-telemetry/opamp-go) — the reference
  implementation this project tracks for interoperability ([ADR-0004](0004-protocol-baseline-and-conformance.md))
  keeps its agent map in memory; it is an example, not a product, and offers no persistence design
  to follow.
- This repository: [ADR-0024](0024-gateway-mode.md) — names the liveness gap and explains why the
  Gateway cannot close it itself; [ADR-0014](0014-server-driven-connection-settings.md) — the
  offered `heartbeat_interval_seconds` the budget of clause 3 is built on; the Server's
  `last_seen_ms` and per-connection `connected` ([`fleet.rs`](../../crates/server/src/fleet.rs)),
  which staleness reads rather than adding state;
  [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md) (the REST API and its
  OpenAPI contract), [ADR-0015](0015-package-delivery-for-managed-processes.md), and
  [ADR-0027](0027-server-set-labels.md) (the file-per-item store idiom the default adapter reuses;
  ADR-0015 also for the package re-offer gate and the Client-side idempotence that makes it
  harmless); [ADR-0013](0013-opamp-endpoint-admission.md) and
  [ADR-0013](0013-opamp-endpoint-admission.md) (fleet-wide membership,
  not per-Agent identity); [ADR-0006](0006-proto-vendoring-and-codegen.md) (why the wire types
  have no serde and why protobuf bytes are the stable encoding).

## Consequences

- Positive: the fleet view stops asserting something it does not know. The three cases it was blind
  to — a gatewayed Agent, a polling Agent, and a WebSocket Agent whose process wedged without
  dropping the socket — all become visible, and by one rule rather than three.
- Positive: staleness stores nothing, runs no task, and treats no Agent differently. It cannot cause
  an outage, which is the right risk profile for something whose whole job is to report on other
  things going wrong.
- Positive: the fleet view can be made true again. A decommissioned host stops occupying a row
  forever, and the `stale` flag has a consumer — a diagnosis with a remedy next to it rather than
  one the operator can only look at.
- Positive: forgetting cannot damage a running fleet. It touches no host, and clause 10 keeps it
  away from any Agent still reporting; the worst case is a row that briefly disappears and comes
  back complete.
- Positive: the fleet view survives a restart. Every Agent the Server knew — including the ones not
  currently reporting, which are exactly the ones that cannot re-announce themselves — keeps its
  row, its last-reported build, health, and configuration state, shown honestly as disconnected.
- Positive: a Server restart becomes invisible to the fleet. Restored `sequence_num` and gate
  hashes mean a reconnecting Agent's compressed heartbeat is accepted in place of a fleet-wide
  `ReportFullState` stampede, and nothing is re-offered that already runs.
- Positive: storage is a deployment choice, not an architecture change. A database or external
  store is a new adapter behind a four-operation trait plus its wiring; the fleet logic, the REST
  API, and the transports never learn which one is running. The port also hands the tests an
  in-memory fake for free.
- Positive: a queued restart survives the Server restarting, and forgetting an Agent frees the
  store as well as the row.
- Negative / trade-offs: an Agent that declares no heartbeat is never stale, by design. That is
  the honest position — it promised nothing — but an operator who wants the signal has to configure
  heartbeats, and the manual has to say so where they will read it.
- Negative / trade-offs: the staleness budget is a Server-side guess whenever no interval was
  offered. `stale_after_secs` covers a fleet with one rhythm; a fleet with several will have to
  pick the slowest, or offer an interval and get the exact answer.
- Negative / trade-offs: **a forgotten Agent that returns costs one re-apply.** The Server has lost
  the hashes that would have told it to stay quiet, so it re-offers configuration, connection
  settings, and packages. The packages are free — the Client re-installs nothing it already has —
  but the configuration is applied again, and for a managed Agent that is one restart of the Managed
  Process. Bounded, one-off, and only ever paid by a host the operator had already written off, but
  it is a real cost and the manual should say so plainly.
- Negative / trade-offs: forgetting is not unenrolling, and some operator will expect it to be. A
  still-configured Client reconnects and reappears, and the only way to stop it is on the host. That
  is the honest position given fleet-wide credentials, and it is what both Bindplane and Elastic's
  first stage do — but it is the sentence most likely to be missed, which is why clause 8 puts the
  word *forget* in the interface rather than only in this document.
- Negative / trade-offs: what the UI offers and what the Server allows are not the same set. Clause
  10's rule is not expressible in the fields `AgentView` carries, so the button is offered
  everywhere and a refusal comes back as a `409` the operator reads after clicking. The alternative
  was a third view field that exists only to grey out a button, or two copies of the rule; a message
  is cheaper than either, but it is a click that can fail rather than one that cannot.
- Negative / trade-offs: an Agent that is *disconnected* is forgettable immediately, with no silence
  required — a WebSocket that dropped a second ago qualifies. That is intended (a dropped connection
  is evidence enough, and waiting would help nobody), but it means a flapping Agent can be forgotten
  in the gap between two connections and pay the re-apply when it comes back.
- Negative / trade-offs: the fleet view and the store are unbounded for anyone who never forgets.
  Nothing expires by itself (clause 12), so a large autoscaled deployment accumulates rows and
  stored records with every Agent ever seen, and the manual remedy does not scale to hundreds of
  them.
- Negative / trade-offs: a port with one shipped adapter is surface carried ahead of its second
  user. Accepted deliberately — the requirement names that second user — and bounded by keeping
  the trait at four operations and the write discipline outside it.
- Negative / trade-offs: after a crash (not a graceful stop), a restored `last_seen_ms` dates from
  the Agent's last durable change, not its last heartbeat. An idle-but-alive Agent can briefly read
  as silent — and therefore forgettable — until its next report corrects it; the window is one
  heartbeat interval for a live Agent.
- Negative / trade-offs: the default adapter's files are only half human-readable — scalars and
  the effective-config text are plain, the wire-typed fields are base64 protobuf. An operator can
  see *that* a record exists and delete it by hand, but not edit what the Agent reported; that is
  arguably correct (the Server should not invent reports), but it is a difference from the fully
  readable Configuration files.
- Negative / trade-offs: a corrupt record in the default adapter fails startup, consistent with
  every other store's loud-failure principle — the remedy (delete the named file) costs that Agent
  one `ReportFullState`.
- Follow-ups: acting on staleness — an alert, a webhook, a REST filter — is deliberately not
  decided; the field exists first, and what reads it is a decision with a user behind it. An
  automatic retention policy for Agents nobody claims — a duration, whether it is fleet-wide or
  targeted, and how an Agent on a long holiday is spared — which clause 12 declines to settle and
  which meets the version-directory retention ADR-0010 deferred. A bulk or filtered form of
  forgetting, if a deployment ever needs to forget more rows than a person wants to click. A
  configuration surface for selecting and parameterising a storage backend (a `[storage]` section),
  which only earns its keep once a second adapter exists; extending the port pattern to the
  Configuration, label, and package stores, each a decision of its own; and observability of the
  store itself — a count and byte size in a health or status surface, should the footprint ever
  need watching.
