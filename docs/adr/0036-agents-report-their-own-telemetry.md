# ADR-0036: An Agent reports its own telemetry over OTLP/HTTP, from protobuf this project vendors

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

**OTLP is protobuf over HTTP, and this project already speaks both.** [ADR-0006](0006-proto-vendoring-and-codegen.md)
vendors the Baseline's schema and compiles it with prost via protox, with no system `protoc`; the
Client carries `reqwest` for its plain-HTTP transport and its package downloads. The alternative —
the `opentelemetry` SDK with `opentelemetry-otlp` — is the obvious choice in most projects, and it is
a stack of four or five crates with a global provider, a pipeline, and a batching model, of which
this Client would use a handful of counters and a log bridge.

## Decision

We will implement **all three own-telemetry capabilities**, sending **OTLP/HTTP with protobuf
bodies** to the destinations the Server offers, encoded from **protobuf definitions this project
vendors exactly as ADR-0006 vendors the Baseline's** — no OpenTelemetry SDK.

1. **Vendor `opentelemetry-proto`, compile it with protox.** The metrics, logs, trace, common, and
   resource messages land under `crates/opamp/proto/otlp/<version>/` beside the Baseline's own
   schema, pinned and recorded the same way, and `build.rs` compiles them with the same protox
   invocation. Encoding a `ExportMetricsServiceRequest` with prost and POSTing it with reqwest is
   the whole of the client side.

   This is the decision most worth arguing with, so its reasoning is explicit: this project already
   made "vendor the schema, compile it in-tree, own the bytes" its way of speaking a protocol, and
   the SDK's value — a global instrumentation surface, sampling, batching, context propagation — is
   value for an application that instruments *itself broadly*. This Client emits a fixed handful of
   points on a timer. Taking five crates and a global provider to do that inverts the ratio, and it
   would put a second, differently-shaped protobuf toolchain beside the one ADR-0006 chose.

2. **One new dependency: `sysinfo`, for process metrics.** CPU and resident memory for a pid, on
   Linux, macOS, and Windows, is three platform APIs (`/proc`, `task_info`, `GetProcessMemoryInfo`)
   and is exactly the kind of thing not to hand-roll three times. Pure Rust over `libc`; no C
   toolchain, no cmake.

3. **What each signal carries.**

   | Signal | What the Client sends |
   |---|---|
   | Metrics | Process metrics per Agent: `process.cpu.time`, `process.memory.usage`, `process.uptime`, following the semantic conventions the Baseline points at. The Client's own Agent reports the Client's process; each Supervisor-backed Agent reports its Managed Process — which this Client already owns the pid of, so nothing new has to be discovered. |
   | Logs | The Client's own `tracing` output, bridged to OTLP log records at the level the log filter already selects. Stderr keeps everything it prints today; this adds a destination, it does not move one. |
   | Traces | One span per control-loop operation that already has a lifecycle: applying a remote configuration, installing a package, a self-update. Phases become child spans and the existing outcome becomes the span status, so a failed rollout is one trace rather than a log hunt. |

4. **Every Agent's telemetry is attributed to that Agent.** The OTLP Resource carries the
   `AgentDescription`'s identifying attributes — `service.name`, `service.instance.id`, and
   `service.namespace` where set — which is what the Baseline asks for and what makes a Supervisor's
   metrics distinguishable from the Client's own on the same host.

5. **Destinations come only from the Server, and the offer flow is the one that already exists.**
   `own_metrics`, `own_traces`, and `own_logs` are folded, persisted, and applied exactly as the
   `opamp` settings are ([ADR-0014](0014-server-driven-connection-settings.md)): an offer carrying
   only one of them leaves the others alone. There is no destination in `client.toml` — the whole
   point of the capability is that the Server names it. With no destination offered, the Client
   sends nothing and costs nothing.

6. **`https://` or loopback, nothing else.** The Baseline's "MAY refuse" is taken: a destination on
   plain `http://` beyond the loopback interface is refused and reported, because the Resource
   carries identifying attributes and the records carry whatever the Client logs. This mirrors the
   warning ADR-0013 already emits for credentials in cleartext, one step firmer.

7. **The three capabilities are declared unconditionally.** Unlike `OffersPackages` or
   `[client_ca]`, the capability here states an *ability* the Client always has — "I can report to a
   destination you name" — and the Server's offer is what arms it. Declaring it conditionally on a
   destination already being in force would mean the Server could never make the first offer.

8. **`certificate`, `tls`, and `proxy` behave exactly as they do on the OpAMP settings.**
   `TelemetryConnectionSettings` carries the same three fields; the certificate machinery of
   ADR-0035 is reused as-is, and `tls`/`proxy` are refused and *reported* rather than dropped in
   silence.

9. **The Server gains a `[telemetry_offer]` section** — `metrics_endpoint`, `traces_endpoint`,
   `logs_endpoint`, and optional headers per signal — compiled into the same hash-gated
   `ConnectionSettingsOffers` `[connection_offer]` already produces. Without it the Server offers no
   destination, and `OffersConnectionSettings` stays what it is today.

## Alternatives considered

- **Use the `opentelemetry` SDK with `opentelemetry-otlp`.** The conventional answer, and it would
  bring batching, retry, and the semantic conventions for free; its default features are now
  `http-proto` with a blocking reqwest client and no gRPC, so it would not drag tonic in. Rejected in
  point 1 — five crates and a global provider for a fixed handful of points, beside a protobuf
  toolchain this project already owns. Worth revisiting if own telemetry ever grows into general
  instrumentation, which is a different decision from this one.
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
  [feature flags](https://lib.rs/crates/opentelemetry-otlp/features) — the SDK route, including that
  `http-proto` with a blocking reqwest client is now the default and needs no gRPC stack. Evaluated
  and declined in point 1.
- [`opentelemetry-proto`](https://github.com/open-telemetry/opentelemetry-proto) — the schema this
  decision vendors, the same way [ADR-0006](0006-proto-vendoring-and-codegen.md) vendors the
  Baseline's.
- [`sysinfo`](https://docs.rs/sysinfo/latest/sysinfo/) — cross-platform process CPU and memory, pure
  Rust over `libc`.

## Consequences

- Positive: three capability bits at once. The Client reaches **15 of 16**, and the only one left is
  `AcceptsOtherConnectionSettings`, which is left undone deliberately and on the record.
- Positive: the fleet's Clients get observable without a second agent on the host — and the Server
  decides where that telemetry goes, which is the same "one place decides" the whole project is for.
- Positive: no second protobuf toolchain, and the vendored OTLP schema is pinned and diffable like
  the Baseline's, so drift is detected rather than discovered.
- Negative / trade-offs: hand-built OTLP payloads mean this project owns their correctness. A
  receiver that rejects a malformed record is the failure mode, and the mitigation is tests that
  decode what was sent — not the SDK's reputation.
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
