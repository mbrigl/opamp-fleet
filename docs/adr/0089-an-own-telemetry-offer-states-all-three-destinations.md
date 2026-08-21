# ADR-0089: An own-telemetry offer states all three destinations — and an empty endpoint withdraws one

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

Decides what [ADR-0086](0086-a-telemetry-destination-is-an-offer-of-its-own-class.md) left without an
owner. ADR-0086 superseded point 7 of [ADR-0036](0036-agents-report-their-own-telemetry.md) — the
point that said the destinations are *"folded, persisted, and applied exactly as the `opamp` settings
are"* — and replaced its *lifecycle* with a class of its own, without restating what folding means
for that class. Since then the fold rests on nothing but a comment in the vendored schema and the
code that reads it. This ADR states the rule and changes it. It supersedes nothing.

## Context

**Own telemetry can be switched on from the fleet, moved from the fleet, and never switched off.**
A destination that is once in force stays in force: it survives reconnects by design (ADR-0086) and
restarts by design (the persisted settings are put back before the first exchange). Every path that
would end it is closed:

- The Client folds per signal. `connection::merge` takes an offered destination or, failing that,
  the stored one — so an offer that omits `own_traces` leaves the traces exporter running, and an
  offer that names an *empty* endpoint is discarded in favour of the stored one.
- The Server falls silent. With `[telemetry_offer]` removed and no `[connection_offer]`,
  `fleet.rs::settings_offer` returns `None`: there is no message at all, so the Client has nothing
  to act on even if it wanted to.

What remains is deleting `connection-settings.pb` on the host and restarting the Client — an
intervention per machine, which is the class of work this project exists to remove.

**The Baseline closes the obvious reading, and it does so deliberately.** The schema this project
vendors is explicit for each of the three fields — *"Settings to connect to an OTLP metrics backend
to send Agent's own metrics to. If this field is not set then the Agent should assume that the
settings are unchanged"* — and the `hash` field is defined as *"Hash of all settings, including
settings that may be omitted from this message because they are unchanged."* An offer is a delta
carrying a full-state hash. Omission therefore cannot mean "stop"; that reading is excluded in
writing, not merely unspecified.

**And no withdrawal is defined in its place.** `destination_endpoint` says *"The value MUST be a
full URL an OTLP/HTTP/Protobuf receiver with path"*, so the empty string is not a legal value and
carries no meaning. Nowhere does the specification say how a Server ends own-telemetry reporting, or
what an Agent does when it should stop. The one revocation these messages *do* define is for the
client certificate — *"This field is optional: if omitted the client SHOULD NOT use a client-side
certificate. This field can be used to perform a client certificate revocation/rotation"* — which
is field omission inside a *present* message meaning "stop using it". The protocol can express
withdrawal. It just does not express this one.

**So the Client is conformant and the feature is unusable.** That combination is what makes this a
decision rather than a bug report.

**The reference implementation resolves it the other way — in code, not in prose.** The
`opampsupervisor` of `opentelemetry-collector-contrib`, built on `opamp-go`, is the one widely
deployed Client that implements these fields. It reads an offer as follows:

```go
func (*Supervisor) updateOwnTelemetryData(data map[string]any, signal string, settings *protobufs.TelemetryConnectionSettings) map[string]any {
	if settings == nil || settings.DestinationEndpoint == "" {
		return data
	}
```

```go
	data := s.updateOwnTelemetryData(map[string]any{}, "Metrics", settings.GetOwnMetrics())
	data = s.updateOwnTelemetryData(data, "Logs", settings.GetOwnLogs())
	data = s.updateOwnTelemetryData(data, "Traces", settings.GetOwnTraces())

	if len(data) == 0 {
		s.telemetrySettings.Logger.Debug("Disabling own telemetry pipeline in the config")
	}
```

The map starts **empty on every message**. Nothing is folded in: `processOwnTelemetryConnSettingsMessage`
hands the received message straight to `setupOwnTelemetry`, and the layer below it —
`opamp-go`'s `receivedprocessor` — passes each field through untouched, gated only on the
corresponding capability. What is persisted is the last received *message*, replayed through the
same function at startup. Two rules follow, and the code states both plainly:

- **An offer that names any telemetry destination states all three.** A signal absent from that
  message is dropped from the generated configuration, not carried over.
- **An empty `destination_endpoint` is a withdrawal.** It takes the same branch as an absent one, and
  when the last of the three goes the supervisor logs it as *disabling*.

One nuance keeps this from contradicting the schema outright: the supervisor's dispatch only enters
that path `if msg.OwnMetricsConnSettings != nil || msg.OwnTracesConnSettings != nil ||
msg.OwnLogsConnSettings != nil`. An offer naming none of the three changes nothing — which is the
schema's rule, at the level of the message rather than the field. The delta is between *messages
about telemetry* and *messages that are silent about it*, not between individual fields.

**The two other implementations we looked at avoid the question entirely.** Elastic's `fleet-server`
runs OpAMP in what its own documentation calls *monitoring-only mode*, listing "No connection
settings management" among the server-to-agent features it does not implement — its agents' self
telemetry is switched in the agent policy, i.e. in the configuration channel. Bindplane reports
collector self-telemetry over a **custom capability**, `com.bindplane.measurements.v1` with a
`reportMeasurements` message, fed by a processor that lives in the collector configuration it
pushes — again the configuration channel, which is full-state and can therefore express removal.
Neither ships an off switch built on `own_metrics`, because there is none to build on.

That is the shape of the field: of three implementations, two route around the mechanism and the
third redefines it. Nobody implements the delta.

**This project has already named the tie-breaker.** [ADR-0040](0040-interoperability-against-opamp-go.md)
made `opamp-go` the *behavioural oracle* precisely because both ends here were written from the same
sentences by one author, so a misreading is symmetric and invisible to our own tests. This is that
case, with the sign reversed: our reading is the more literal one, and it is the one that leaves the
feature inert against every peer in the field. A Server following the supervisor's convention cannot
switch us off; our Server, offering only the signals an operator kept, would silently switch a
supervisor's other signals off. Both directions are interoperability failures, and only one of the
two readings can be held by both ends.

## Decision

We will read an own-telemetry offer as **complete state for all three signals**, and treat an empty
`destination_endpoint` as an **explicit withdrawal** — the reference implementation's semantics,
adopted deliberately and recorded as a deviation from the Baseline's text.

1. **An offer that names at least one telemetry destination states all three.** When any of
   `own_metrics`, `own_traces`, `own_logs` is present, the offer is the whole truth about own
   telemetry: a signal it does not name is **stopped**, not carried over. The per-signal fold in
   `connection::merge` ends for this class.

2. **An offer that names none of the three changes nothing.** Silence about telemetry stays silence
   — an OpAMP-settings-only offer, a certificate rotation, a heartbeat change. This is the Baseline's
   own rule applied at the level it can still hold at, and it is what keeps the three classes of
   ADR-0086 independent.

3. **An empty `destination_endpoint` withdraws that signal.** It is admitted rather than refused: the
   exporter is shut down, the destination leaves the persisted state, and the offer is acknowledged
   `APPLIED`. It is not a malformed URL to report back, and it is the only way to say "all three
   off" — by rule 2, an offer that names nothing cannot say it.

4. **The Server can state a withdrawal.** An endpoint set to the empty string in `[telemetry_offer]`
   is a withdrawal to be sent, not a validation error, and such an offer counts as non-empty for the
   hash gate and for `OffersConnectionSettings` (ADR-0086 point 5). Removing the section altogether
   keeps its present meaning — *"I have nothing to say about telemetry"* — so that a Server which
   never offered telemetry does not tombstone a fleet another Server configured.

5. **The deviation is recorded, not hidden.** [`CONFORMANCE.md`](../CONFORMANCE.md) gains its first
   row under *Deviations*, naming the sentence departed from, the implementation followed, and the
   reason. That record is the whole of what this decision owes: the disagreement between the schema
   comment and the reference implementation is upstream's to settle, and **this project does not
   carry it there** — it takes no position in `opamp-spec` and opens nothing. What it owes its own
   readers is to say plainly which of the two it follows and why, which is what the row does.

## Alternatives considered

- **Keep the literal reading and document the gap.** The status quo, and defensible: the sentence is
  unambiguous and we would be the only ones obeying it. Rejected because obedience here has a
  concrete cost — no fleet-driven off switch, and silent mutual misconfiguration with the one peer
  that implements the same fields. A conformance claim that no other implementation can exercise is
  not evidence of interoperability; ADR-0040 was written to stop exactly this kind of self-agreement.

- **A local switch: drop the three capability bits from `supervisor.toml`.** Fully conformant — the
  specification permits an Agent to *"update any of its capabilities at any time after the first
  message"* and binds the Server to respect it — and it needs no interpretation of anything. Rejected
  as an answer to *this* question: it is a per-host edit, which is the very intervention the fleet is
  meant to replace. Worth having later for a different reason (an operator refusing telemetry the
  Server keeps offering), and nothing here forecloses it.

- **Per-signal completeness without the empty-endpoint withdrawal (rule 1 without rule 3).** Smaller,
  and it needs no illegal value on the wire. Rejected because it cannot express "all off": by rule 2
  an offer that names nothing means "unchanged", so the last remaining signal could never be
  switched off — the operator would be left with exactly one destination they cannot get rid of.

- **Have the Server send withdrawals automatically when `[telemetry_offer]` disappears.** Convenient,
  and it would spare the operator the empty string. Rejected because the Server cannot tell "this
  fleet was never offered telemetry" from "telemetry was withdrawn" — it holds no record of what it
  previously offered — so every Server without a `[telemetry_offer]` would tombstone every Agent it
  meets, including Agents another Server configured. Withdrawal is a thing an operator says, not a
  thing absence implies.

- **Take it upstream — raise it against `opamp-spec` and wait for an answer.** The clean order of
  operations, and it would spare every implementer this question. Rejected on two counts. The
  timing: `opamp-go` last released `v0.23.0` in February 2026 against spec `v0.16.0` (ADR-0040) and
  the protocol is still Beta, so waiting on that cadence means shipping a feature that cannot be
  turned off for the foreseeable future. And the scope: this project implements the protocol and
  records where it departs from it (goals 12 and 13) — it does not staff its evolution. Reading the
  disagreement correctly and writing down which side we take is the obligation; arguing it in
  someone else's repository is not one we take on.

- **Invent a cleaner signal of our own** — a `disabled = true` in the offer, or a custom capability
  in the style of Bindplane's `measurements.v1`. Rejected: `SPECIFICATION.md` lists inventing
  protocol semantics as a non-goal, and a bespoke signal would be understood by exactly one Server
  and one Client — ours. If we are going to depart from the text, departing *towards* the
  implementation everyone else runs is the only departure that buys interoperability.

## Sources / Prior art

- **The Baseline `v0.20.0`, `ConnectionSettingsOffers`** — `own_metrics` / `own_traces` /
  `own_logs` (*"If this field is not set then the Agent should assume that the settings are
  unchanged"*) and `hash` (*"Hash of all settings, including settings that may be omitted from this
  message because they are unchanged"*). The sentence this ADR departs from, quoted from
  `crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto`.
- **The Baseline, `TelemetryConnectionSettings.destination_endpoint`** — *"The value MUST be a full
  URL an OTLP/HTTP/Protobuf receiver with path"*, which is why an empty string is a deviation and not
  merely an unusual value; and `certificate`, the one field for which the schema does define
  withdrawal by omission.
- **`opampsupervisor` (`opentelemetry-collector-contrib`, `cmd/opampsupervisor/supervisor/supervisor.go`,
  `main` as of 2026-08-21)** — `updateOwnTelemetryData`, `setupOwnTelemetry`,
  `processOwnTelemetryConnSettingsMessage`, `loadLastReceivedOwnTelemetryConfig`, and the dispatch in
  `onMessage`. Read in full rather than summarised; the behaviour in the Context section is quoted
  from it.
- **`opamp-go`, `client/internal/receivedprocessor.go`** — the pass-through of `OwnMetrics`,
  `OwnTraces`, `OwnLogs` into `MessageData`, gated per capability, with no merge against prior state
  and no persistence. The library leaves the semantics to the Client, which is why the supervisor's
  reading is the ecosystem's reading.
- **Elastic `fleet-server`, `docs/opamp.md`** — *monitoring-only mode*, "No connection settings
  management": an implementation that answers the question by not implementing the mechanism.
- **Bindplane** — `com.bindplane.measurements.v1` / `reportMeasurements` custom messages and the
  throughput measurement processor carried in the pushed collector configuration: self-telemetry
  routed through the configuration channel, where removal is expressible.
- **[ADR-0040](0040-interoperability-against-opamp-go.md)** — `opamp-go` as the behavioural oracle,
  and the symmetric-misreading argument that makes it one. The tie-breaker this ADR invokes.
- **[ADR-0086](0086-a-telemetry-destination-is-an-offer-of-its-own-class.md)** — the class model this
  decision sits inside, and the ADR whose supersession of ADR-0036 point 7 left the fold rule
  unowned.

## Consequences

- **Positive: own telemetry can be switched off from the fleet.** Per signal, by dropping it from
  `[telemetry_offer]`; entirely, by stating an empty endpoint. No host is touched, no state file is
  deleted, and the Agent acknowledges the change like any other offer.

- **Positive: one reading, held by both ends and by the oracle.** Our Server can drive a
  supervisor-backed Agent without silently disabling signals it did not mention, and a
  supervisor-shaped Server can drive our Client. The interop harness of ADR-0040 gains a case it can
  actually assert, where before it could only assert our own reading back to us.

- **Positive: the Server's silence keeps its meaning.** Rule 2 leaves an OpAMP-only offer, a
  certificate rotation and a heartbeat change unable to disturb telemetry — the independence
  ADR-0086 established stays intact.

- **Negative: this is the project's first recorded deviation from the Baseline.** *Deviations* in
  `CONFORMANCE.md` currently reads *"none yet"*, and the line that "nothing implemented diverges" has
  been true so far. It stops being true here, and the document's honesty depends on that row being
  written in the same change.

- **Negative: we will emit a value the schema forbids.** An empty `destination_endpoint` fails the
  MUST that requires a full URL. A strict third-party Client may reject the message, and would be
  right to; what it costs is that the withdrawal does not reach that Client, not that anything else
  breaks.

- **Negative: a third-party Server written to the literal text can disable a signal by accident.**
  A Server that rotates the metrics headers by sending `own_metrics` alone will, under rule 1, stop
  our traces and logs. That is exactly what it would do to a supervisor today, so the failure mode is
  the ecosystem's rather than ours — but before this ADR our Client was immune to it, and after it we
  are not.

- **Negative: the meaning of an offer now depends on which fields it carries.** "Names none of the
  three" and "names one of the three" are different kinds of message, and the difference is
  load-bearing. It is one sentence to state and one predicate in the code, but it is a rule an
  operator can be surprised by, and stating it in `config/server.toml` beside the endpoints is part
  of the work.

- **Follow-ups:** what this ADR means if upstream settles the question the other way — a reversal
  would be a new ADR, and the deviation row is where the cost of that reversal is already written
  down. Watching for it belongs with the Baseline bump, which is the moment this project reads
  upstream's changes anyway. Separately, an Agent that has
  *stopped* reporting is as invisible in the fleet view as a failed rotation is — the same gap
  ADR-0086 flagged for `connection_settings_status`, now with a second reason to close it.
