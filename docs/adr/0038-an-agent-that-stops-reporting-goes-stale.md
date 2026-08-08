# ADR-0038: An Agent that stops reporting goes stale — liveness beside connectedness, never instead of it

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

[ADR-0037](0037-gateway-mode.md) shipped Gateway Mode and named its own sharpest edge: "a downstream
Client that vanishes without a goodbye stays 'connected' in the fleet view until it is noticed by
hand … Server-side liveness — marking an Agent stale after a missed heartbeat interval — is the fix,
and it is a decision of its own rather than something to bolt on here." This is that decision.

**The gap is older than the Gateway, which only made it obvious.** The Server marks an Agent
disconnected when the WebSocket that *owns* it drops, and it records `last_seen_ms` on every report
([`fleet.rs`](../../crates/server/src/fleet.rs)) — but nothing ever reads that timestamp back.
Two shapes of Agent were already invisible to the connection rule:

- a **plain-HTTP** Agent, whose polling is stateless: it is never "connected" in the first place, so
  its going away is indistinguishable from the gap between two polls;
- an Agent **behind a Gateway**, whose owning connection belongs to the Gateway and stays up.

Both read as they always did while nothing arrives from them, which is the fleet view stating
something it does not know.

**The protocol supplies the promise this can be checked against.** `ReportsHeartbeat` is exactly an
Agent saying it will report periodically even when nothing changes, and `OpAMPConnectionSettings`
carries the `heartbeat_interval_seconds` the Server may set. An Agent that declares that capability
has made a promise with a period attached; an Agent that does not has promised nothing, and silence
from it means nothing at all.

**Connectedness and liveness are different facts, and the fleet view currently has one field for
them.** `connected` answers "is a connection carrying this Agent open" — true and useful, and behind
a Gateway it is the *Gateway's* connection. Whether the Agent itself is still there is a second
question, and answering it by overwriting the first would make a WebSocket Agent's `connected` mean
one thing and a polling Agent's another.

## Decision

We will add **staleness as a fact of its own**, computed from `last_seen_ms`, reported beside
`connected` and never in place of it, and applied **only to Agents that promised to report
periodically**.

1. **`stale` is a new field on the fleet row, derived, never stored.** It is computed when the view
   is built: an Agent is stale when `now - last_seen_ms` exceeds its staleness budget. Nothing is
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

5. **The bundled UI shows it, and the REST API carries it.** `AgentView` gains `stale`, so the
   OpenAPI document does too; the fleet row marks a stale Agent visibly rather than leaving an
   operator to compare timestamps by eye.

6. **The Server changes nothing about how it treats a stale Agent.** It keeps its configuration, its
   package state, and its identity; it is offered what it always would be, and its next report
   clears the flag with no special handling. Staleness is an observation, not a state transition —
   nothing is torn down on a timer, and an Agent that comes back after an outage finds the fleet
   exactly as it left it.

## Alternatives considered

- **Mark a stale Agent disconnected.** One field instead of two, and it reads naturally in the UI.
  Rejected: `connected` is a fact about a connection, and behind a Gateway that connection *is* up.
  Overwriting it would make the field mean "connected, or maybe recently talkative", which is the
  kind of quietly ambiguous state that makes a fleet view untrustworthy.
- **Have the Gateway synthesise `agent_disconnect` for a downstream peer that vanished.** The
  smallest change, and it would need nothing on the Server. Rejected in ADR-0037 and again here: the
  message asserts the Agent said goodbye, and it did not. This decision exists precisely because
  that shortcut was refused.
- **Apply staleness to every Agent, heartbeat or not.** Simpler rule, no capability check. Rejected
  in point 2: an Agent that never promised to report periodically is not late, and flagging it would
  train operators to ignore the flag — which costs more than not having it.
- **A background sweeper that flips a stored `stale` flag on a timer.** It would let the Server log
  or push on the transition. Rejected: it adds a task and a stored state that can disagree with the
  timestamp it derives from, to buy an event nobody consumes yet. Deriving on read is exact and free.
- **Infer the period from the Agent's own reporting rhythm** instead of the offered interval.
  Rejected: it is a guess dressed as a measurement, and the first slow fleet-wide restart would make
  every Agent look late at once.

## Sources / Prior art

- [ADR-0037](0037-gateway-mode.md) — names this gap as the follow-up and explains why the Gateway
  cannot close it itself.
- [ADR-0014](0014-server-driven-connection-settings.md) — the offered `heartbeat_interval_seconds`
  the budget of point 3 is built on.
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) —
  `ReportsHeartbeat` as the Agent's promise to report periodically, and its default interval of 30
  seconds, which is where point 3's default comes from. Baseline `v0.19.0`.
- The Server's existing `last_seen_ms` and per-connection `connected`
  ([`fleet.rs`](../../crates/server/src/fleet.rs)) — both already recorded; this decision reads one
  of them rather than adding state.

## Consequences

- Positive: the fleet view stops asserting something it does not know. The three cases it was blind
  to — a gatewayed Agent, a polling Agent, and a WebSocket Agent whose process wedged without
  dropping the socket — all become visible, and by one rule rather than three.
- Positive: nothing is stored, no task runs, and no Agent is treated differently for being stale. The
  feature cannot cause an outage, which is the right risk profile for something whose whole job is to
  report on other things going wrong.
- Negative / trade-offs: an Agent that declares no heartbeat is still invisible, by design. That is
  the honest position — it promised nothing — but an operator who wants the signal has to configure
  heartbeats, and the manual has to say so where they will read it.
- Negative / trade-offs: the budget is a Server-side guess whenever no interval was offered.
  `stale_after_secs` covers a fleet with one rhythm; a fleet with several will have to pick the
  slowest, or offer an interval and get the exact answer.
- Follow-ups: acting on staleness — an alert, a webhook, a REST filter — is deliberately not part of
  this. The field exists first; what reads it is a decision with a user behind it.
