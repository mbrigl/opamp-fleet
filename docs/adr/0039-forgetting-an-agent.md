# ADR-0039: Forgetting an Agent — the fleet view drops a record, and reaches no host

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

The fleet view has no way to forget anything. `GET /api/v1/agents` returns every Agent the Server
has ever heard from, and the only other operation on one is
`POST /api/v1/agents/{instance_uid}/restart` ([`api.rs`](../../crates/server/src/api.rs)). A host
that is decommissioned, a VM that was rolled, a Supervisor that was renamed — each leaves a row that
nothing can remove.

[ADR-0038](0038-an-agent-that-stops-reporting-goes-stale.md) made that visible without making it
actionable. An Agent that stops reporting now shows `stale`, which is the right diagnosis and, for a
host that is never coming back, a permanent one. The fleet view accumulates rows describing things
that no longer exist, and the operator's only remedy is to restart the Server.

**Four forces shape what "forget" may mean here.**

- **The record is memory, not storage.** The fleet is a `HashMap<InstanceUid, AgentRecord>` behind a
  mutex ([`fleet.rs`](../../crates/server/src/fleet.rs)) and nothing writes it to disk — unlike
  Configurations and packages, which persist. A Server restart therefore *already* forgets every
  Agent, and the connected ones reappear within one exchange. Whatever this decision builds is not a
  new capability so much as the ability to aim an existing one at a single row.
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
  ([ADR-0013](0013-opamp-endpoint-authentication.md)), and a client certificate *"proves fleet
  membership, never which Agent is speaking"*
  ([ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md)). Nothing this Server
  holds can stop one particular Agent from connecting again. An operation that implied otherwise
  would be lying about a security property.

## Decision

We will add **`DELETE /api/v1/agents/{instance_uid}`**, which makes the Server forget what it knows
about one Agent — no more and no less — and refuse when the Agent is still reporting.

1. **It removes the record, and that is the whole of it.** The `AgentRecord` is dropped from the
   fleet map and the row disappears from `GET /api/v1/agents`. Nothing is written to disk because
   nothing was: this is exactly what a Server restart does to every Agent, aimed at one.

2. **It reaches no host.** No process is stopped, no configuration withdrawn, no package removed, no
   credential revoked — the fourth force says there is none to revoke. The word in the API
   description, the UI, and the manual is **forget**, never *delete*, *remove*, or *unenrol*: an
   operator who reads "delete agent" may reasonably expect something to happen on the machine, and
   nothing does.

3. **A still-running Agent comes back, and that is the design.** Its next report carries an identity
   the Server does not know, the Server demands `ReportFullState` as it already does for any unknown
   Agent, and the row returns complete. Forgetting is therefore never destructive to a live fleet;
   it is only ever wrong about it for one exchange.

4. **Only an Agent that is not reporting may be forgotten.** The operation succeeds when the record
   is **disconnected** *or* has been **silent for longer than the staleness budget**, and is refused
   `409` otherwise. The gate is not ceremony: the third force is that forgetting a live managed Agent
   restarts its Managed Process, and an operator who wants that has `POST .../restart`, which says
   so.

   Both halves are needed, because each covers a case the other misses. `connected` is set by every
   report and nothing but a closing WebSocket or an `agent_disconnect` ever clears it, so an Agent
   that vanishes while polling — or one behind a Gateway, where the open connection is the
   *Gateway's* — reads as connected indefinitely; silence is the only evidence there. And silence
   alone would refuse an Agent that is disconnected but was heard from a moment ago, which is
   precisely the tidy-up case.

   **Silence here is the fact, not the flag.** It is measured against ADR-0038's budget but *without*
   its `ReportsHeartbeat` gate. That gate is right for the flag — calling an Agent late is only fair
   if it promised to be punctual — and wrong for this question, which is about evidence rather than
   promises. With it, an Agent that declares no heartbeat and polls plain HTTP could never be
   forgotten at all: never disconnected, never stale, its row on a long-dead host permanent. Forgiving
   the promise is what makes the feature reach the case it exists for.

5. **The outcomes are `204`, `409`, `404`, `400`.** `204 No Content` when the record is gone;
   `409 Conflict` when the Agent is still reporting (point 4); `404` when no such Agent is known,
   matching the restart endpoint's answer to the same condition rather than pretending idempotence
   the fleet map cannot distinguish from a typo; `400` when the Instance UID does not parse. The
   OpenAPI description carries all four, since the REST contract is goal 5's deliverable.

6. **Nothing expires on its own.** No retention timer, no inactivity sweep, no `ephemeral` marking.
   Forgetting is one explicit act by one operator on one Agent, which keeps ADR-0038's property that
   the fleet view stores nothing and runs no timers. An automatic policy is a genuinely different
   decision — it needs a duration, a scope, and an answer for the Agent that is merely on holiday —
   and it belongs with the retention question [ADR-0010](0010-client-os-service-and-cli.md) deferred.

7. **The UI puts the action where the diagnosis is.** The fleet row already shows the `stale` pill
   ADR-0038 added; the forget action sits beside it, so the signal and the remedy are in one place.
   It is offered on every row rather than pre-filtered by the row's own fields: the view carries
   `connected` and `stale`, and point 4's rule is deliberately not either of them. The Server holds
   the one rule and answers `409` with the reason, which the UI shows — one place to be right rather
   than two places to drift apart.

## Alternatives considered

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
- **Gate on ADR-0038's `stale` flag itself**, rather than on the silence underneath it. This is what
  an earlier draft of point 4 said, and it is more legible: the operator forgets exactly the rows the
  UI marks. Rejected once the capability gate was followed through: `stale` is false by construction
  for an Agent that never declared `ReportsHeartbeat`, and `connected` is never cleared for one that
  polls plain HTTP and stops without saying goodbye. An Agent that is both — a Foreign Agent with no
  heartbeat, or a Client configured `heartbeat_interval_secs = 0` — would have been permanently
  unforgettable, which is the exact defect this ADR set out to fix, reintroduced in the rule meant to
  make it safe.
- **Preserve the re-offer gates across a forget**, keying them by Instance UID in a side table so a
  returning Agent is not re-offered anything. Rejected: it is the thing it claims not to be — a
  record of the Agent, under another name, that nothing ever removes. Forgetting that leaves a
  remembering behind is worth neither the storage nor the explanation.
- **An automatic sweep of Agents disconnected for long enough** — Bindplane purges `ephemeral=true`
  collectors after 15 minutes, Elastic unenrols inactive Agents on a configurable timeout. Genuinely
  useful for Kubernetes and autoscaled fleets, and the right answer eventually. Rejected *now*
  (point 6): it decides a retention policy, and deciding it as a footnote to a REST endpoint settles
  a broad question through a narrow case.
- **Revoke on forget — refuse the Agent if it connects again.** Rejected on merit: the Server holds
  no per-Agent credential to revoke (fourth force), so the only implementation would be a deny-list
  of Instance UIDs — and an Instance UID is not an authenticator. The Server re-keys it through
  `AgentIdentification` whenever it likes, and an Agent chooses its own. Blocking on it would be
  security theatre over a value neither side treats as a secret.
- **`POST /api/v1/agents/{uid}/forget` instead of `DELETE`.** Rejected: the record *is* the resource
  the collection returns, and `DELETE` on it is what the OpenAPI description should say. The naming
  worry point 2 raises is answered in the description and the UI label, not by bending the method.

## Sources / Prior art

- **[OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)**,
  Baseline `v0.19.0` — checked first, and **silent** on server-side record lifetime: it says
  `AgentDisconnect` *"MUST be set in the last AgentToServer message"* without saying what a Server
  does with the record afterwards, and defines no retention, expiry, or removal. The one removal it
  does describe is credential-shaped — the Server *"can revoke access to individual Agents by marking
  the corresponding connection settings as 'revoked' and disconnecting the Client"* — which is a
  different operation from the one decided here, and one this project cannot perform per Agent
  (fourth force). As with Gateway Mode, the absence of an oracle is the finding: this decision
  carries its own justification.
- **[Bindplane ephemeral collectors](https://docs.bindplane.com/how-to-guides/collector-management/golden-images-and-ephemeral-collectors)**
  — the closest precedent for the operation itself. Collectors marked `ephemeral=true` are swept once
  *"disconnected 15 or more minutes"*, the stated purpose being to *"clean up agents that aren't
  likely to ever come back"*; and on the question this ADR's point 3 answers, Bindplane is explicit:
  *"If they do come back, the system will just treat them as a new agent again while using the
  previous agent ID."* Confirmation that a forgotten-then-returning Agent is a normal state rather
  than an error to engineer away.
- **[Elastic Fleet inactivity timeout](https://www.elastic.co/docs/reference/fleet/set-inactivity-timeout)**
  and **[unenrolment](https://www.elastic.co/docs/reference/fleet/unenroll-elastic-agent)** — the
  two-stage model, and the source of the first rejected alternative. *Inactive* hides the row and is
  reversible on the next check-in; *unenrol* is the real removal and *"revoke[s] the API keys"*, after
  which *"unenrolled agents need to be re-enrolled to be operational again"*. The split is instructive
  and deliberately not copied: its second stage rests on a per-Agent credential this project does not
  have, and claiming the shape without the mechanism would be the lie point 2 avoids.
- This repository: [ADR-0038](0038-an-agent-that-stops-reporting-goes-stale.md) (the `stale` flag
  point 4 gates on, and its "no storage, no timers" property point 6 keeps),
  [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md) (the REST API and its
  OpenAPI contract), [ADR-0013](0013-opamp-endpoint-authentication.md) and
  [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md) (fleet-wide membership, not
  per-Agent identity), [ADR-0015](0015-package-delivery-for-managed-processes.md) (the package
  re-offer gate and the Client-side idempotence that makes it harmless).

## Consequences

- Positive: the fleet view can be made true again. A decommissioned host stops occupying a row
  forever, and the `stale` flag ADR-0038 added gets its first consumer — a diagnosis with a remedy
  next to it rather than one the operator can only look at.
- Positive: the operation cannot damage a running fleet. It touches no host, and point 4 keeps it
  away from any Agent still reporting; the worst case is a row that briefly disappears and comes
  back complete.
- Negative / trade-offs: **a forgotten Agent that returns costs one re-apply.** The Server has lost
  the hashes that would have told it to stay quiet, so it re-offers configuration, connection
  settings, and packages. The packages are free — the Client re-installs nothing it already has —
  but the configuration is applied again, and for a managed Agent that is one restart of the Managed
  Process. Bounded, one-off, and only ever paid by a host the operator had already written off, but
  it is a real cost and the manual should say so plainly.
- Negative / trade-offs: forgetting is not unenrolling, and some operator will expect it to be. A
  still-configured Client reconnects and reappears, and the only way to stop it is on the host. That
  is the honest position given fleet-wide credentials, and it is what both Bindplane and Elastic's
  first stage do — but it is the sentence most likely to be missed, which is why point 2 puts the
  word *forget* in the interface rather than only in this document.
- Negative / trade-offs: what the UI offers and what the Server allows are not the same set. Point 4's
  rule is not expressible in the fields `AgentView` carries, so the button is offered everywhere and
  a refusal comes back as a `409` the operator reads after clicking. The alternative was a third view
  field that exists only to grey out a button, or two copies of the rule; a message is cheaper than
  either, but it is a click that can fail rather than one that cannot.
- Negative / trade-offs: an Agent that is *disconnected* is forgettable immediately, with no silence
  required — a WebSocket that dropped a second ago qualifies. That is intended (a dropped connection
  is evidence enough, and waiting would help nobody), but it means a flapping Agent can be forgotten
  in the gap between two connections and pay the re-apply when it comes back.
- Negative / trade-offs: the fleet view stays unbounded for anyone who never uses this. Nothing
  expires by itself (point 6), so a large autoscaled deployment accumulates rows exactly as it does
  today, and now has a manual remedy that does not scale to hundreds of them.
- Follow-ups: an automatic retention policy for Agents nobody claims — a duration, whether it is
  fleet-wide or targeted, and how an Agent on a long holiday is spared — which is the question point
  6 declines to settle here and which meets the version-directory retention ADR-0010 deferred; a
  bulk or filtered form of this operation, if a deployment ever needs to forget more rows than a
  person wants to click; and the operator authentication for the REST API that ADR-0013 and ADR-0035
  both leave open — this endpoint is not more dangerous than the ones beside it, but it is one more
  thing an unauthenticated caller can do.
