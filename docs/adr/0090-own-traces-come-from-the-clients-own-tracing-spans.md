# ADR-0090: Own traces come from the Client's own `tracing` spans, and a span is a fleet operation

- **Status:** 🟢 accepted
- **Date:** 2026-08-21
- **Deciders:** Markus Brigl

Implements what [ADR-0036](0036-agents-report-their-own-telemetry.md) point 5 promised for the third
signal and never named a mechanism for. It supersedes nothing: ADR-0036's table already says *what* a
trace is worth carrying, this says *how* one comes into existence and *which* operations become one.

## Context

**The traces half of ADR-0036 is built everywhere except where spans are made.** The capability is
declared, the offer is folded and persisted ([ADR-0086](0086-a-telemetry-destination-is-an-offer-of-its-own-class.md),
[ADR-0089](0089-an-own-telemetry-offer-states-all-three-destinations.md)), `telemetry.rs` builds the
`SpanExporter`, registers the tracer provider globally, and shuts it down on withdrawal — and the
development stack provisions a dashboard for the result. What is missing is the first line of the
pipeline: **no code in this Client creates a span.** `grep` for `#[instrument]`, `span!`, or a tracer
outside `telemetry.rs` finds nothing, and `.devcontainer/OBSERVABILITY.md` already says so in as many
words. A Server that offers `own_traces` today gets an exporter that has nothing to export.

**Nothing that is already linked would make a span, either.** `opentelemetry-appender-tracing` — the
bridge ADR-0036 point 4 chose — converts `tracing` **events** into OTLP log records and states the
boundary itself: *"This crate does not convert `tracing` spans into OpenTelemetry spans. Use
`tracing-opentelemetry` for that."* So an `#[instrument]` written today would reach stderr and the
log file of [ADR-0041](0041-the-client-logs-to-a-file-in-service-mode.md), and reach the OTLP
destination never. The gap is a missing producer, not missing instrumentation, and instrumenting
before deciding the producer would produce nothing.

**The one-store decision rests on a join that has no left side.** `.devcontainer/OBSERVABILITY.md`
justifies collapsing Tempo, Loki and Prometheus into ClickHouse with cross-signal questions —
*"which log lines belong to the operation that failed"* is a join between `otel_logs` and
`otel_traces` on `TraceId`. Every log record this Client exports today carries a zero `TraceId`,
because a log record takes its trace context from the span in force and there is never one in force.
The store is shaped for a correlation the Client cannot currently supply.

**Two mechanisms exist in Rust, and the choice between them stopped being awkward recently.**

- The OpenTelemetry trace API directly: `global::tracer(…)`, a `Context` threaded through the code
  that is being measured.
- `tracing-opentelemetry`: a `tracing-subscriber` layer that turns the `tracing` spans a program
  already writes into OpenTelemetry spans and hands them to the SDK's provider. Version `0.33.0`
  (2026-05-18) is the one built against `opentelemetry`/`opentelemetry_sdk` `0.32`, which is what
  this workspace runs; the crate is deliberately numbered one release ahead of the OTel crates it
  binds to.

The awkwardness that used to sit between them was correlation. `opentelemetry-appender-tracing` takes
a log record's trace context from the active OpenTelemetry `Context`, which a `tracing` span did not
populate — the reason the appender carried an `experimental_use_tracing_span_context` feature, and
the reason its interaction with `tracing-opentelemetry` produced a run of bug reports
(`opentelemetry-rust` [#1378](https://github.com/open-telemetry/opentelemetry-rust/issues/1378),
[#2803](https://github.com/open-telemetry/opentelemetry-rust/issues/2803),
[#2824](https://github.com/open-telemetry/opentelemetry-rust/issues/2824)). That feature is **gone**
as of the `0.32` the workspace already depends on, and its changelog says why:

> Remove the `experimental_use_tracing_span_context` since `tracing-opentelemetry` now supports
> activating the OpenTelemetry context for the current tracing span. This fixes
> [#3190](https://github.com/open-telemetry/opentelemetry-rust/issues/3190) — the circular dependency
> introduced by depending on `tracing-opentelemetry` that depends on `opentelemetry`.

`OpenTelemetryLayer::with_context_activation` is the switch that does it, and it is *on by default*:
entering a `tracing` span attaches its OpenTelemetry context, so the appender finds it. The two
crates now compose without a feature flag, an experiment, or a workaround — which is what makes this
decision a small one today and would have made it a bad one a year ago.

**The reference implementation had the identical gap and closed it.** `opampsupervisor` shipped the
`reports_own_traces` capability and its exporter before it emitted anything;
[contrib#38724](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/38724)
("Emit spans via trace exporter") asked for spans around *"the start of the supervisor"*, *"handling
messages from the agent"*, *"handling messages from the server"*, and *"applying a new config and
restarting the collector"*, and was closed by
[contrib#38797](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38797) with
spans named `GetBootstrapInfo`, `onMessage`, `handleAgentOpAMPMessage` and
`processRemoteConfigMessage`, whose outcomes are recorded with `span.SetStatus`. Two of those four
are message-handling spans, and that is the one part of it this ADR does not follow — see clause 4.

## Decision

We will produce own traces from the **`tracing` spans this Client already writes**, through
`tracing-opentelemetry`, and treat a span as a **fleet operation with a lifecycle and an outcome** —
not as a unit of message handling.

1. **`tracing-opentelemetry` is the producer.** It joins the three OTel crates ADR-0036 chose, at the
   version matched to them (`0.33` against `opentelemetry` `0.32`), and it is the only new
   dependency this decision adds. The reasoning of ADR-0036 point 1 carries over unchanged: the
   standard's own implementation, not a second copy of its schema.

2. **The layer lives in a reload slot, exactly as the log bridge does.** `tracing` takes one
   subscriber per process and `main` installs it before any destination is known, so the span layer
   is held open from the start and filled when an offer puts a tracer provider in force — and
   emptied when the destination is withdrawn (ADR-0089). This is the mechanism `telemetry.rs`
   already runs for logs, for the same reason, and it keeps the cost of an un-offered destination at
   what a `tracing` span costs when no layer is interested in it.

3. **Instrumentation is written in `tracing`, never in OpenTelemetry.** `#[instrument]` and
   `tracing::span!` in the modules being measured; no OTel type outside `telemetry.rs`. An outcome
   becomes a span status through the crate's reserved fields — `otel.status_code` and
   `otel.status_description` — so no status vocabulary of this project's own is invented, in keeping
   with ADR-0036 point 2.

4. **These five operations are spans, and their phases are child spans.** Each already has a
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

   The names are this project's vocabulary, not a semantic convention. OpenTelemetry defines none for
   an agent's own lifecycle, and ADR-0036 point 2 binds names to the standard *where the standard has
   one* — which is why the metric names are semconv's and these are not. Stated here so that nobody
   later "corrects" them towards a convention that does not cover this.

   Two of the five are narrower than the table reads, and the implementation says so rather than
   forcing the table:

   - **`self.update` is a root only in principle.** A self-update is always reached from a package
     offer, so in practice it is a child of the `package.install` that downloaded the artifact. That
     is the better shape — one trace answers *"did this Client update itself"* from the download to
     the commit — and nothing about it is a separate decision, so it is recorded here rather than
     given a clause of its own.
   - **`connection.settings.apply` has two phases, not three.** `verify` and `store` are spans;
     the reconnection is a **field** on the root span, because it happens after the operation
     returns, in the transport loop that owns the connection. A span for it would measure nothing.

5. **What is deliberately not a span:** the transport exchange (a poll cycle, a WebSocket receive),
   the metrics sampler tick, and the Supervisor Endpoint's message handling. They have no outcome to
   carry and no end, and at one exchange per Agent per interval — the Baseline's default is 30 s —
   they would be an unbounded stream that buries five real operations in the same dashboards, whose
   *"operation rate"* and *"which phase fails most"* panels are computed over all spans. This is a
   deliberate divergence from `opampsupervisor`, which spans `onMessage` and
   `handleAgentOpAMPMessage`: that supervisor is one process managing one Collector, where message
   handling *is* the work; a Client here multiplexes n Agents over one connection
   ([ADR-0003](0003-client-modes-and-connection-multiplexing.md)) and runs on every host of a fleet.
   A failed exchange stays what it is today — a logged warning, now carrying the trace context of
   whatever operation was in flight.

6. **The self-update trace crosses the restart.** The trace id and the root span id go into the
   `UpdateMarker` that ADR-0020 already writes, and the process that comes up after the restart
   continues that trace: `commit` or `roll_back` is a child of the span that staged the version. Without
   this the trace ends one line before the part that fails, which is the part the trace exists for. A
   marker written by an older Client carries no ids; the post-restart span then opens its own trace
   rather than failing.

7. **No sampler.** Always-on, because the volume is fleet operations and not requests — a handful per
   host per day. A sampler here would be volume management for a volume that does not exist, and the
   first thing it would drop is the rare failed rollout the trace was built for.

8. **The logs gain their trace context, and that is part of this decision.** With clause 2 in force,
   every log record the appender exports from inside an instrumented operation carries that
   operation's `TraceId` and `SpanId`. The join `.devcontainer/OBSERVABILITY.md` promises becomes
   answerable, and the dashboard's trace-detail view can reach the lines that explain a failure.

9. **A span attribute is data leaving the host, and is named one by one.** The same discipline
   `telemetry.rs` applies to the Resource's descriptive attributes applies here: attributes are
   chosen individually — a Supervisor name, a package name and version, a configuration hash, a
   count — and never a whole configuration, a URL with credentials in it, or an offer's headers. The
   `Debug` impl that already hides package header values exists for this reason and must not be
   defeated by a span field.

## Alternatives considered

- **Use the OpenTelemetry trace API directly and add no dependency.** Attractive on the dependency
  count, and it is what the reference implementation does (in Go, where the context is threaded
  through every call anyway). Rejected: it puts OTel types and an explicit `Context` into
  `reconfigure`, `packages`, `selfupdate` and the Supervisor plugins — modules whose whole shape is
  domain logic with `tracing` on top — and it would leave log correlation to be attached by hand at
  every site instead of once by a layer. The dependency buys the separation ADR-0011's hexagonal core
  is built on.

- **Drop the promise: keep metrics and logs, stop declaring `ReportsOwnTraces`.** Honest, and cheaper
  than this ADR. Rejected because the operations really are multi-phase lifecycles with outcomes —
  ADR-0036 already argued this, and nothing since has weakened it — and because the withdrawal would
  have to be real: the capability bit, the offer path, the dashboard and the Server's
  `traces_endpoint` would all have to go. That is more work than instrumenting five operations, and
  it removes the one signal that answers *why* a rollout failed rather than *that* it did.

- **Span every message, as `opampsupervisor` does.** It would make our traces directly comparable to
  the ecosystem's, which [ADR-0040](0040-interoperability-against-opamp-go.md) generally favours.
  Rejected on the ground stated in clause 5: ADR-0040 makes `opamp-go` the oracle for *protocol
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

- **[`tracing-opentelemetry` 0.33.0](https://docs.rs/tracing-opentelemetry/0.33.0/tracing_opentelemetry/)**
  — `OpenTelemetryLayer`, the reserved `otel.name` / `otel.kind` / `otel.status_code` /
  `otel.status_description` fields, and `with_context_activation` (*"entering a span will activate its
  OpenTelemetry context, making it available to other OpenTelemetry instrumentation … By default,
  context activation is enabled"*). Also the version rule that puts `0.33` against `opentelemetry`
  `0.32`.
- **`opentelemetry-appender-tracing` 0.32.0** — its own statement that it does not convert `tracing`
  spans, and the `CHANGELOG` entry removing `experimental_use_tracing_span_context` because
  `tracing-opentelemetry` now activates the context (fixing the circular dependency of
  [#3190](https://github.com/open-telemetry/opentelemetry-rust/issues/3190)). Read from the vendored
  crate source rather than a summary.
- **`opentelemetry-rust` issues [#1378](https://github.com/open-telemetry/opentelemetry-rust/issues/1378),
  [#2803](https://github.com/open-telemetry/opentelemetry-rust/issues/2803),
  [#2824](https://github.com/open-telemetry/opentelemetry-rust/issues/2824)** — the history of
  log/trace correlation between these two crates, and the reason to state plainly *which* versions
  this decision depends on.
- **`opampsupervisor`
  ([contrib#38724](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/38724),
  [contrib#38797](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38797))** —
  the same gap in the reference implementation, the operations it chose to span, and its use of
  `span.SetStatus` for outcomes. The prior art this ADR follows on structure and departs from on
  message handling.
- **[ADR-0036](0036-agents-report-their-own-telemetry.md)** — points 1, 2, 4 and 5: the SDK choice,
  names from the standard, the log bridge, and the table that promised *"one span per control-loop
  operation … phases become child spans and the existing outcome becomes the span status"*.
- **[ADR-0041](0041-the-client-logs-to-a-file-in-service-mode.md)** — the second `fmt` layer, which is
  where the visible change to log lines lands.
- **[ADR-0020](0020-client-self-update.md)** — the `UpdateMarker` and the split across the restart
  that clause 6 rides on.

## Consequences

- **Positive: the third signal starts existing.** The provisioned dashboard shows real operations
  instead of what `send-test-telemetry.py` puts there, and a failed rollout is one trace with a
  failing phase in it rather than a hunt through a day of log lines.

- **Positive: the logs become joinable.** Trace context on exported log records is what
  `.devcontainer/OBSERVABILITY.md` assumed when it collapsed three stores into one; after this the
  assumption holds.

- **Positive: instrumentation is free when nobody asked for it.** With no destination offered the
  slot is empty, and an `#[instrument]` is a `tracing` span no layer subscribes to.

- **Negative: a fourth OTel crate, on a release train of its own.** `tracing-opentelemetry` is
  numbered one ahead of the OTel crates and released after them, so a future `opentelemetry` bump has
  a second cadence to wait for. That is a real maintenance cost and it is why the version pairing is
  written into clause 1 rather than left to `cargo update`.

- **Negative: stderr and the log file change shape.** The `fmt` layers print the enclosing span's name
  and fields on every event inside an instrumented operation, so lines an operator knows will gain a
  `package.install{package=…}:` prefix. Better context, different output — including in the ADR-0041
  file, which is read with a pager and by whatever the operator greps with.

- **Negative: a second surface where data leaves the host.** Clause 9 states the discipline, but it is
  a discipline: a span field is as easy to add as a log field and reaches a destination the *Server*
  named. This is the same exposure the Resource attributes have, now spread across five modules
  instead of one function.

- **Negative: the offer that switches tracing on is itself untraced.** The span of the
  `connection.settings.apply` that installs the exporter is created before the exporter exists, so
  that one apply is missing from the destination it just configured. It cannot be otherwise — there
  is nothing to export to until that operation is half done — and it corrects itself on the next
  start, where the persisted settings put the exporter in force before anything runs. Worth knowing
  before somebody reads the gap as a bug.

- **Negative: the update marker becomes a telemetry carrier.** It is an operational file with a safety
  role, and clause 6 puts two ids in it that have nothing to do with that role. The fallback keeps an
  old marker working, but the file now has two reasons to change.

- **Follow-ups:** whether Gateway Mode should carry trace context across the hop — it forwards
  messages unchanged and speaks in no Agent's name ([ADR-0037](0037-gateway-mode.md)), so relating a
  downstream Agent's operation to the Gateway's own would be a new decision, not an implementation
  detail of this one. And whether a Server should be able to ask for less than everything (a sampling
  hint in the offer) if a large fleet ever makes clause 7 wrong; the Baseline offers no field for it
  today.
