# ADR-0023: An Agent reports its own telemetry over OTLP/HTTP, through the OpenTelemetry SDK, to destinations the Server offers as a class of their own

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

## Context

Three of the Baseline's capability bits are one feature: `ReportsOwnTraces` (`0x0020`),
`ReportsOwnMetrics` (`0x0040`), and `ReportsOwnLogs` (`0x0080`), all `[Beta]`. Taking them together
leaves exactly one bit undone — `AcceptsOtherConnectionSettings`, which cannot be honoured without
inventing semantics the protocol deliberately leaves to the Agent.

**The protocol says what to send and where.** Each capability means "the Agent can report own
\<signal\> to the destination specified by the Server via `ConnectionSettingsOffers.own_*`"
([`opamp.proto:744-756`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L744-L756)). The
destination is a `TelemetryConnectionSettings` whose `destination_endpoint` "MUST be a full URL an
OTLP/HTTP/Protobuf receiver with path", and the Agent "MAY refuse to send the telemetry if the URL
begins with `http://`"
([`opamp.proto:347-375`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L347-L375)). The
Baseline also asks that the `AgentDescription`'s identifying attributes appear in the OTLP Resource,
so the telemetry is attributable to the Agent that produced it, and names process metrics —
"CPU or RAM usage" — as what own metrics are for.

Three forces shape how this is built here.

**"Own" cannot mean the Managed Process's internals.** Upstream's `opampsupervisor` answers the
own-telemetry offer by configuring the Collector's *internal* telemetry to point at the destination.
That road is closed: [ADR-0011](0011-supervisor-mode-and-lifecycle-port.md) has the Collector
Supervisor pass each config-map entry as its own `--config` and do **no YAML manipulation**, and the
specification's non-goal forbids inventing an abstraction over a Managed Process's configuration
language. What this Client can honestly report is what it observes from outside: its own process, and
the processes it spawned and holds the pids of.

**The signals are not equally shaped, but all three have a subject.** Metrics are the process
metrics of the Client and of every Managed Process. Logs are what the Client already writes to
stderr through `tracing` — a fleet's client logs in one place is the single most useful thing an
operator cannot otherwise get. Traces are the least obvious, and the temptation is to skip them; but
the control loop is already a set of lifecycles with phases and outcomes (`APPLYING` →
`APPLIED`/`FAILED`, `Downloading` → `Installing` → `Installed`/`InstallFailed`, the self-update's
stage-prove-switch), and those are spans, not an instrumentation project.

**OTLP has a reference implementation, and it is the standard's own.** The temptation here is to
encode OTLP by hand: [ADR-0006](0006-proto-vendoring-and-codegen.md) already vendors the Baseline's
schema and compiles it with prost via protox, so vendoring a second schema and POSTing the bytes
with the `reqwest` the Client already carries would need no new crate at all. But OTLP is not this
project's protocol to own. Its wire format, its semantic conventions, and their versioning are
maintained upstream, and `opentelemetry-otlp` is where that maintenance lands — it depends on the
`opentelemetry-proto` crate generated from the very schema the vendoring would copy. A copy in this
repository would be a second protocol to keep in sync, with no Baseline discipline behind it and no
authority to resolve a disagreement.

### A telemetry destination is not an OpAMP endpoint

[ADR-0014](0014-server-driven-connection-settings.md)'s flow for connection settings is: acknowledge
`APPLYING`, **verify by actually connecting**, persist, **reconnect**, acknowledge `APPLIED`.

**That flow cannot be run for a telemetry destination.** The thing to be proved is an OTLP receiver,
not the OpAMP endpoint; connecting to it proves nothing about the OpAMP connection, and there is no
connection to re-establish afterwards.

**Gating the offer on its `opamp` field breaks the feature silently.** A Server with a
`[telemetry_offer]` and no `[connection_offer]` composes an offer whose `opamp` field is absent. A
Client that acts on an offer only if `opamp` is set ignores it entirely — no `APPLYING`, no
`connection_settings_status`, no exporters. The Server's hash gate compares a reported
`last_connection_settings_hash` that never arrives, so it re-offers on **every** exchange, forever.
Nothing logs an error on either side: each end is doing what it was built to do.

**The Baseline has three classes of offer, each with its own sequence.** *Connection Settings
Management* opens by naming *"3 classes of destinations"* — the OpAMP Server, own telemetry, and
"other" — and states plainly: *"Depending on which connection settings are offered **the sequence of
operations is slightly different**."* It then routes each class to its own section. Three details
follow from that structure and none of them is ambiguous:

- The verification MUST is scoped to **one field**. It appears under
  `ConnectionSettingsOffers.opamp` — *"The Client MUST verify the offered connection settings by
  actually connecting before accepting the setting to ensure it does not lose access to the OpAMP
  Server due to invalid settings"* — and the justification is the scope: losing access to the
  *OpAMP Server*. No such requirement appears under `own_metrics`, `own_traces`, or `own_logs`.
- The telemetry sequence in *Own Telemetry Reporting* is: the offer arrives, the Agent exports. No
  verification step, no reconnect. What it does ask for is the acknowledgement: *"If the Agent has
  the ReportsConnectionSettingsStatus capability it SHOULD set the connection_settings_status
  accordingly when new settings are received."*
- The Baseline **expects the standalone offer**: *"The Server SHOULD populate the connection_settings
  field when it sends the first ServerToAgent message to the particular Agent (normally in response
  to the first status report from the Client), unless there is no OTLP backend that can be used."*
  A Server holding only a telemetry destination is asked to send it in its first reply, whether or
  not it has anything to say about the OpAMP connection.

What has to be settled is whether this project has **one** class of connection-settings offer with
one lifecycle, or the Baseline's three with sequences of their own.

### Where cleartext may go

The OTLP Resource carries the Agent's identifying attributes and the log records carry whatever the
Client logs, so the Baseline's *"MAY refuse to send the telemetry if the URL begins with `http://`"*
is taken — one step firmer than the credential warning of
[ADR-0013](0013-opamp-endpoint-admission.md), because this is a continuous stream rather than a
single request. The open question is where the line falls.

**Loopback is the only line with a definition that needs no assumption.** It is not a judgement about
networks — it is the statement *nothing leaves the machine*, which needs no assumption about the
deployment to be true. [`.devcontainer/`](../../.devcontainer/) publishes the Collector's OTLP/HTTP
port to the host precisely so `http://localhost:4318` means the same thing inside the workspace
container as outside it, and `config/server.toml`'s first worked example says so in as many words.

**Loopback alone makes the ordinary small-fleet shape impossible.** One Collector on a host of its
own, Agents on the same LAN, one hop between them: the configuration a fleet reaches the moment it
stops being one machine, and long before it becomes something with an ingress and a certificate
lifecycle. Under a loopback-only rule an operator in that position has exactly two options — put a
Collector on every host, or terminate TLS in front of the one they have. The second means a
certificate, a name to put it on, and a renewal, all so that a stream can cross a network segment the
operator already owns and where nothing on the path is anyone else's.

**The risk has an address range attached to it.** *"Someone between the Agent and the receiver can
read the identifying attributes and the logs"* is a statement about who is on the wire. Loopback
answers it with *nobody*; the public internet answers it with *anyone*. The private address space —
[RFC 1918](https://www.rfc-editor.org/rfc/rfc1918)'s `10/8`, `172.16/12` and `192.168/16`, and
[RFC 4193](https://www.rfc-editor.org/rfc/rfc4193)'s `fc00::/7` — answers it with *whoever the
operator has put on their own network*, and those ranges are not routable across the public internet,
so the answer does not quietly change when a route does. That is a weaker guarantee than loopback's
and a categorically stronger one than a public address's, and it is the same line this project
already draws elsewhere: `crates/server/src/api.rs` deliberately does *not* block the RFC 1918 ranges
when it validates a listener.

**String comparison cannot answer range membership.** A URL brackets an IPv6 literal, so
`http://[::1]:4318/v1/logs` split on `:` yields `[`, and a check that compares the host against
literals refuses the very loopback address it means to admit. The predicate has to parse an address
rather than compare strings, which is what makes range membership answerable at all.

### What an offer says about telemetry

**Read per signal, own telemetry can be switched on from the fleet, moved from the fleet, and never
switched off.** A destination that is once in force stays in force: it survives reconnects by design
(clause 15) and restarts by design (the persisted settings are put back before the first exchange).
A per-signal fold takes an offered destination or, failing that, the stored one — so an offer that
omits `own_traces` leaves the traces exporter running, and an offer that names an *empty* endpoint is
discarded in favour of the stored one. And with `[telemetry_offer]` removed and no
`[connection_offer]`, `fleet.rs::settings_offer` returns `None`: there is no message at all. What
remains is deleting `connection-settings.pb` on the host and restarting the Client — an intervention
per machine, which is the class of work this project exists to remove.

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

**So the literal reading is conformant and the feature is unusable.** That combination is what makes
this a decision rather than a bug report.

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

**This project has already named the tie-breaker.** [ADR-0004](0004-protocol-baseline-and-conformance.md)
made `opamp-go` the *behavioural oracle* precisely because both ends here were written from the same
sentences by one author, so a misreading is symmetric and invisible to our own tests. This is that
case, with the sign reversed: the literal reading is the more literal one, and it is the one that
leaves the feature inert against every peer in the field. A Server following the supervisor's
convention could not switch a literal-reading Client off; a literal-reading Server, offering only the
signals an operator kept, would silently switch a supervisor's other signals off. Both directions are
interoperability failures, and only one of the two readings can be held by both ends.

### Where spans come from

**An exporter alone produces no traces.** `telemetry.rs` builds the `SpanExporter`, registers the
tracer provider globally, and shuts it down on withdrawal, and the development stack provisions a
dashboard for the result. A trace exists only if code creates a span, and a span reaches the
exporter only through a producer.

**Nothing that is already linked would make a span.** `opentelemetry-appender-tracing` — the bridge
clause 4 chose — converts `tracing` **events** into OTLP log records and states the boundary itself:
*"This crate does not convert `tracing` spans into OpenTelemetry spans. Use `tracing-opentelemetry`
for that."* So an `#[instrument]` without a producer reaches stderr and the log file of
[ADR-0026](0026-the-client-logs-to-a-file-in-service-mode.md), and reaches the OTLP destination
never. The gap is a missing producer, not missing instrumentation, and instrumenting before deciding
the producer would produce nothing.

**The one-store decision rests on a join that needs a left side.** `.devcontainer/OBSERVABILITY.md`
justifies collapsing Tempo, Loki and Prometheus into ClickHouse with cross-signal questions —
*"which log lines belong to the operation that failed"* is a join between `otel_logs` and
`otel_traces` on `TraceId`. A log record takes its trace context from the span in force, so without a
span in force every exported log record carries a zero `TraceId`, and the store is shaped for a
correlation the Client cannot supply.

**Two mechanisms exist in Rust.**

- The OpenTelemetry trace API directly: `global::tracer(…)`, a `Context` threaded through the code
  that is being measured.
- `tracing-opentelemetry`: a `tracing-subscriber` layer that turns the `tracing` spans a program
  already writes into OpenTelemetry spans and hands them to the SDK's provider. Version `0.33.0`
  (2026-05-18) is the one built against `opentelemetry`/`opentelemetry_sdk` `0.32`, which is what
  this workspace runs; the crate is deliberately numbered one release ahead of the OTel crates it
  binds to.

**The two crates now compose without a workaround.** `opentelemetry-appender-tracing` takes a log
record's trace context from the active OpenTelemetry `Context`, which a `tracing` span did not
populate — the reason the appender carried an `experimental_use_tracing_span_context` feature, and
the reason its interaction with `tracing-opentelemetry` produced a run of bug reports
(`opentelemetry-rust` [#1378](https://github.com/open-telemetry/opentelemetry-rust/issues/1378),
[#2803](https://github.com/open-telemetry/opentelemetry-rust/issues/2803),
[#2824](https://github.com/open-telemetry/opentelemetry-rust/issues/2824)). That feature is **gone**
as of the `0.32` the workspace depends on, and its changelog says why:

> Remove the `experimental_use_tracing_span_context` since `tracing-opentelemetry` now supports
> activating the OpenTelemetry context for the current tracing span. This fixes
> [#3190](https://github.com/open-telemetry/opentelemetry-rust/issues/3190) — the circular dependency
> introduced by depending on `tracing-opentelemetry` that depends on `opentelemetry`.

`OpenTelemetryLayer::with_context_activation` is the switch that does it, and it is *on by default*:
entering a `tracing` span attaches its OpenTelemetry context, so the appender finds it. The two
crates compose without a feature flag, an experiment, or a workaround — which is what makes this a
small decision against these versions.

**The reference implementation had the identical gap and closed it.** `opampsupervisor` shipped the
`reports_own_traces` capability and its exporter before it emitted anything;
[contrib#38724](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/38724)
("Emit spans via trace exporter") asked for spans around *"the start of the supervisor"*, *"handling
messages from the agent"*, *"handling messages from the server"*, and *"applying a new config and
restarting the collector"*, and was closed by
[contrib#38797](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38797) with
spans named `GetBootstrapInfo`, `onMessage`, `handleAgentOpAMPMessage` and
`processRemoteConfigMessage`, whose outcomes are recorded with `span.SetStatus`. Two of those four
are message-handling spans, and that is the one part of it this decision does not follow — see
clause 32.

## Decision

We will implement **all three own-telemetry capabilities** through the **OpenTelemetry Rust SDK**,
exporting **OTLP/HTTP with protobuf bodies** to the destinations the Server offers, and we will
**invent nothing OTLP does not already define** — no second vendored schema, no metric names of our
own. A telemetry destination is **an offer of its own class**: applied and acknowledged without a
connection to prove it and without restarting the OpAMP connection, so the Baseline's three classes
become three classes here. Cleartext is admitted **inside the private address space** and refused
everywhere else. An own-telemetry offer is read as **complete state for all three signals**, with an
empty `destination_endpoint` as an **explicit withdrawal** — the reference implementation's
semantics, adopted deliberately and recorded as a deviation from the Baseline's text. Own traces
come from the **`tracing` spans this Client already writes**, through `tracing-opentelemetry`, and a
span is a **fleet operation with a lifecycle and an outcome** — not a unit of message handling.

### The SDK and the signals

1. **The standard's own implementation, not a copy of its schema.** `opentelemetry`,
   `opentelemetry_sdk`, and `opentelemetry-otlp` carry the wire format; the exporter is configured
   for OTLP over HTTP with protobuf bodies and an async reqwest client, which is what the Baseline's
   `destination_endpoint` requires:

   ```toml
   opentelemetry-otlp = { version = "0.31", default-features = false,
                          features = ["http-proto", "reqwest-client", "trace", "metrics", "logs"] }
   ```

   Features are stated rather than inherited: the defaults carry `reqwest-blocking-client`, and this
   Client is a tokio process. `grpc-tonic` is left off — the schema requires HTTP, so a gRPC stack
   would be weight for a transport this protocol does not permit here.

   **ADR-0006's vendoring is not extended to a second protocol.** That decision exists so this
   project owns the *OpAMP* wire contract and can diff it against upstream; OTLP is not ours to own,
   and a hand-encoded copy would carry the maintenance of someone else's standard with none of the
   Baseline machinery that makes the first copy safe.

2. **Names come from the standard too.** Metric and attribute names are taken from
   `opentelemetry-semantic-conventions` rather than written as string literals, so what this Client
   emits is what a receiver already knows how to chart — and a convention that moves is a version
   bump rather than a silent divergence.

3. **`sysinfo` for the numbers themselves.** CPU and resident memory for a pid, on Linux, macOS, and
   Windows, is three platform APIs (`/proc`, `task_info`, `GetProcessMemoryInfo`) and is exactly the
   kind of thing not to hand-roll three times. Pure Rust over `libc`; no C toolchain, no cmake. The
   SDK has no process instrumentation for Rust, so this is the one gap it leaves.

4. **Logs bridge through `opentelemetry-appender-tracing`.** The Client already logs through
   `tracing`; `OpenTelemetryTracingBridge` is the standard layer that turns those events into OTLP
   log records, registered beside the existing `fmt` layer. Stderr keeps everything it prints today.

5. **What each signal carries.**

   | Signal | What the Client sends |
   |---|---|
   | Metrics | Process metrics per Agent: `process.cpu.time`, `process.memory.usage`, `process.uptime`, following the semantic conventions the Baseline points at. The Client's own Agent reports the Client's process; each Supervisor-backed Agent reports its Managed Process — which this Client already owns the pid of, so nothing new has to be discovered. |
   | Logs | The Client's own `tracing` output, bridged to OTLP log records at the level the log filter already selects. Stderr keeps everything it prints today; this adds a destination, it does not move one. |
   | Traces | One span per control-loop operation that already has a lifecycle: applying a remote configuration, installing a package, a self-update. Phases become child spans and the existing outcome becomes the span status, so a failed rollout is one trace rather than a log hunt. Clauses 28–36 say how a span comes into existence and which operations become one. |

6. **Every Agent's telemetry is attributed to that Agent.** The OTLP Resource carries the
   `AgentDescription`'s identifying attributes — `service.name`, `service.instance.id`, and
   `service.namespace` where set — which is what the Baseline asks for and what makes a Supervisor's
   metrics distinguishable from the Client's own on the same host.

### Destinations

7. **Destinations come only from the Server.** `own_metrics`, `own_traces`, and `own_logs` arrive
   in `ConnectionSettingsOffers` and are persisted with the connection settings; clauses 12–17 say
   how such an offer is applied, and clauses 23–27 what it states. There is no destination in
   `supervisor.toml` (the Client's configuration file, ADR-0022 clause 10) — the whole point of the
   capability is that the Server names it. With no destination offered, the Client sends nothing and
   costs nothing.

8. **`https://`, or cleartext inside the private address space, nothing else.** The Baseline's "MAY
   refuse" is taken: a destination on plain `http://` outside the admitted set is refused and
   reported, because the Resource carries identifying attributes and the records carry whatever the
   Client logs. This mirrors the warning ADR-0013 already emits for credentials in cleartext, one
   step firmer. The admitted set is loopback plus the private ranges: `127.0.0.0/8` and `::1`;
   RFC 1918's `10.0.0.0/8`, `172.16.0.0/12` and `192.168.0.0/16`; RFC 4193's unique-local
   `fc00::/7`. An `https://` destination is unaffected — this rule governs cleartext only. Clauses
   18–22 say how membership is decided.

9. **The three capabilities are declared unconditionally.** Unlike `OffersPackages` or
   `[client_ca]`, the capability here states an *ability* the Client always has — "I can report to a
   destination you name" — and the Server's offer is what arms it. Declaring it conditionally on a
   destination already being in force would mean the Server could never make the first offer.

10. **`certificate`, `tls`, and `proxy` behave exactly as they do on the OpAMP settings.**
    `TelemetryConnectionSettings` carries the same three fields; the certificate machinery of
    ADR-0013 is reused as-is, and `tls`/`proxy` are refused and *reported* rather than dropped in
    silence.

11. **The Server has a `[telemetry_offer]` section** — `metrics_endpoint`, `traces_endpoint`,
    `logs_endpoint`, and optional headers per signal — compiled into the same hash-gated
    `ConnectionSettingsOffers` that `[connection_offer]` produces. Without it the Server offers no
    destination. **The Server declares `OffersConnectionSettings` whenever it can offer anything** —
    a `[connection_offer]`, a `[telemetry_offer]`, or a `[client_ca]` whose issued certificate
    travels as an ordinary offer (ADR-0013). Declaring a capability the Server exercises is the
    Baseline's rule, and it is what makes the comment on `capabilities()` — *"an undeclared
    capability is never exercised, a declared one never hollow"* — true.

### A telemetry destination is an offer of its own class

12. **An offer is actionable when it carries anything this Client can put in force** — OpAMP
    settings, or any of `own_metrics`, `own_traces`, `own_logs`. Not `opamp` alone. An offer
    carrying only `other_connections` is **not** actionable while `AcceptsOtherConnectionSettings`
    is undeclared: acknowledging what cannot be applied is the lie this whole path exists to
    prevent, and a conforming Server does not send it.

13. **Verification proves what it is able to prove.** The `opamp` half is verified by actually
    connecting, exactly as ADR-0014 requires and for exactly ADR-0014's reason — not losing access
    to the Server; ADR-0014's verify-by-connecting rule thus governs the class it was written for and
    is not weakened there. A telemetry destination is not verified by connecting: reachability of an
    OTLP receiver is not this Client's to establish at offer time, and a receiver that is down is not
    an offer that is wrong. What *is* checked before it is put in force is what this Client can
    decide on its own — the cleartext refusal (clauses 8 and 18–21) and the unhonoured fields
    (clause 10). Those checks are the telemetry class's admission test, and a failure is reported,
    not swallowed.

14. **One offer, one hash, one acknowledgement.** The Baseline hashes the whole message —
    *"Hash of all settings"* — so the Agent acknowledges the message, never one part of it. A single
    `connection_settings_status` is reported per offer, and its `error_message` names **everything**
    dropped or refused across both halves. If the `opamp` half fails verification, nothing from that
    offer is persisted or applied — the telemetry half included — and the offer reports `FAILED`.
    Half-applying an offer whose other half was rejected would leave the Server unable to tell what
    is running, which is what the single hash exists to prevent.

15. **A telemetry-only offer does not restart the connection.** It is applied in place, the
    acknowledgement rides the reports already owed, and the transport loop carries on. Only a
    verified `opamp` half causes the reconnect ADR-0014 describes.

16. **What is persisted says only what was offered.** The stored `connection-settings.pb` carries an
    `opamp` block only when the offer or the state it folds into had one. A file that claims the
    Server offered OpAMP settings it never offered is a lie in the one artefact an operator is told
    to inspect and delete.

17. **`other_connections` joins this class when it lands.** It is the Baseline's third class and it
    names destinations that are not the OpAMP endpoint, so the same reasoning applies: no
    verification by connecting, no reconnect, one acknowledgement. Implementing
    `AcceptsOtherConnectionSettings` therefore does not reopen this question — which is the point of
    deciding it here rather than case by case.

### Cleartext: by address, never by name

18. **The judgement is made on an address, never on a name.** `localhost` stays admitted, because it
    *is* loopback by definition ([RFC 6761](https://www.rfc-editor.org/rfc/rfc6761)) rather than by
    resolution. No other name is resolved to decide this. An admission test whose answer a
    re-resolve can flip is not one an operator can reason about, and DNS is the part of the path an
    attacker would move; a Collector reached by name over cleartext is therefore refused, and the
    answer is to name it by address or to put TLS in front of it.

19. **Membership is decided by parsing, not by prefix.** `192.168.0.1.example.com` is a host name
    that begins with a private address and is a public destination; `172.32.0.5` is one character
    from `172.16.0.0/12` and outside it. Both are refused because the host is parsed as an IP address
    first and tested for range membership second. This is also what admits the bracketed IPv6
    loopback literal `http://[::1]:4318/v1/logs`.

20. **Ranges that are private but not the operator's are not admitted.** Link-local
    (`169.254.0.0/16`, `fe80::/10`) is autoconfiguration rather than a network anyone deployed, and
    on a cloud host `169.254.169.254` is the instance metadata service — not a place to stream logs
    at. Carrier-grade NAT (`100.64.0.0/10`) is shared with other subscribers of the same provider,
    which is the *someone else's wire* this refusal exists for. Neither is admitted, and neither is a
    judgement call left to the code: they are named here so that a later reading of "private" does
    not quietly widen.

21. **The refusal keeps its voice.** A destination outside the admitted set is refused and reported
    back with the reason, exactly as clause 8 has it — not warned about, not downgraded, not dropped
    in silence. The message names what would be accepted, because *"use https://"* is unhelpful
    advice to an operator whose Collector is one hop away.

22. **This is the Agent's rule and it stays there.** The Server validates that a `[telemetry_offer]`
    endpoint is a full OTLP/HTTP URL with a path (or the empty string of clause 26) and nothing
    further; whether a destination is reachable in cleartext is decided where the packets originate.
    A Server may therefore offer a private address to a fleet, and each Agent answers for itself.

### An own-telemetry offer states all three destinations

23. **An offer that names at least one telemetry destination states all three.** When any of
    `own_metrics`, `own_traces`, `own_logs` is present, the offer is the whole truth about own
    telemetry: a signal it does not name is **stopped**, not carried over. There is no per-signal
    fold in `connection::merge` for this class.

24. **An offer that names none of the three changes nothing.** Silence about telemetry stays silence
    — an OpAMP-settings-only offer, a certificate rotation, a heartbeat change. This is the
    Baseline's own rule applied at the level it can still hold at, and it is what keeps the three
    classes of clauses 12–17 independent.

25. **An empty `destination_endpoint` withdraws that signal.** It is admitted rather than refused:
    the exporter is shut down, the destination leaves the persisted state, and the offer is
    acknowledged `APPLIED`. It is not a malformed URL to report back, and it is the only way to say
    "all three off" — by clause 24, an offer that names nothing cannot say it.

26. **The Server can state a withdrawal.** An endpoint set to the empty string in `[telemetry_offer]`
    is a withdrawal to be sent, not a validation error, and such an offer counts as non-empty for the
    hash gate and for `OffersConnectionSettings` (clause 11). Removing the section altogether means
    *"I have nothing to say about telemetry"*, so that a Server which never offered telemetry does
    not tombstone a fleet another Server configured.

27. **The deviation is recorded, not hidden.** [`CONFORMANCE.md`](../CONFORMANCE.md) carries a row
    under *Deviations*, naming the sentence departed from, the implementation followed, and the
    reason. That record is the whole of what this decision owes: the disagreement between the schema
    comment and the reference implementation is upstream's to settle, and **this project does not
    carry it there** — it takes no position in `opamp-spec` and opens nothing. What it owes its own
    readers is to say plainly which of the two it follows and why, which is what the row does.

### Own traces

28. **`tracing-opentelemetry` is the producer.** It joins the three OTel crates of clause 1, at the
    version matched to them (`0.33` against `opentelemetry` `0.32`), and it is the only dependency
    this part of the decision adds. The reasoning of clause 1 carries over unchanged: the standard's
    own implementation, not a second copy of its schema.

29. **The layer lives in a reload slot, exactly as the log bridge does.** `tracing` takes one
    subscriber per process and `main` installs it before any destination is known, so the span
    layer is held open from the start and filled when an offer puts a tracer provider in force — and
    emptied when the destination is withdrawn (clauses 23 and 25). This is the mechanism
    `telemetry.rs` runs for logs, for the same reason, and it keeps the cost of an un-offered
    destination at what a `tracing` span costs when no layer is interested in it.

30. **Instrumentation is written in `tracing`, never in OpenTelemetry.** `#[instrument]` and
    `tracing::span!` in the modules being measured; no OTel type outside `telemetry.rs`. An outcome
    becomes a span status through the crate's reserved fields — `otel.status_code` and
    `otel.status_description` — so no status vocabulary of this project's own is invented, in
    keeping with clause 2.

31. **These five operations are spans, and their phases are child spans.** Each already has a
    beginning, an end, named phases in between, and an outcome this Client reports to the Server:

    | Root span | Phases | Where it starts |
    |---|---|---|
    | `package.install` | `download`, `verify`, `stage`, `preflight`, `swap`, `gate`, `rollback` | `transport::process_package_downloads` |
    | `config.apply` (Supervisor set) | `validate`, `stop`, `write`, `purge`, `start` | `reconfigure::apply` |
    | `config.apply` (Managed Process) | `reload` or `restart`, `gate` | `engine::handle`, where the configuration is handed over |
    | `connection.settings.apply` | `verify`, `store` | `transport::process_connection_offer` |
    | `self.update` | `stage`, `probe`, `commit` or `roll_back` | `selfupdate::install` |

    Two of them span **two tasks**: an install and a Managed Process's configuration apply are begun
    by whoever received the message and finished by the Supervisor's own task. The span travels with
    the command through the Port, which is why `ProcessCommand::ApplyConfig` and `ApplyPackage` each
    carry one. A trace that ended at the hand-over would stop exactly where the interesting failures
    are.

    The names are this project's vocabulary, not a semantic convention. OpenTelemetry defines none
    for an agent's own lifecycle, and clause 2 binds names to the standard *where the standard has
    one* — which is why the metric names are semconv's and these are not. Stated here so that nobody
    later "corrects" them towards a convention that does not cover this.

    Two of the five are narrower than the table reads, and the implementation says so rather than
    forcing the table:

    - **`self.update` is a root only in principle.** A self-update is always reached from a package
      offer, so in practice it is a child of the `package.install` that downloaded the artifact.
      That is the better shape — one trace answers *"did this Client update itself"* from the
      download to the commit — and nothing about it is a separate decision, so it is recorded here
      rather than given a clause of its own.
    - **`connection.settings.apply` has two phases, not three.** `verify` and `store` are spans;
      the reconnection is a **field** on the root span, because it happens after the operation
      returns, in the transport loop that owns the connection. A span for it would measure nothing.

32. **What is deliberately not a span:** the transport exchange (a poll cycle, a WebSocket receive),
    the metrics sampler tick, and the Supervisor Endpoint's message handling. They have no outcome to
    carry and no end, and at one exchange per Agent per interval — the Baseline's default is 30 s —
    they would be an unbounded stream that buries five real operations in the same dashboards, whose
    *"operation rate"* and *"which phase fails most"* panels are computed over all spans. This is a
    deliberate divergence from `opampsupervisor`, which spans `onMessage` and
    `handleAgentOpAMPMessage`: that supervisor is one process managing one Collector, where message
    handling *is* the work; a Client here multiplexes n Agents over one connection
    ([ADR-0003](0003-client-modes-and-connection-multiplexing.md)) and runs on every host of a fleet.
    A failed exchange stays a logged warning, carrying the trace context of whatever operation was in
    flight.

33. **The self-update trace crosses the restart.** The trace id and the root span id go into the
    `UpdateMarker` that ADR-0017 writes, and the process that comes up after the restart continues
    that trace: `commit` or `roll_back` is a child of the span that staged the version. Without this
    the trace ends one line before the part that fails, which is the part the trace exists for. A
    marker that carries no ids — one written by an older Client — makes the post-restart span open
    its own trace rather than fail.

34. **No sampler.** Always-on, because the volume is fleet operations and not requests — a handful
    per host per day. A sampler here would be volume management for a volume that does not exist,
    and the first thing it would drop is the rare failed rollout the trace was built for.

35. **The logs gain their trace context, and that is part of this decision.** With clause 29 in
    force, every log record the appender exports from inside an instrumented operation carries that
    operation's `TraceId` and `SpanId`. The join `.devcontainer/OBSERVABILITY.md` promises becomes
    answerable, and the dashboard's trace-detail view can reach the lines that explain a failure.

36. **A span attribute is data leaving the host, and is named one by one.** The same discipline
    `telemetry.rs` applies to the Resource's descriptive attributes applies here: attributes are
    chosen individually — a Supervisor name, a package name and version, a configuration hash, a
    count — and never a whole configuration, a URL with credentials in it, or an offer's headers.
    The `Debug` impl that hides package header values exists for this reason and must not be
    defeated by a span field.

## Alternatives considered

- **Vendor `opentelemetry-proto` and encode OTLP by hand**, exactly as ADR-0006 vendors the
  Baseline's schema. It is the cheaper build — prost, protox, and the `reqwest` this Client already
  carries, with no new crate at all. Rejected: ADR-0006 exists so this project owns the *OpAMP*
  contract and can prove it matches upstream, and none of that machinery would come along. A second
  copied schema is a second protocol to keep in sync, its semantic conventions would be string
  literals nobody re-checks, and the correctness of someone else's standard would become this
  project's to defend. Owning the bytes is right where the bytes are the product; here they are the
  exhaust.
- **Take the SDK but keep its default features.** Fewer lines in the manifest. Rejected: the
  defaults select `reqwest-blocking-client`, and blocking HTTP inside a tokio runtime is how an
  executor gets starved.
- **Metrics and logs only, traces deferred** — or, equivalently, stop declaring `ReportsOwnTraces`.
  Tempting, because an Agent has no request path and a span looks like a stretch, and cheaper than
  instrumenting. Rejected: the operations that fail in a fleet — a configuration that will not apply,
  a package that will not stay up — are exactly the ones this project already models as multi-phase
  lifecycles with outcomes, so the spans are a mapping of state that exists rather than new
  instrumentation. A withdrawal would also have to be real: the capability bit, the offer path, the
  dashboard and the Server's `traces_endpoint` would all have to go, which is more work than
  instrumenting five operations, and it removes the one signal that answers *why* a rollout failed
  rather than *that* it did.
- **Configure the Collector's internal telemetry from the Supervisor**, as `opampsupervisor` does.
  It is the richer answer for a Collector, and it is what an operator coming from upstream will
  expect. Rejected: ADR-0011 forbids this Client from touching a Managed Process's configuration,
  and the specification's non-goal forbids inventing an abstraction over it. What the Supervisor can
  report about a process it did not configure is what it can observe, and saying only that is
  honest.
- **A destination in `supervisor.toml`.** Would let own telemetry work against a Server that offers
  none. Rejected: the capability is defined as reporting "to the destination specified by the
  Server", and a locally configured destination is a private extension of the protocol wearing the
  capability's name.
- **Send OTLP over gRPC.** Rejected by the schema: `destination_endpoint` "MUST be a full URL an
  OTLP/HTTP/Protobuf receiver with path".
- **Declare only the capabilities a destination is currently offered for.** Consistent with how
  `OffersPackages` and `AcceptsConnectionSettingsRequest` are declared. Rejected in clause 9: those
  describe something the end *has*, this describes something it *can do*, and the conditional
  version deadlocks the first offer.
- **Always carry an `opamp` block in the offer, so one lifecycle serves every offer.** The smaller
  diff, and it needs no new class. Rejected on three counts. It makes the Server synthesise a block
  it has nothing to put in, purely so the Client's guard passes. Each such offer then costs a
  verify-and-reconnect of the whole fleet for a change that never touched the OpAMP connection — a
  telemetry endpoint move would disconnect every Agent. And it contradicts the Baseline twice over:
  the *"3 classes"* structure, and the SHOULD that a Server send telemetry settings in its first
  reply *"unless there is no OTLP backend"* — with no mention of needing OpAMP settings to carry them.
- **Require `[connection_offer]` alongside `[telemetry_offer]` and document the gap in
  `CONFORMANCE.md`.** Honest, and it is what the Deviations table exists for. Rejected because the
  failure is silent on both ends and the workaround is a coupling with no reason behind it — an
  operator would have to configure credential rotation in order to get metrics. `CONFORMANCE.md`
  records deliberate departures; this would be recording an accident.
- **Give each class its own hash and acknowledge them separately.** Tempting: it would let the
  telemetry half apply when the OpAMP half fails. Rejected — the Baseline defines `hash` as *"Hash of
  all settings"* on the offer as a whole, and there is one `connection_settings_status` field to
  answer with. Splitting it would be this project inventing protocol semantics, which
  `SPECIFICATION.md` lists as a non-goal.
- **Verify a telemetry destination by connecting to it too, for symmetry.** Rejected on merit rather
  than on cost. An OTLP receiver that is momentarily down would turn a correct offer into a `FAILED`
  one, and the Server would re-offer settings that were never wrong. The OpAMP rule exists because a
  bad endpoint takes the host out of reach; a bad telemetry endpoint costs telemetry, which is what
  the refusal report is for.
- **`https://` or loopback, nothing else.** Needs no assumption about the deployment. Rejected
  because the cost lands on the deployment this project is actually for: a fleet of some tens of
  hosts with one Collector among them, told to obtain and renew a certificate for a stream that never
  leaves its own network. A rule at that price is one operators work around — by putting the
  Collector on every host, or by not reporting own telemetry at all — and neither workaround is
  better for the thing the refusal protects.
- **Admit `192.168.0.0/16` alone.** Rejected: `10.0.0.0/8` and `172.16.0.0/12` are the same class of
  network under the same RFC, and which of the three a site uses is an addressing-plan accident. A
  rule that admits one and refuses the others is one an operator cannot predict, and the first
  person to hit it would read it as a bug rather than a decision.
- **Make the admitted set configurable on the Client** — a list of trusted networks in
  `supervisor.toml`. Rejected on clause 7: nothing about own telemetry is configured on the Client,
  because the whole content of the capability is *"report to the destination you name"*. Splitting
  the decision across a Server-named destination and a per-host allowlist would give a fleet two
  places to look when telemetry does not arrive, and the second one is on the host nobody is logged
  into.
- **Resolve host names and admit those that resolve into the private space.** The friendly option:
  an operator's `collector.lan` would just work. Rejected because it makes the admission test depend
  on what DNS answered at the instant the offer arrived. The offer is persisted and re-applied across
  restarts, the name can be re-pointed afterwards without any offer changing, and the check that
  said yes would never be consulted again. A test that cannot be re-run to the same answer is not a
  security boundary.
- **Drop the refusal and emit a warning instead**, as ADR-0013 does for credentials. Rejected: a
  warning is proportionate to a credential on one request and not to a continuous stream of
  identifying attributes and log records. Moving the line is not the same as removing it.
- **Keep the literal per-signal reading of an offer and document the gap.** Defensible: the sentence
  is unambiguous and we would be the only ones obeying it. Rejected because obedience here has a
  concrete cost — no fleet-driven off switch, and silent mutual misconfiguration with the one peer
  that implements the same fields. A conformance claim that no other implementation can exercise is
  not evidence of interoperability; ADR-0004 was written to stop exactly this kind of self-agreement.
- **A local switch: drop the three capability bits from `supervisor.toml`.** Fully conformant — the
  specification permits an Agent to *"update any of its capabilities at any time after the first
  message"* and binds the Server to respect it — and it needs no interpretation of anything. Rejected
  as an answer to *this* question: it is a per-host edit, which is the very intervention the fleet is
  meant to replace. Worth having later for a different reason (an operator refusing telemetry the
  Server keeps offering), and nothing here forecloses it.
- **Per-signal completeness without the empty-endpoint withdrawal (clause 23 without clause 25).**
  Smaller, and it needs no illegal value on the wire. Rejected because it cannot express "all off":
  by clause 24 an offer that names nothing means "unchanged", so the last remaining signal could
  never be switched off — the operator would be left with exactly one destination they cannot get
  rid of.
- **Have the Server send withdrawals automatically when `[telemetry_offer]` disappears.** Convenient,
  and it would spare the operator the empty string. Rejected because the Server cannot tell "this
  fleet was never offered telemetry" from "telemetry was withdrawn" — it holds no record of what it
  previously offered — so every Server without a `[telemetry_offer]` would tombstone every Agent it
  meets, including Agents another Server configured. Withdrawal is a thing an operator says, not a
  thing absence implies.
- **Take it upstream — raise it against `opamp-spec` and wait for an answer.** The clean order of
  operations, and it would spare every implementer this question. Rejected on two counts. The
  timing: `opamp-go` last released `v0.23.0` in February 2026 against spec `v0.16.0` (ADR-0004) and
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
- **Use the OpenTelemetry trace API directly and add no dependency.** Attractive on the dependency
  count, and it is what the reference implementation does (in Go, where the context is threaded
  through every call anyway). Rejected: it puts OTel types and an explicit `Context` into
  `reconfigure`, `packages`, `selfupdate` and the Supervisor plugins — modules whose whole shape is
  domain logic with `tracing` on top — and it would leave log correlation to be attached by hand at
  every site instead of once by a layer. The dependency buys the separation ADR-0011's hexagonal core
  is built on.
- **Span every message, as `opampsupervisor` does.** It would make our traces directly comparable to
  the ecosystem's, which [ADR-0004](0004-protocol-baseline-and-conformance.md) generally favours.
  Rejected on the ground stated in clause 32: ADR-0004 makes `opamp-go` the oracle for *protocol
  behaviour*, and what a Client chooses to measure about itself is not protocol behaviour. Nothing
  about it reaches the wire.
- **One span per operation, no child spans.** Simpler, and half the instrumentation. Rejected: the
  provisioned dashboard's central panel groups child spans by how often they fail, and *"which phase
  failed"* is the question an operator actually asks of a rollout. A single span answers only *"it
  failed"*, which the reported package status already says.
- **Link the post-restart self-update span instead of continuing the trace.** Span links are the
  standard's way to relate spans in different traces, and a restart is a real boundary. Rejected as
  the primary mechanism: the operator's question — *"did this update land"* — is one question, and
  answering it should not require finding a second trace and following a link. The marker carries the
  ids either way, so this stays a reversible choice about how they are used.

## Sources / Prior art

- [OpAMP specification — Agent's own telemetry](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  and the vendored schema:
  [the three capability bits](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L744-L756),
  [`TelemetryConnectionSettings`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L347-L375),
  [the `own_*` offer fields](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L521-L545) — the
  OTLP/HTTP/Protobuf requirement (*"The value MUST be a full URL an OTLP/HTTP/Protobuf receiver with
  path"*, which is why an empty string is a deviation and not merely an unusual value), the
  identifying attributes in the Resource, the process-metrics expectation, the "MAY refuse
  `http://`" provision (a MAY, which leaves this project free to decide where the line falls), and
  `certificate`, the one field for which the schema defines withdrawal by omission.
- **The Baseline `v0.20.0`, *Connection Settings Management*** — the *"3 classes of destinations"*
  framing and *"Depending on which connection settings are offered the sequence of operations is
  slightly different"* are the structural claim clauses 12–17 follow; the per-class capability
  gating listed there is what clause 11 aligns the Server with.
- **The Baseline, `ConnectionSettingsOffers.opamp`** — the verification MUST and its stated
  justification (*"does not lose access to the OpAMP Server"*), which is what scopes it to one field.
- **The Baseline, *Own Telemetry Reporting*** — the sequence with no verification step, the SHOULD
  that the Server offers in its first message *"unless there is no OTLP backend that can be used"*,
  and the SHOULD that the Agent sets `connection_settings_status` *"when new settings are received"*.
- **The Baseline, `ConnectionSettingsOffers`** — `own_metrics` / `own_traces` / `own_logs` (*"If this
  field is not set then the Agent should assume that the settings are unchanged"*) and `hash`
  (*"Hash of all settings, including settings that may be omitted from this message because they are
  unchanged"*). The sentence clause 23 departs from, quoted from
  `crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto`.
- [`opampsupervisor`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor)
  — the reference answer, which configures the Collector's own telemetry from the offer; read as the
  alternative this decision declines, for the reason ADR-0011 states. Its
  `cmd/opampsupervisor/supervisor/supervisor.go` (`main` as of 2026-08-21) — `updateOwnTelemetryData`,
  `setupOwnTelemetry`, `processOwnTelemetryConnSettingsMessage`, `loadLastReceivedOwnTelemetryConfig`,
  and the dispatch in `onMessage` — was read in full rather than summarised; the offer behaviour in
  the Context section is quoted from it. And
  [contrib#38724](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/38724) /
  [contrib#38797](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38797) — the
  same span gap in the reference implementation, the operations it chose to span, and its use of
  `span.SetStatus` for outcomes; the prior art clauses 31–32 follow on structure and depart from on
  message handling.
- **`opamp-go`, `client/internal/receivedprocessor.go`** — the pass-through of `OwnMetrics`,
  `OwnTraces`, `OwnLogs` into `MessageData`, gated per capability, with no merge against prior state
  and no persistence. The library leaves the semantics to the Client, which is why the supervisor's
  reading is the ecosystem's reading.
- **`opampextension` (opentelemetry-collector-contrib)** — a widely deployed Client that implements
  `ReportsEffectiveConfig`, `ReportsHealth` and `ReportsAvailableComponents` and *not* the
  connection-settings capabilities, which is why interoperability here cannot be checked against it
  and had to be read out of the specification instead.
- **Elastic `fleet-server`, `docs/opamp.md`** — *monitoring-only mode*, "No connection settings
  management": an implementation that answers the question by not implementing the mechanism.
- **Bindplane** — `com.bindplane.measurements.v1` / `reportMeasurements` custom messages and the
  throughput measurement processor carried in the pushed collector configuration: self-telemetry
  routed through the configuration channel, where removal is expressible.
- [`opentelemetry-otlp`](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/) and its
  [feature flags](https://lib.rs/crates/opentelemetry-otlp/features) — confirms the feature set
  clause 1 pins: `http-proto` is the OTLP/HTTP protobuf encoding, `reqwest-client` the async client
  against the default `reqwest-blocking-client`, and `grpc-tonic` the only thing that pulls a gRPC
  stack in.
- [`opentelemetry-appender-tracing`](https://docs.rs/opentelemetry-appender-tracing/latest/opentelemetry_appender_tracing/)
  — `OpenTelemetryTracingBridge`, the standard layer from `tracing` events to OTLP log records,
  registered on the subscriber registry beside the existing `fmt` layer. Version `0.32.0`: its own
  statement that it does not convert `tracing` spans, and the `CHANGELOG` entry removing
  `experimental_use_tracing_span_context` because `tracing-opentelemetry` now activates the context
  (fixing the circular dependency of
  [#3190](https://github.com/open-telemetry/opentelemetry-rust/issues/3190)). Read from the vendored
  crate source rather than a summary.
- **[`tracing-opentelemetry` 0.33.0](https://docs.rs/tracing-opentelemetry/0.33.0/tracing_opentelemetry/)**
  — `OpenTelemetryLayer`, the reserved `otel.name` / `otel.kind` / `otel.status_code` /
  `otel.status_description` fields, and `with_context_activation` (*"entering a span will activate its
  OpenTelemetry context, making it available to other OpenTelemetry instrumentation … By default,
  context activation is enabled"*). Also the version rule that puts `0.33` against `opentelemetry`
  `0.32`.
- **`opentelemetry-rust` issues [#1378](https://github.com/open-telemetry/opentelemetry-rust/issues/1378),
  [#2803](https://github.com/open-telemetry/opentelemetry-rust/issues/2803),
  [#2824](https://github.com/open-telemetry/opentelemetry-rust/issues/2824)** — the history of
  log/trace correlation between these two crates, and the reason to state plainly *which* versions
  this decision depends on.
- [`opentelemetry-proto`](https://github.com/open-telemetry/opentelemetry-proto) — the schema, which
  this decision deliberately does **not** copy: `opentelemetry-otlp` already depends on the crate
  generated from it, so it is maintained upstream rather than here.
- [`sysinfo`](https://docs.rs/sysinfo/latest/sysinfo/) — cross-platform process CPU and memory, pure
  Rust over `libc`.
- **[RFC 1918](https://www.rfc-editor.org/rfc/rfc1918)** (private IPv4 address space) and
  **[RFC 4193](https://www.rfc-editor.org/rfc/rfc4193)** (unique-local IPv6) — the definitions clause 8
  takes verbatim, and the reason the set is exactly three IPv4 ranges and one IPv6 range.
- **[RFC 6761](https://www.rfc-editor.org/rfc/rfc6761) §6.3** — `localhost` resolves to loopback by
  specification, which is what lets clause 18 keep it as the single admitted name without resolving
  it.
- **[RFC 3927](https://www.rfc-editor.org/rfc/rfc3927)** (IPv4 link-local) and
  **[RFC 6598](https://www.rfc-editor.org/rfc/rfc6598)** (carrier-grade NAT) — the two ranges clause 20
  names and excludes.
- **Rust's standard library** — `Ipv4Addr::is_private` is the RFC 1918 trio exactly, and
  `Ipv4Addr::is_loopback` is `127.0.0.0/8`; `Ipv6Addr::is_unique_local` is still unstable, so
  `fc00::/7` is spelled out at the code with a note saying why.
- **This project's own `crates/server/src/api.rs`** — `is_internal`, which decides where a
  client-supplied artifact URL may steer the Server. It blocks link-local (*"where `169.254.169.254`
  lives"*) and the CGNAT range, and deliberately does *not* block loopback or the RFC 1918 /
  unique-local ranges, *"an operator's mirror legitimately lives on an internal network"*. Clauses 8
  and 20 draw the same line for the same reasons, reconciled on purpose: two admission tests in one
  codebase disagreeing about what "private" means is how a gap gets found by somebody else.
- **This project's own Server** — `fleet.rs::settings_offer` and
  `crates/server/tests/own_telemetry.rs`, which implement and assert the standalone telemetry offer.
- **[ADR-0014](0014-server-driven-connection-settings.md)'s verification rule** and its reasoning,
  which clause 13 confines to the OpAMP class and leaves untouched in force there.
- **[ADR-0004](0004-protocol-baseline-and-conformance.md)** — `opamp-go` as the behavioural oracle,
  and the symmetric-misreading argument that makes it one. The tie-breaker clauses 23–25 invoke.
- **[ADR-0026](0026-the-client-logs-to-a-file-in-service-mode.md)** — the second `fmt` layer, which is
  where the visible change to log lines lands.
- **[ADR-0017](0017-client-self-update-and-its-consent.md)** — the `UpdateMarker` and the split across the restart
  that clause 33 rides on.

## Consequences

- Positive: three capability bits at once. The only one left is `AcceptsOtherConnectionSettings`,
  which is left undone deliberately and on the record — and clause 17 answers in advance the question
  that would otherwise have to be reopened when it lands.
- Positive: the fleet's Clients get observable without a second agent on the host — and the Server
  decides where that telemetry goes, which is the same "one place decides" the whole project is for.
- Positive: the wire format and the semantic conventions stay upstream's to maintain. A convention
  that moves arrives as a version bump in `Cargo.toml`, not as a silent divergence nobody diffs.
- Positive: the feature works in the configuration it was designed for. A Server holding only
  `[telemetry_offer]` reaches its Agents, and the hash gate closes because there is an
  acknowledgement to close it with. An operator gets metrics without configuring credential rotation:
  the two settings are independent, which is what they always were on the wire.
- Positive: a telemetry endpoint move does not disconnect the fleet, as it would if every change
  carried a synthetic `opamp` block through verify-and-reconnect.
- Positive: the shape between "one machine" and "an ingress with a certificate" is configurable. A
  Collector on a LAN address is offered to the fleet as a plain `http://` URL, and the Agents report
  to it. That is the configuration most fleets of this size are actually in. Loopback is still
  loopback, so the development stack and the published ports in `.devcontainer/` keep their meaning.
- Positive: own telemetry can be switched off from the fleet. Per signal, by dropping it from
  `[telemetry_offer]`; entirely, by stating an empty endpoint. No host is touched, no state file is
  deleted, and the Agent acknowledges the change like any other offer.
- Positive: one reading, held by both ends and by the oracle. Our Server can drive a
  supervisor-backed Agent without silently disabling signals it did not mention, and a
  supervisor-shaped Server can drive our Client. The interop harness of ADR-0004 has a case it can
  actually assert, rather than only asserting our own reading back to us.
- Positive: the Server's silence keeps its meaning. Clause 24 leaves an OpAMP-only offer, a
  certificate rotation and a heartbeat change unable to disturb telemetry — the independence of the
  three classes stays intact.
- Positive: a failed rollout is one trace with a failing phase in it rather than a hunt through a day
  of log lines, and the provisioned dashboard shows real operations instead of what
  `send-test-telemetry.py` puts there.
- Positive: the logs are joinable. Trace context on exported log records is what
  `.devcontainer/OBSERVABILITY.md` assumed when it collapsed three stores into one.
- Positive: instrumentation is free when nobody asked for it. With no destination offered the slot is
  empty, and an `#[instrument]` is a `tracing` span no layer subscribes to.
- Negative / trade-offs: five crates and a second batching machinery enter a Client that already has
  its own scheduling. They bring a *global* provider model, which sits awkwardly beside a
  destination that arrives from the Server at runtime and can change: applying a new offer means
  building fresh providers, installing them, and shutting the old ones down cleanly. That sequence
  is the part of this decision most likely to be fiddly, and it needs a test that changes the
  destination while telemetry is in flight.
- Negative / trade-offs: the SDK's own diagnostics and this Client's `tracing` output share a
  process, and the logs bridge exports what `tracing` emits. Exporter errors must not be bridged
  back into the exporter — the `internal-logs` feature and the bridge need to be kept from feeding
  each other.
- Negative / trade-offs: writing the Resource from the `AgentDescription` means the Client's
  telemetry carries whatever the operator put in `[attributes]`. That is what the Baseline asks for,
  and it is worth saying out loud before someone tags an Agent with something they would not send to
  a telemetry backend.
- Negative / trade-offs: a Collector's *internal* metrics still do not reach the fleet's backend
  through this Client. An operator who wants them configures the Collector for them, as they would
  without OpAMP — this decision makes the Supervisor's outside view available, not the inside one.
- Negative: two lifecycles exist where one would be simpler. "Every offer is proved by connecting"
  would be a rule with no exceptions and easy to hold in the head; it holds for one field of three.
  The mitigation is that the seam is the Baseline's own and is named in one predicate rather than
  spread through the transports — but it is a second case to reason about.
- Negative: a refused telemetry destination fails the whole offer. By clause 14, a cleartext public
  `own_logs` endpoint makes an offer `FAILED` that also carried a perfectly good credential
  rotation — which did apply, since verification succeeded, but is reported as part of a failed
  offer. The `error_message` names what was dropped, so the Server can tell; a Server that reads
  only the status enum sees a failure it may not deserve. Accepted as the price of one hash.
- Negative: the Server declares `OffersConnectionSettings` more often, and that is visible to peers.
  A Server with only `[client_ca]` declares a capability whose standing offer is empty until a CSR
  arrives. That is what the bit means — it *can* offer — but it is a wide declaration, and a peer
  Client may probe it.
- Negative: the cleartext test is on the address's *class*, not on reachability or on which network
  the Agent is actually on. `http://192.168.10.5:4318` is admitted by every Agent in the fleet,
  including one on a different site where that address belongs to a different machine — and that
  Agent will send its logs there, in cleartext, to whatever answers. Loopback had no such ambiguity:
  it was always the same host. The mitigation is that the destination is one fleet-wide operator
  decision rather than something an Agent discovers, but the ambiguity is real.
- Negative: a private network is not a trusted network, and clauses 8 and 18–22 assume it is. A flat
  office LAN with a guest VLAN bridged onto it satisfies every clause here. What is being decided is
  that the operator's network is the operator's problem, which is a reasonable division of
  responsibility and is nevertheless weaker than what loopback alone guarantees.
- Negative: cleartext by name stays refused, and names are what operators use. An operator whose
  Collector is `collector.lan` on DHCP has to pin an address or terminate TLS. Clause 18 accepts that
  friction deliberately, but it is friction, and it will be reported as a bug at least once.
- Negative: this is a recorded deviation from the Baseline. The claim that nothing implemented
  diverges does not hold, and the document's honesty depends on the *Deviations* row of clause 27.
- Negative: we emit a value the schema forbids. An empty `destination_endpoint` fails the MUST that
  requires a full URL. A strict third-party Client may reject the message, and would be right to;
  what it costs is that the withdrawal does not reach that Client, not that anything else breaks.
- Negative: a third-party Server written to the literal text can disable a signal by accident. A
  Server that rotates the metrics headers by sending `own_metrics` alone will, under clause 23, stop
  our traces and logs. That is exactly what it would do to a supervisor, so the failure mode is the
  ecosystem's rather than ours — but a literal-reading Client would be immune to it, and this one is
  not.
- Negative: the meaning of an offer depends on which fields it carries. "Names none of the three" and
  "names one of the three" are different kinds of message, and the difference is load-bearing. It is
  one sentence to state and one predicate in the code, but it is a rule an operator can be surprised
  by, and stating it in `config/server.toml` beside the endpoints is part of the work.
- Negative: a fourth OTel crate, on a release train of its own. `tracing-opentelemetry` is numbered
  one ahead of the OTel crates and released after them, so a future `opentelemetry` bump has a second
  cadence to wait for. That is a real maintenance cost and it is why the version pairing is written
  into clause 28 rather than left to `cargo update`.
- Negative: stderr and the log file change shape. The `fmt` layers print the enclosing span's name
  and fields on every event inside an instrumented operation, so lines an operator knows gain a
  `package.install{package=…}:` prefix. Better context, different output — including in the ADR-0026
  file, which is read with a pager and by whatever the operator greps with.
- Negative: a second surface where data leaves the host. Clause 36 states the discipline, but it is a
  discipline: a span field is as easy to add as a log field and reaches a destination the *Server*
  named. This is the same exposure the Resource attributes have, spread across five modules instead
  of one function.
- Negative: the offer that switches tracing on is itself untraced. The span of the
  `connection.settings.apply` that installs the exporter is created before the exporter exists, so
  that one apply is missing from the destination it just configured. It cannot be otherwise — there
  is nothing to export to until that operation is half done — and it corrects itself on the next
  start, where the persisted settings put the exporter in force before anything runs. Worth knowing
  before somebody reads the gap as a bug.
- Negative: the update marker is a telemetry carrier. It is an operational file with a safety role,
  and clause 33 puts two ids in it that have nothing to do with that role. The fallback keeps a
  marker without ids working, but the file has two reasons to change.
- Follow-ups: whether the Server should also *store* or forward what it is offered a destination for
  is a separate question, and the answer is probably no — the specification's non-goal keeps this
  project out of the telemetry-backend business.
- Follow-ups: `AgentSummary` exposes `remote_config_status` but not `connection_settings_status`, so
  a stalled or failed rotation — and an Agent that has *stopped* reporting own telemetry — is
  invisible in the fleet view and in the REST API. Worth its own change; not decided here. Whether
  the telemetry class should also carry the interim `APPLYING` state, given there is no lengthy
  verification to be in the middle of, is left as it is — it is reported, because the Baseline's
  status enum has it and a Server may be watching for it.
- Follow-ups: whether the Server should refuse to *compile* a `[telemetry_offer]` it can tell no Agent
  will accept — a public `http://` address, accepted by the Server and refused by every Agent it
  reaches — is a separate decision about where configuration errors are caught, and is not taken
  here.
- Follow-ups: if upstream settles the offer-reading question the other way, a reversal is a new ADR,
  and the deviation row is where the cost of that reversal is already written down. Watching for it
  belongs with the Baseline bump, which is the moment this project reads upstream's changes anyway.
- Follow-ups: whether Gateway Mode should carry trace context across the hop — it forwards messages
  unchanged and speaks in no Agent's name ([ADR-0024](0024-gateway-mode.md)), so relating a
  downstream Agent's operation to the Gateway's own would be a new decision. And whether a Server
  should be able to ask for less than everything (a sampling hint in the offer) if a large fleet ever
  makes clause 34 wrong; the Baseline offers no field for it.
