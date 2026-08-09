# ADR-0036: An Agent reports its own telemetry over OTLP/HTTP, through the OpenTelemetry SDK

- **Status:** 🟡 proposed
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

With [ADR-0035](0035-mutual-tls-and-the-server-issued-client-certificate.md) implemented, 17 of the
Baseline's 23 capability bits are implemented and 23 of the 24 behaviour rows in
[`CONFORMANCE.md`](../CONFORMANCE.md) hold. Four bits remain unimplemented, and **three of them are
one feature**: `ReportsOwnTraces` (`0x0020`), `ReportsOwnMetrics` (`0x0040`), and `ReportsOwnLogs`
(`0x0080`), all `[Beta]`. Taking them closes the largest remaining gap in one decision and leaves
exactly one bit undone — `AcceptsOtherConnectionSettings`, which cannot be honoured without
inventing semantics the protocol deliberately leaves to the Agent.

**The protocol says what to send and where.** Each capability means "the Agent can report own
\<signal\> to the destination specified by the Server via `ConnectionSettingsOffers.own_*`"
([`opamp.proto:744-756`](../../crates/opamp/proto/v0.19.0/opamp/v1/opamp.proto#L744-L756)). The
destination is a `TelemetryConnectionSettings` whose `destination_endpoint` "MUST be a full URL an
OTLP/HTTP/Protobuf receiver with path", and the Agent "MAY refuse to send the telemetry if the URL
begins with `http://`"
([`opamp.proto:347-375`](../../crates/opamp/proto/v0.19.0/opamp/v1/opamp.proto#L347-L375)). The
Baseline also asks that the `AgentDescription`'s identifying attributes appear in the OTLP Resource,
so the telemetry is attributable to the Agent that produced it, and names process metrics —
"CPU or RAM usage" — as what own metrics are for.

Three forces shape how this is built here.

**"Own" cannot mean the Managed Process's internals.** Upstream's `opampsupervisor` answers the
own-telemetry offer by configuring the Collector's *internal* telemetry to point at the destination.
That road is closed: [ADR-0011](0011-supervisor-mode-hexagonal-core-and-plugins.md) has the Collector
Supervisor pass each config-map entry as its own `--config` and do **no YAML manipulation**, and the
specification's non-goal forbids inventing an abstraction over a Managed Process's configuration
language. What this Client can honestly report is what it observes from outside: its own process, and
the processes it spawned and holds the pids of.

**The signals are not equally shaped, but all three have a subject.** Metrics are the process
metrics of the Client and of every Managed Process. Logs are what the Client already writes to
stderr through `tracing` — a fleet's client logs in one place is the single most useful thing an
operator cannot get today. Traces are the least obvious, and the temptation is to skip them; but the
control loop is already a set of lifecycles with phases and outcomes (`APPLYING` → `APPLIED`/`FAILED`,
`Downloading` → `Installing` → `Installed`/`InstallFailed`, the self-update's stage-prove-switch), and
those are spans, not an instrumentation project.

**OTLP has a reference implementation, and it is the standard's own.** The temptation here is to
encode OTLP by hand: [ADR-0006](0006-proto-vendoring-and-codegen.md) already vendors the Baseline's
schema and compiles it with prost via protox, so vendoring a second schema and POSTing the bytes
with the `reqwest` the Client already carries would need no new crate at all. But OTLP is not this
project's protocol to own. Its wire format, its semantic conventions, and their versioning are
maintained upstream, and `opentelemetry-otlp` is where that maintenance lands — it depends on the
`opentelemetry-proto` crate generated from the very schema the vendoring would copy. A copy in this
repository would be a second protocol to keep in sync, with no Baseline discipline behind it and no
authority to resolve a disagreement.

## Decision

We will implement **all three own-telemetry capabilities** through the **OpenTelemetry Rust SDK**,
exporting **OTLP/HTTP with protobuf bodies** to the destinations the Server offers, and we will
**invent nothing OTLP does not already define** — no second vendored schema, no metric names of our
own.

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
   | Traces | One span per control-loop operation that already has a lifecycle: applying a remote configuration, installing a package, a self-update. Phases become child spans and the existing outcome becomes the span status, so a failed rollout is one trace rather than a log hunt. |

6. **Every Agent's telemetry is attributed to that Agent.** The OTLP Resource carries the
   `AgentDescription`'s identifying attributes — `service.name`, `service.instance.id`, and
   `service.namespace` where set — which is what the Baseline asks for and what makes a Supervisor's
   metrics distinguishable from the Client's own on the same host.

7. **Destinations come only from the Server, and the offer flow is the one that already exists.**
   `own_metrics`, `own_traces`, and `own_logs` are folded, persisted, and applied exactly as the
   `opamp` settings are ([ADR-0014](0014-server-driven-connection-settings.md)): an offer carrying
   only one of them leaves the others alone. There is no destination in `client.toml` — the whole
   point of the capability is that the Server names it. With no destination offered, the Client
   sends nothing and costs nothing.

8. **`https://` or loopback, nothing else.** The Baseline's "MAY refuse" is taken: a destination on
   plain `http://` beyond the loopback interface is refused and reported, because the Resource
   carries identifying attributes and the records carry whatever the Client logs. This mirrors the
   warning ADR-0013 already emits for credentials in cleartext, one step firmer.

9. **The three capabilities are declared unconditionally.** Unlike `OffersPackages` or
   `[client_ca]`, the capability here states an *ability* the Client always has — "I can report to a
   destination you name" — and the Server's offer is what arms it. Declaring it conditionally on a
   destination already being in force would mean the Server could never make the first offer.

10. **`certificate`, `tls`, and `proxy` behave exactly as they do on the OpAMP settings.**
   `TelemetryConnectionSettings` carries the same three fields; the certificate machinery of
   ADR-0035 is reused as-is, and `tls`/`proxy` are refused and *reported* rather than dropped in
   silence.

11. **The Server gains a `[telemetry_offer]` section** — `metrics_endpoint`, `traces_endpoint`,
   `logs_endpoint`, and optional headers per signal — compiled into the same hash-gated
   `ConnectionSettingsOffers` `[connection_offer]` already produces. Without it the Server offers no
   destination, and `OffersConnectionSettings` stays what it is today.

## Alternatives considered

- **Vendor `opentelemetry-proto` and encode OTLP by hand**, exactly as ADR-0006 vendors the
  Baseline's schema. It is the cheaper build — prost, protox, and the `reqwest` this Client already
  carries, with no new crate at all — and it was the shape of an earlier draft of this ADR.
  Rejected: ADR-0006 exists so this project owns the *OpAMP* contract and can prove it matches
  upstream, and none of that machinery would come along. A second copied schema is a second
  protocol to keep in sync, its semantic conventions would be string literals nobody re-checks, and
  the correctness of someone else's standard would become this project's to defend. Owning the bytes
  is right where the bytes are the product; here they are the exhaust.
- **Take the SDK but keep its default features.** Fewer lines in the manifest. Rejected: the
  defaults select `reqwest-blocking-client`, and blocking HTTP inside a tokio runtime is how an
  executor gets starved.
- **Metrics and logs only, traces deferred.** Tempting, because an Agent has no request path and a
  span looks like a stretch. Rejected: the operations that fail in a fleet — a configuration that
  will not apply, a package that will not stay up — are exactly the ones this project already models
  as multi-phase lifecycles with outcomes, so the spans are a mapping of state that exists rather
  than new instrumentation. And a bit left undone here would be undone for a reason that stops being
  true the first time someone debugs a rollout.
- **Configure the Collector's internal telemetry from the Supervisor**, as `opampsupervisor` does.
  It is the richer answer for a Collector, and it is what an operator coming from upstream will
  expect. Rejected: ADR-0011 forbids this Client from touching a Managed Process's configuration,
  and the specification's non-goal forbids inventing an abstraction over it. What the Supervisor can
  report about a process it did not configure is what it can observe, and saying only that is
  honest.
- **A destination in `client.toml`.** Would let own telemetry work against a Server that offers
  none. Rejected: the capability is defined as reporting "to the destination specified by the
  Server", and a locally configured destination is a private extension of the protocol wearing the
  capability's name.
- **Send OTLP over gRPC.** Rejected by the schema: `destination_endpoint` "MUST be a full URL an
  OTLP/HTTP/Protobuf receiver with path".
- **Declare only the capabilities a destination is currently offered for.** Consistent with how
  `OffersPackages` and `AcceptsConnectionSettingsRequest` are declared. Rejected in point 7: those
  describe something the end *has*, this describes something it *can do*, and the conditional
  version deadlocks the first offer.

## Sources / Prior art

- [OpAMP specification — Agent's own telemetry](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  and the vendored schema:
  [the three capability bits](../../crates/opamp/proto/v0.19.0/opamp/v1/opamp.proto#L744-L756),
  [`TelemetryConnectionSettings`](../../crates/opamp/proto/v0.19.0/opamp/v1/opamp.proto#L347-L375),
  [the `own_*` offer fields](../../crates/opamp/proto/v0.19.0/opamp/v1/opamp.proto#L521-L545) — the
  OTLP/HTTP/Protobuf requirement, the identifying attributes in the Resource, the process-metrics
  expectation, and the "MAY refuse `http://`" provision.
- [`opampsupervisor`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor)
  — the reference answer, which configures the Collector's own telemetry from the offer. Read as the
  alternative this decision declines, for the reason ADR-0011 states.
- [`opentelemetry-otlp`](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/) and its
  [feature flags](https://lib.rs/crates/opentelemetry-otlp/features) — confirms the feature set point
  1 pins: `http-proto` is the OTLP/HTTP protobuf encoding, `reqwest-client` the async client against
  the default `reqwest-blocking-client`, and `grpc-tonic` the only thing that pulls a gRPC stack in.
- [`opentelemetry-appender-tracing`](https://docs.rs/opentelemetry-appender-tracing/latest/opentelemetry_appender_tracing/)
  — `OpenTelemetryTracingBridge`, the standard layer from `tracing` events to OTLP log records,
  registered on the subscriber registry beside the existing `fmt` layer.
- [`opentelemetry-proto`](https://github.com/open-telemetry/opentelemetry-proto) — the schema, which
  this decision deliberately does **not** copy: `opentelemetry-otlp` already depends on the crate
  generated from it, so it is maintained upstream rather than here.
- [`sysinfo`](https://docs.rs/sysinfo/latest/sysinfo/) — cross-platform process CPU and memory, pure
  Rust over `libc`.

## Consequences

- Positive: three capability bits at once. The Client reaches **15 of 16**, and the only one left is
  `AcceptsOtherConnectionSettings`, which is left undone deliberately and on the record.
- Positive: the fleet's Clients get observable without a second agent on the host — and the Server
  decides where that telemetry goes, which is the same "one place decides" the whole project is for.
- Positive: the wire format and the semantic conventions stay upstream's to maintain. A convention
  that moves arrives as a version bump in `Cargo.toml`, not as a silent divergence nobody diffs.
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
- Follow-ups: whether the Server should also *store* or forward what it is offered a destination for
  is a separate question, and the answer is probably no — the specification's non-goal keeps this
  project out of the telemetry-backend business. `AcceptsOtherConnectionSettings` remains the one
  bit deliberately unimplemented.
