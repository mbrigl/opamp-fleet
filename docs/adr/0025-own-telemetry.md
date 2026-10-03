# ADR-0025: An Agent reports its own telemetry over OTLP/HTTP to the destinations the Server names

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-agent/src/telemetry.rs, the telemetry half of crates/fleet-agent/src/connection.rs, the operation spans in the Client's transport, engine, reconfigure, packages, update and Supervisor modules, the Server's `[telemetry_offer]` (crates/fleet-server/src/config.rs, crates/fleet-server/src/fleet.rs), and the OpenTelemetry crates in Cargo.toml

## Context

Three capability bits of the Baseline are one feature: `ReportsOwnTraces` (`0x0020`),
`ReportsOwnMetrics` (`0x0040`) and `ReportsOwnLogs` (`0x0080`), all `[Beta]`. Each means "the Agent
can report own \<signal\> to the destination specified by the Server via
`ConnectionSettingsOffers.own_*`". The destination is a `TelemetryConnectionSettings` whose
`destination_endpoint` "MUST be a full URL an OTLP/HTTP/Protobuf receiver with path", and the Agent
"MAY refuse to send the telemetry if the URL begins with `http://`"
([`opamp.proto`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto)). The Baseline asks that the
`AgentDescription`'s identifying attributes appear in the OTLP Resource, and names process metrics
— "CPU or RAM usage" — as what own metrics are for.

**"Own" cannot mean a Managed Process's internals.** Upstream's `opampsupervisor` answers the offer
by configuring the Collector's *internal* telemetry. That road is closed here: a Supervisor passes a
Configuration through without touching it
([ADR-0015](0015-supervisor-mode-and-its-kinds.md)), and the specification's non-goals forbid an
abstraction over a Managed Process's configuration language. What this Client can honestly report
is what it observes from outside: its own process, and the processes it spawned and holds the pids
of.

**All three signals have a subject.** Metrics are process metrics. Logs are what the Client already
writes through `tracing`. Traces are the control loop's existing lifecycles — a configuration
apply, a package install, a self-update — which already have phases and an outcome the Server is
told; they are spans to map, not an instrumentation project. Making them needs a producer: the log
bridge converts `tracing` *events* and says so itself (*"This crate does not convert `tracing` spans
into OpenTelemetry spans. Use `tracing-opentelemetry` for that."*). `tracing-opentelemetry` `0.33`
is built against the `opentelemetry` `0.32` this workspace runs, and with it entering a `tracing`
span activates its OpenTelemetry context, so the log appender picks up the trace context with no
feature flag.

**OTLP has a reference implementation, and it is the standard's own.** This project vendors the
OpAMP schema because it owns that wire contract
([ADR-0010](0010-protocol-baseline-and-conformance.md)). OTLP is not its protocol: its wire format,
semantic conventions and versioning are maintained upstream, and `opentelemetry-otlp` is where that
maintenance lands.

**Cleartext has a line to be drawn.** The Resource carries identifying attributes and the log records
carry whatever the Client logs, as a continuous stream. Loopback alone would force every small fleet
with one Collector on its LAN to put TLS in front of a stream that never leaves the operator's
network. The private address space — [RFC 1918](https://www.rfc-editor.org/rfc/rfc1918)'s three IPv4
ranges and [RFC 4193](https://www.rfc-editor.org/rfc/rfc4193)'s `fc00::/7` — is not routable across
the public internet, and the Server's artifact-URL check (`is_internal` in
[`api.rs`](../../crates/fleet-server/src/api.rs)) already draws the same line.

**The Baseline cannot switch own telemetry off.** For each `own_*` field the schema says *"If this
field is not set then the Agent should assume that the settings are unchanged"*, and the empty string
is not a legal `destination_endpoint`. Read literally, a destination once in force can be moved but
never ended, short of deleting state on each host. The one widely deployed Client that implements
these fields, `opampsupervisor`, reads them otherwise: it builds its own-telemetry section from each
received message alone, drops a signal whose endpoint is absent or empty, and logs *"Disabling own
telemetry pipeline in the config"* when nothing is left — but only enters that path when the message
names at least one of the three. `opamp-go` is this project's behavioural oracle
([ADR-0010](0010-protocol-baseline-and-conformance.md)); here the literal reading would leave both
ends unable to drive each other.

How an offer is applied, acknowledged and persisted — the telemetry destinations as a class of offer
of their own, without a verifying connection or a reconnect — is
[ADR-0018](0018-connection-settings-and-server-capabilities.md)'s. This ADR decides what own
telemetry is, what an offer of it means, and what the Client admits.

## Decision

We will implement all three own-telemetry capabilities through the OpenTelemetry Rust SDK, exporting
OTLP/HTTP with protobuf bodies only to destinations the Server offers, read an offer that names any
of them as the complete state of all three, admit cleartext only inside the private address space,
and invent nothing OTLP already defines.

1. **The standard's own implementation, not a copy of its schema.** `opentelemetry`,
   `opentelemetry_sdk` and `opentelemetry-otlp` carry the wire format, with the exporter's features
   stated rather than inherited:

   ```toml
   opentelemetry-otlp = { version = "0.32", default-features = false,
                          features = ["http-proto", "reqwest-client", "trace", "metrics", "logs"] }
   ```

   `reqwest-client` replaces the default blocking client; `grpc-tonic` stays off because the schema
   permits no gRPC destination. `tracing-opentelemetry` joins them at the version built against them
   — `0.33` against `opentelemetry` `0.32` — and that pairing is a decision, not a range to widen. No
   OTLP schema is vendored ([ADR-0010](0010-protocol-baseline-and-conformance.md) covers OpAMP only).

2. **Names come from the standard where it has them.** Metric and attribute names are taken from
   `opentelemetry-semantic-conventions` (with `semconv_experimental`, since the process metrics are
   experimental upstream), never written as string literals, so a moved convention is a compile
   error on the next bump. The operation span names of clause 8 are this project's vocabulary
   because OpenTelemetry defines none for an agent's own lifecycle; they are not to be "corrected"
   towards a convention that does not cover them.

3. **"Own" is what the Client observes from outside.** The Client's own Agent reports the Client's
   process; each Supervisor-backed Agent reports its Managed Process, by the pid the Supervisor
   already holds. No Managed Process's configuration is touched to obtain telemetry. CPU and memory
   for a pid come from `sysinfo` (pure Rust, Linux, macOS and Windows), the one gap the SDK leaves.

4. **Metrics are process metrics per Agent, every 10 s.** `process.memory.usage`,
   `process.cpu.utilization` and `process.uptime`, recorded as gauges, each sample carrying the
   sampled Agent's `service.instance.id`, `service.instance.name` and `service.name` — the Resource
   is the Client's, so a Managed Process's series would otherwise carry the Client's identity.
   Sampling and export share one interval, 10 s, the Baseline's recommended reporting interval; the
   periodic reader is told it explicitly, since its default is 60 s. A pid that has gone away records
   nothing.

5. **Logs are the Client's `tracing` output, bridged.** `opentelemetry-appender-tracing`'s
   `OpenTelemetryTracingBridge` turns events into OTLP log records at the level the log filter
   already selects. Stderr and the service-mode log file
   ([ADR-0014](0014-the-client-as-an-installed-service.md)) keep everything they print; this adds a
   destination. A record written inside an instrumented operation carries that operation's `TraceId`
   and `SpanId`, which is what makes a log-to-trace join answerable at the receiving end.

6. **Traces come from the `tracing` spans this Client writes.** `tracing-opentelemetry`'s layer
   converts them; instrumentation is written in `tracing` (`#[instrument]`, `tracing::info_span!`)
   and no OpenTelemetry type appears outside `telemetry.rs`. An outcome becomes the span status
   through the crate's reserved fields `otel.status_code` and `otel.status_description`, written
   through `telemetry::failed` and `telemetry::succeeded`; the failure text is the same string the
   Server is told.

7. **Both bridges live in reload slots.** `tracing` has one subscriber per process, installed before
   any destination is known, so the log bridge and the span layer each sit in a
   `tracing_subscriber::reload` slot held open from startup, filled when an offer puts a provider in
   force and emptied when it is withdrawn. A slot is emptied before its provider shuts down, so an
   event or span during shutdown never reaches a closing exporter. With no destination, an
   instrumented span costs what a `tracing` span costs with no layer interested in it.

8. **Five operations are root spans, and their phases are child spans.**

   | Root span | Phases | Opened in |
   |---|---|---|
   | `package.install` | `download`, `verify`, `stage`, `preflight`, `swap`, `gate`, `rollback` | `transport::process_package_downloads` |
   | `config.apply` (Supervisor set) | `validate`, `stop`, `write`, `purge`, `start` | `reconfigure::apply` |
   | `config.apply` (Managed Process) | `reload` or `restart`, `gate` | `engine::handle`, where the Configuration is handed over |
   | `connection.settings.apply` | `verify`, `store` | `transport::process_connection_offer` |
   | `self.update` | `stage`, `probe`, then `commit` or `roll_back` | `update::installer::install` |

   An install and a Managed Process's apply are begun by the task that received the message and
   finished by the Supervisor's own task, so the span travels with the command through the Port
   (`ProcessCommand::ApplyConfig` and `ApplyPackage` each carry one); a trace that ended at the
   hand-over would stop where the interesting failures are. A self-update is always reached from a
   package offer, so `self.update` is in practice a child of the `package.install` that downloaded
   it. The reconnection after `connection.settings.apply` is a field on the root span, not a phase:
   it happens after the operation returns, in the transport loop.

9. **Message handling is not a span.** A transport exchange (a poll, a WebSocket receive), a sampler
   tick and the Supervisor Endpoint's message handling have no outcome and no end; at one exchange
   per Agent per interval they would bury the five operations. A span that belongs to an operation is
   therefore opened inside the per-exchange function, where the operation begins, never on the
   function itself. A failed exchange stays a logged warning, carrying the trace context of whatever
   operation is in flight.

10. **A self-update stays one trace across its restart.** The trace id and the span id of the
    `self.update` span go into the update marker
    ([ADR-0021](0021-the-client-updates-itself.md)) as hex, and the process that comes up after the
    restart opens `commit` or `roll_back` as a child of that span, its parent marked remote. A marker
    without ids, or with ids that do not parse, makes the post-restart span open its own trace; a
    trace is never a reason to fail an update. `self.update` itself records no status — the process
    that stages a version does not learn whether it stays up.

11. **No sampler.** Every span is exported: the volume is a handful of fleet operations per host per
    day, and a sampler would first drop the rare failed rollout a trace exists for.

12. **Every signal is attributed to its Agent.** The OTLP Resource carries the `AgentDescription`'s
    identifying attributes, plus four descriptive ones named one by one — `service.instance.name`,
    `os.type`, `host.arch`, `os.description` — each only where the description carries it. No other
    non-identifying attribute (`host.ip`, `host.mac`, operator `[attributes]`) is put on the Resource:
    widening that list is a decision about what leaves the host.

13. **A span attribute is data leaving the host, and is chosen one by one.** A Supervisor name, a
    package name and version, a configuration hash, a count — never a whole Configuration, an
    offer's headers, or a URL that can carry a credential. A download source is labelled by scheme,
    host and path only, its query dropped, because a pre-signed URL puts its signature there; the
    `Debug` impls that hide package header values must not be defeated by a span field.

14. **The exporters send through a client bound to this process's runtime, with a bounded export.**
    The SDK exports from dedicated threads that block on the request and have no Tokio reactor, so
    each request is spawned onto the runtime handle captured when the exporter was built. The HTTP
    client carries this Client's TLS trust and a 5 s timeout — half the reporting interval — because
    neither the periodic reader nor a client handed to `opentelemetry-otlp` bounds an export, and a
    destination that stops answering would otherwise hold the exporter until the Client restarts.

15. **Destinations come only from the Server.** There is no destination in `supervisor.toml`: the
    capability is reporting "to the destination specified by the Server". With no destination
    offered, nothing is built and nothing is sent. The offer's lifecycle — applied in place,
    acknowledged on the same offer, persisted and put back in force at startup — is
    [ADR-0018](0018-connection-settings-and-server-capabilities.md)'s.

16. **The three capabilities are declared unconditionally.** They state an ability — "I can report
    to a destination you name" — and the offer arms it. Declaring them only once a destination is in
    force would mean the Server could never make the first offer.

17. **An offer that names any telemetry destination states all three; one that names none changes
    nothing.** When any of `own_metrics`, `own_traces`, `own_logs` is present, the offer is the whole
    truth about own telemetry: a signal it does not name is stopped and leaves the persisted state,
    not carried over. An offer naming none of the three — OpAMP settings only, a certificate, a
    heartbeat change — leaves all three as they are; that keeps the classes of offer of
    [ADR-0018](0018-connection-settings-and-server-capabilities.md) independent.

18. **An empty `destination_endpoint` withdraws that signal.** It is admitted, not refused: the
    exporter is shut down, the destination leaves the persisted state, nothing is reported against
    it, and the offer is acknowledged `APPLIED`. It is the only way to stop all three, since by
    clause 17 an offer naming nothing cannot say it. The Server sends it as clause 23 describes.

19. **Cleartext is admitted inside the private address space and nowhere else.** An `http://`
    destination is admitted when its host is loopback (`127.0.0.0/8`, `::1`, or the name
    `localhost`), in RFC 1918's `10.0.0.0/8`, `172.16.0.0/12` or `192.168.0.0/16`, or in RFC 4193's
    `fc00::/7`. Link-local (`169.254.0.0/16`, `fe80::/10`, where cloud metadata services live) and
    carrier-grade NAT (`100.64.0.0/10`, shared with other subscribers) are not admitted, and are
    named here so that "private" does not quietly widen. An `https://` destination is always
    admitted; an endpoint that is neither is refused as not an OTLP/HTTP URL.

20. **The judgement is made on a parsed address, never on a name.** The host is extracted from the
    URL — an IPv6 literal from its brackets — parsed as an IP address and tested for range
    membership; a prefix match is never used (`192.168.0.1.example.com` and `172.32.0.5` are
    refused). `localhost` is the only name admitted, because it is loopback by definition
    ([RFC 6761](https://www.rfc-editor.org/rfc/rfc6761)). No other name is resolved: an admission
    test that a re-resolve can flip, on a destination that is persisted and re-applied, is not one
    an operator can reason about. A Collector reached by name over cleartext is named by address or
    put behind TLS.

21. **A refused destination is reported, never warned about or downgraded.** Its exporter is not
    built, and the refusal reaches the Server in the offer's `connection_settings_status`
    ([ADR-0018](0018-connection-settings-and-server-capabilities.md) clause 7) with the reason; the cleartext
    message names the ranges that would be accepted. This is one step firmer than the cleartext
    credential warning of [ADR-0017](0017-admission-and-authentication.md), because this is a
    continuous stream of identifying attributes and log records.

22. **`certificate`, `tls` and `proxy` behave as they do on the OpAMP settings.** An offered
    `certificate` is honoured: its `cert` is presented by the exporter, paired with the key this
    Client generated for its CSR — the client-certificate machinery of
    [ADR-0017](0017-admission-and-authentication.md), reused as-is. A `private_key` in the offer is
    refused by name, because this Client's private key never leaves its host and is never accepted
    from the Server; a `cert` with no key on disk to pair with is refused by name too. `ca_cert` is
    not added to the trust store, as the Baseline advises. `tls` and `proxy` are refused and reported
    by name.

23. **The Server offers destinations from a `[telemetry_offer]` section.** `metrics_endpoint`,
    `traces_endpoint`, `logs_endpoint`, and one `[telemetry_offer.headers]` table sent with every
    signal (typically the backend's access token). Endpoints are full OTLP/HTTP URLs with path; the
    Server appends no `/v1/…`. At startup a section naming no endpoint, or an endpoint that is not an
    `http://` or `https://` URL, fails loudly; the Server judges nothing about cleartext — that is
    the Agent's rule, decided where the packets originate. What the section says, it says about all
    three signals (clause 17). An endpoint set to `""` is a withdrawal to be sent, carries no headers,
    and counts as something to offer. Removing the section withdraws nothing — a Server cannot tell
    "never offered" from "withdrawn", and must not tear down a fleet another Server pointed at a
    Collector. Each Agent is offered a destination only for the signals it declares. How the
    destinations ride in the one hashed `ConnectionSettingsOffers` and when the Server declares
    `OffersConnectionSettings` is [ADR-0018](0018-connection-settings-and-server-capabilities.md)'s.

24. **The departure from the Baseline is recorded, not argued upstream.** Clauses 17, 18 and 23
    depart from the schema's "not set means unchanged" and from the `destination_endpoint` MUST.
    [`CONFORMANCE.md`](../CONFORMANCE.md) carries this as a row under *Deviations*, naming the
    sentences departed from, the implementation followed and the reason. Settling the disagreement
    between the schema and `opampsupervisor` is upstream's; this project takes no position in
    `opamp-spec`.

**Out of scope:** a Collector's *internal* telemetry (an operator configures the Collector for it);
the Server storing or forwarding telemetry — the specification keeps this project out of the
telemetry-backend business; `AcceptsOtherConnectionSettings`; trace context across a Gateway hop
([ADR-0009](0009-client-modes-and-the-gateway.md)); a sampling hint from the Server; a local switch
on the Agent that drops the three capability bits.

## Alternatives considered

- **Vendor `opentelemetry-proto` and encode OTLP by hand.** The cheaper build — prost, protox and the
  `reqwest` already present, no new crate. Rejected: a second copied schema is a second protocol to
  keep in sync with none of the Baseline machinery that makes the OpAMP copy safe, and its semantic
  conventions would become string literals nobody re-checks. Owning the bytes is right where the
  bytes are the product; here they are the exhaust.
- **Keep the SDK's default features.** The defaults select `reqwest-blocking-client`, and blocking
  HTTP inside a tokio runtime starves an executor.
- **Send OTLP over gRPC.** The schema requires an OTLP/HTTP/Protobuf receiver.
- **Configure the Collector's internal telemetry from the Supervisor**, as `opampsupervisor` does.
  The richer answer for a Collector, and what an operator from upstream expects. Rejected: this
  Client does not touch a Managed Process's configuration, and inventing an abstraction over it is a
  non-goal.
- **Use the OpenTelemetry trace API directly.** No added dependency, and what the reference
  implementation does in Go. Rejected: it puts OTel types and an explicit `Context` into domain
  modules built as logic with `tracing` on top, and log correlation would be attached by hand at
  every site instead of once by a layer.
- **Metrics and logs only; stop declaring `ReportsOwnTraces`.** Rejected: the operations that fail in
  a fleet are already multi-phase lifecycles with outcomes, and the trace is the one signal that
  answers *why* a rollout failed rather than *that* it did.
- **Span every message**, as `opampsupervisor` does. That supervisor manages one Collector, where
  message handling is the work; a Client here multiplexes n Agents over one connection on every host
  of a fleet. The oracle governs protocol behaviour, and what a Client measures about itself never
  reaches the wire.
- **One span per operation, no child spans.** *"Which phase failed"* is the question an operator asks
  of a rollout; a single span answers only *"it failed"*, which the reported status already says.
- **Link the post-restart span instead of continuing the trace.** Span links are the standard's way
  across a boundary, but *"did this update land"* is one question and should be one trace. The marker
  carries the ids either way, so this stays reversible.
- **A destination, or a list of trusted networks, in `supervisor.toml`.** A locally configured
  destination is a private extension wearing the capability's name, and a per-host allowlist would
  give a fleet two places to look when telemetry does not arrive — one on the host nobody is logged
  into.
- **Declare the capabilities only while a destination is in force**, as `OffersPackages` is. Those
  describe something the end has; these describe something it can do, and the conditional version
  deadlocks the first offer.
- **`https://` or loopback only.** Needs no assumption about the deployment, and costs the ordinary
  small fleet a certificate, a name and a renewal for a stream that never leaves its own network —
  a rule operators work around by a Collector per host or no telemetry at all.
- **Admit one private range only.** Which RFC 1918 range a site uses is an addressing-plan accident;
  admitting one and refusing the others would read as a bug.
- **Resolve names and admit those that resolve privately.** The answer would depend on what DNS said
  when the offer arrived, and the name can be re-pointed afterwards without any offer changing.
- **Warn instead of refusing**, as for credentials in cleartext. Proportionate to a credential on one
  request, not to a continuous stream of identifying attributes and logs.
- **The literal reading of the offer, with the gap documented.** Leaves no fleet-driven off switch,
  and misconfigures silently in both directions against the one peer that implements these fields.
- **Complete state per offer without the empty-endpoint withdrawal.** Cannot express "all off": an
  offer naming nothing means "unchanged", so the last remaining signal could never be stopped.
- **Withdraw automatically when `[telemetry_offer]` disappears.** The Server holds no record of what
  it offered, so every Server without the section would tombstone Agents another Server configured.
- **A signal of our own** — a `disabled` flag or a custom capability like Bindplane's. Inventing
  protocol semantics is a non-goal, and a bespoke signal would be understood by one Server and one
  Client. Departing towards the implementation everyone runs is the only departure that buys
  interoperability.
- **Raise it against `opamp-spec` and wait.** The protocol is Beta on a slow cadence, and this project
  implements the protocol and records its departures; it does not staff the protocol's evolution.

## Sources / Prior art

- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) —
  *Own Telemetry Reporting*, and the vendored
  [`opamp.proto`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto): the three capability bits,
  `ConnectionSettingsOffers.own_*` ("If this field is not set then the Agent should assume that the
  settings are unchanged") and `hash`, `TelemetryConnectionSettings` (the OTLP/HTTP/Protobuf MUST,
  the "MAY refuse `http://`", `certificate`'s withdrawal by omission and the advice on `ca_cert`),
  the identifying attributes in the Resource, and the 10 s recommended reporting interval.
- [`opampsupervisor`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor)
  — `cmd/opampsupervisor/supervisor/supervisor.go`: `updateOwnTelemetryData`, `setupOwnTelemetry`,
  `processOwnTelemetryConnSettingsMessage`, `loadLastReceivedOwnTelemetryConfig` and the dispatch in
  `onMessage`; and the spans it emits,
  [contrib#38724](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/38724) and
  [contrib#38797](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38797).
- `opamp-go`, `client/internal/receivedprocessor.go` — passes `OwnMetrics`, `OwnTraces`, `OwnLogs`
  through per capability, with no merge and no persistence.
- Elastic `fleet-server`, `docs/opamp.md` (monitoring-only mode, no connection settings management),
  and Bindplane's `com.bindplane.measurements.v1` — self-telemetry routed through the configuration
  channel instead.
- [`opentelemetry-otlp`](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/) and its
  [feature flags](https://lib.rs/crates/opentelemetry-otlp/features);
  [`opentelemetry-proto`](https://github.com/open-telemetry/opentelemetry-proto), the schema
  deliberately not copied.
- [`opentelemetry-appender-tracing`](https://docs.rs/opentelemetry-appender-tracing/latest/opentelemetry_appender_tracing/)
  `0.32` — `OpenTelemetryTracingBridge`, its statement that it does not convert spans, and the
  changelog entry removing `experimental_use_tracing_span_context` (fixing
  [opentelemetry-rust#3190](https://github.com/open-telemetry/opentelemetry-rust/issues/3190)).
- [`tracing-opentelemetry` 0.33.0](https://docs.rs/tracing-opentelemetry/0.33.0/tracing_opentelemetry/)
  — `OpenTelemetryLayer`, the reserved `otel.*` fields, `with_context_activation`; and the history of
  log/trace correlation between the two crates,
  [opentelemetry-rust#1378](https://github.com/open-telemetry/opentelemetry-rust/issues/1378),
  [#2803](https://github.com/open-telemetry/opentelemetry-rust/issues/2803),
  [#2824](https://github.com/open-telemetry/opentelemetry-rust/issues/2824).
- [`sysinfo`](https://docs.rs/sysinfo/latest/sysinfo/) — cross-platform process CPU and memory.
- [RFC 1918](https://www.rfc-editor.org/rfc/rfc1918), [RFC 4193](https://www.rfc-editor.org/rfc/rfc4193),
  [RFC 6761 §6.3](https://www.rfc-editor.org/rfc/rfc6761#section-6.3), [RFC 3927](https://www.rfc-editor.org/rfc/rfc3927)
  and [RFC 6598](https://www.rfc-editor.org/rfc/rfc6598) — the admitted ranges, `localhost`, and the
  two excluded ranges; Rust's `Ipv4Addr::is_private` (exactly the RFC 1918 trio), with `fc00::/7`
  spelled out because `Ipv6Addr::is_unique_local` is unstable.

## Consequences

- Positive: three capability bits from one mechanism, and a fleet's Clients become observable
  without a second agent on the host, at a destination the Server decides.
- Positive: the wire format and the semantic conventions stay upstream's; a moved convention arrives
  as a version bump.
- Positive: a failed rollout is one trace naming the failing phase, and its log lines are joinable to
  it by `TraceId`. With no destination offered, instrumentation costs nothing.
- Positive: own telemetry can be switched off from the fleet — per signal by leaving it out of
  `[telemetry_offer]`, entirely with empty endpoints — and both this Server and a supervisor-shaped
  one can drive this Client and each other's Agents with one reading.
- Positive: a Collector on a LAN address is reachable in plain `http://`, the shape most fleets of
  this size are in.
- Negative / trade-offs: five OpenTelemetry crates and their batching machinery, with a global
  provider model that sits awkwardly beside destinations that change at runtime; and
  `tracing-opentelemetry` is on a release train one step behind the others, so an `opentelemetry`
  bump has a second cadence to wait for.
- Negative / trade-offs: exporter diagnostics and the Client's `tracing` output share a process; the
  bridge must not feed exporter errors back into the exporter.
- Negative / trade-offs: the Resource carries what describes the Agent, and span fields are as easy
  to add as log fields; clauses 12 and 13 are a discipline, not a mechanism.
- Negative / trade-offs: stderr and the log file print the enclosing span on every event inside an
  operation, so familiar lines gain a `package.install{…}:` prefix.
- Negative / trade-offs: the update marker carries two ids unrelated to its safety role.
- Negative / trade-offs: the `connection.settings.apply` that installs the trace exporter is itself
  not exported — there is nothing to export to until it is half done. It corrects itself on the next
  start, when the persisted destination is in force before anything runs.
- Negative / trade-offs: admission is on the address's class, not on which network the Agent is on.
  `http://192.168.10.5:4318` is admitted by every Agent, including one on another site where that
  address is a different machine; and a private network is not necessarily a trusted one. Cleartext
  by name stays refused, which costs an operator with a DHCP-named Collector an address or TLS.
- Negative / trade-offs: this is a recorded deviation. The empty `destination_endpoint` fails the
  schema's MUST, so a strict third-party Client may reject the message; and a third-party Server that
  rotates one signal's headers by sending that signal alone stops this Client's other two.
- Follow-ups: whether the Server should refuse to compile a `[telemetry_offer]` endpoint no Agent will
  accept (a public `http://` address); surfacing `connection_settings_status` and a stopped reporter
  in the fleet view; watching for upstream settling the offer semantics at the next Baseline bump.

## Enforcement

- [`crates/fleet-agent/src/telemetry.rs`](../../crates/fleet-agent/src/telemetry.rs) tests:
  `no_destination_builds_nothing` (clause 15),
  `a_cleartext_destination_beyond_the_private_network_is_refused`,
  `cleartext_is_admitted_by_address_and_nowhere_else`,
  `a_private_network_destination_is_allowed_in_cleartext`,
  `a_loopback_destination_is_allowed_in_cleartext` (clauses 19–21),
  `an_empty_endpoint_stops_reporting_and_refuses_nothing` (clause 18),
  `offered_tls_settings_are_refused_by_name`, `an_offered_certificate_is_presented_by_the_exporter`,
  `an_offered_certificate_without_its_key_is_refused_and_named`,
  `an_offered_private_key_is_refused_by_name` (clause 22),
  `metrics_are_reported_at_the_interval_the_baseline_recommends` (clause 4),
  `the_resource_carries_the_agents_identifying_attributes`, `the_resource_carries_the_platform`,
  `the_resource_carries_no_other_non_identifying_attribute`,
  `an_agent_without_an_instance_name_reports_none` (clause 12),
  `an_export_to_a_destination_that_never_answers_gives_up` (clause 14),
  `a_span_this_client_writes_reaches_the_offered_destination` (clauses 6–7),
  `a_trace_survives_being_written_down_and_picked_up_again`,
  `an_unreadable_trace_reference_is_ignored` (clause 10).
- [`crates/fleet-agent/src/connection.rs`](../../crates/fleet-agent/src/connection.rs) tests:
  `an_offer_naming_one_signal_stops_the_others`,
  `an_offer_silent_about_telemetry_leaves_all_three_alone` (clause 17),
  `an_empty_endpoint_withdraws_the_signal` (clause 18).
- [`crates/fleet-agent/src/transport/mod.rs`](../../crates/fleet-agent/src/transport/mod.rs):
  `a_refused_telemetry_destination_is_reported_failed_on_the_same_offer` (clause 21).
- [`crates/fleet-agent/src/update/installer.rs`](../../crates/fleet-agent/src/update/installer.rs):
  `a_marker_carries_its_trace_and_one_written_without_it_still_parses` (clause 10);
  [`crates/fleet-agent/src/packages.rs`](../../crates/fleet-agent/src/packages.rs):
  `the_download_source_drops_whatever_authorises_it` (clause 13).
- [`crates/fleet-agent/tests/supervisor_process.rs`](../../crates/fleet-agent/tests/supervisor_process.rs):
  `the_phases_of_an_install_hang_off_the_span_that_came_with_it` (clause 8).
- [`crates/fleet-server/tests/own_telemetry.rs`](../../crates/fleet-server/tests/own_telemetry.rs):
  `every_declared_signal_is_offered_a_destination`,
  `a_withdrawn_signal_is_offered_as_an_empty_destination`, `an_undeclared_signal_gets_no_destination`,
  `an_agent_that_reports_no_own_telemetry_is_offered_none` (clause 23); and
  [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `an_empty_endpoint_is_a_withdrawal_and_a_wrong_one_is_still_an_error` (clause 23).

**Not mechanically decidable:** that no OTLP schema is vendored and no OpenTelemetry type appears
outside `telemetry.rs` (clauses 1, 6), that names come from the semantic conventions (clause 2), that
message handling stays unspanned and no sampler is configured (clauses 9, 11), that a new span field
carries nothing that should not leave the host (clause 13), and that the *Deviations* row stays in
step with clauses 17, 18 and 23 (clause 24) — these are review questions on each change to the
files this ADR applies to.
