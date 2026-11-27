# CLT-what-an-agent-reports: What an Agent reports about itself

- **Status:** 🟢 accepted
- **Date:** 2026-11-14
- **Deciders:** Markus Brigl
- **Applies to:** AgentDescription building and the fold of a Managed Process's description in `crates/fleet-agent/src/supervisor/agent.rs` and `crates/fleet-agent/src/supervisor/agent/description.rs`, the host attributes in `crates/fleet-agent/src/host.rs`, the Agent type resolution in `crates/fleet-agent/src/supervisor/block.rs` and `crates/fleet-agent/src/supervisor/mod.rs`, the attribute keys in `crates/opamp/src/attributes.rs`, `[attributes]`, `service_namespace` and a `[[supervisor]]` block's `service_name` in `supervisor.toml`, the fleet view's name and network columns; `crates/fleet-agent/src/telemetry.rs`, the telemetry half of `crates/fleet-agent/src/connection.rs`, the operation spans in the Client's transport, engine, reconfigure, packages, update (`crates/fleet-agent/src/update/`) and Supervisor modules, the Server's `[telemetry_offer]` (`crates/fleet-server/src/config.rs`, `crates/fleet-server/src/fleet.rs`), the OpenTelemetry crates in `Cargo.toml`, and every document that tells an operator where an Agent's type comes from or what a Managed Process or `[attributes]` contributes to an Agent's description

## Context

Everything a Selector matches, every artifact a Package fit chooses, and every row of the fleet
view comes from the `AgentDescription` an Agent reports. What goes into it is therefore a
contract.

**The Baseline reserves `service.name` for the Agent type.** It "should be set to a reverse FQDN
that uniquely identifies the Agent type, e.g. `io.opentelemetry.collector`", and names
`service.instance.id` separately as what identifies the Agent. A `[[supervisor]]` block's `name`
is the operator's name for one instance — a directory, a uniqueness key — and putting it into
`service.name` would give one key two meanings. A Collector carrying the `opampextension` reports
its own description, and its `service.name` is the `dist.name` it was built with
(`otelcol-contrib`), identical on every host. An operator's name kept in that key would be erased
the moment the extension connects, and three Collectors would become three rows of the same name.
The Baseline has no attribute for a human-readable instance name; it admits "any user-defined
attributes the end user would like to associate with this Agent" among the non-identifying ones.
Bindplane, the closest shipping server, derives its Agent Type from `dist.name` and keeps the
human name in a separate field.

**The Server decides by the type.** A Configuration fits an Agent whose reported `service.name`
equals the Configuration's type, and a rollout pins the fitting ones to it. A Deployment's
Selector can aim at `service.name`. A Package is resolved for the type the Agent reports. The
Supervisor refuses an offered Package whose name is not its own type before anything is
downloaded, and keeps the high-water mark per its own type
([PKG-versions-high-water-mark-floor](PKG-versions-high-water-mark-floor.md)); both read the
Supervisor's value.

**A Managed Process is code the Supervisor does not vouch for.** The Server ships its program as a
Package and its configuration as Configurations, and a Configuration's language can run commands,
such as an entry that runs a program, or whatever the configuration a `command` Agent's program
reads makes it run. Every Managed Process is handed the Supervisor Endpoint's token
([CLT-modes-and-gateway](CLT-modes-and-gateway.md)), and so is every process it starts, through its
environment: a command its configuration names, a task its own server sends, a script a `command`
Agent runs.
Any of them can connect to the Endpoint and report a description. Whatever it reports reaches the
Server as the Supervisor's own statement, and the Server cannot tell the two apart. Admitted
unfiltered, such a report could state:

- **Another type.** The next rollout that claims the Agent would pin that type's Configurations to
  it, with whatever credentials they carry, and the Supervisor would write them into its
  `config/`. A Selector on `service.name` would claim it for another Deployment, and the fleet
  view and the telemetry label would show the false type.
- **Another host.** Selectors match the effective description, and a reported attribute beats
  every Label. A process on host B that reports `host.name` or `host.id` of host A is matched by a
  Deployment aimed at host A.
- **Another platform.** `os.type` and `host.arch` choose the artifact a Package offers. A process
  that reports another platform is offered a binary built for another machine, which the
  Supervisor swaps over the program; only the health gate would catch it.
- **A Client Agent.** The Client Agent is recognised by the `supervisor.kind.*` attributes that it
  alone reports ([SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md)). A process that
  reports one makes a second Client Agent in its group: nobody in the group inherits Labels, and
  every Agent of the host is marked ambiguous and drops out of every Deployment aimed by an
  inherited Label. Behind a Gateway the group is keyed by the reported `host.id` or `host.name`, so
  a process that also reports another machine's `host.id` joins that machine's group.
- **The operator's values.** An `[attributes]` entry such as `channel` would be overwritten, and a
  Label is shadowed by any key the Agent reports, so a process reporting `rollout = stable` would
  leave the canary channel its Label put it in.

What the process knows that nobody else does is its version, and for a Collector its `dist.name`.
The rest of what a Collector's `opampextension` reports is `host.name`, `os.type`, `host.arch` and
`os.description`, which the Supervisor reports from the same machine in the same spelling
([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)), plus attributes written
in the Collector's own configuration under `agent_description.non_identifying_attributes` and its
resource attributes. G-16 has the Supervisor Endpoint relay a Collector's `service.*` description
and leaves what the host is to the Supervisor and the operator. No kind this project builds states a
type of its own; `collector` and `command` take the block's `service_name` or fall back to the
program's file name.

**Where the Agent runs** is described with the semantic conventions' `os.*` and `host.*` keys. An
operator also wants to see which addresses a host holds. The connection's peer address is the
wrong fact: behind NAT it is the translator's, behind a Gateway the Gateway's, and a hop must not
become the source of an attribute about someone else's host. The conventions define `host.ip` and
`host.mac` as string arrays, "excluding loopback interfaces", IPv6 in RFC 5952 form and MACs "in
IEEE RA hexadecimal form: as hyphen-separated octets in uppercase".

**Beside its description, an Agent reports its own telemetry.** The
[specification](../SPECIFICATION.md) puts security before convenience (Strategy *Security before
convenience*, Q-1 *Secure by default*): every connection that leaves the host is TLS 1.3, plaintext
is accepted on the loopback alone, and a configuration that breaks this is refused rather than
warned about. Three capability bits of the Baseline are one feature: `ReportsOwnTraces`
(`0x0020`), `ReportsOwnMetrics` (`0x0040`) and `ReportsOwnLogs` (`0x0080`), all `[Beta]`. Each
means "the Agent can report own \<signal\> to the destination specified by the Server via
`ConnectionSettingsOffers.own_*`". The destination is a `TelemetryConnectionSettings` whose
`destination_endpoint` "MUST be a full URL an OTLP/HTTP/Protobuf receiver with path", and the
Agent "MAY refuse to send the telemetry if the URL begins with `http://`"
([`opamp.proto`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto)). The Baseline asks that
the `AgentDescription`'s identifying attributes appear in the OTLP Resource, and names process
metrics — "CPU or RAM usage" — as what own metrics are for.

**"Own" cannot mean a Managed Process's internals.** Upstream's `opampsupervisor` answers the offer
by configuring the Collector's internal telemetry. That road is closed here: a Supervisor passes a
Configuration through without touching it
([CLT-wrappers-act-through-the-core](CLT-wrappers-act-through-the-core.md)), and the specification's
non-goals forbid an abstraction over a Managed Process's configuration language. What this Client
can honestly report is what it observes from outside: its own process, and the processes it spawned
and holds the pids of.

**All three signals have a subject.** Metrics are process metrics. Logs are what the Client
already writes through `tracing`. Traces are the control loop's lifecycles — a configuration
apply, a Package install, a self-update — which have phases and an outcome the Server is told.
Making them needs a producer: the log bridge converts `tracing` events and says so itself ("This
crate does not convert `tracing` spans into OpenTelemetry spans. Use `tracing-opentelemetry` for
that."). `tracing-opentelemetry` `0.34` is built against the `opentelemetry` `0.33` this workspace
runs, and with it entering a `tracing` span activates its OpenTelemetry context, so the log
appender picks up the trace context with no feature flag.

**OTLP has a reference implementation, and it is the standard's own.** This project vendors the
OpAMP schema because it owns that wire contract
([API-protocol-pinned-interop-checked](API-protocol-pinned-interop-checked.md)). OTLP is not its
protocol: its wire format, semantic conventions and versioning are maintained upstream, and
`opentelemetry-otlp` is where that maintenance lands.

**Cleartext stops at the loopback.** The Resource carries identifying attributes and the log
records carry whatever the Client logs, as a continuous stream. A destination the Server offers is
configuration this Client puts in force, so the specification's rule reaches it. The private
address space — [RFC 1918](https://www.rfc-editor.org/rfc/rfc1918)'s three IPv4 ranges and
[RFC 4193](https://www.rfc-editor.org/rfc/rfc4193)'s `fc00::/7` — is not routable across the
public internet, but a stream sent into it crosses every switch, VLAN and tenant between the host
and the Collector. A name is not an address: `localhost` resolves wherever the host's resolver
says. [DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md) draws the same line for the OpAMP
connection, at the literals `127.0.0.1` and `::1`, and builds every rustls configuration for TLS 1.3
alone.

**The Baseline cannot switch own telemetry off.** For each `own_*` field the schema says "If this
field is not set then the Agent should assume that the settings are unchanged", and the empty
string is not a legal `destination_endpoint`. Read literally, a destination once in force can be
moved but never ended, short of deleting state on each host. The one widely deployed Client that
implements these fields, `opampsupervisor`, reads them otherwise: it builds its own-telemetry
section from each received message alone, drops a signal whose endpoint is absent or empty, and
logs "Disabling own telemetry pipeline in the config" when nothing is left — but only enters that
path when the message names at least one of the three. `opamp-go` is this project's behavioural
oracle ([API-protocol-pinned-interop-checked](API-protocol-pinned-interop-checked.md)); the literal
reading would leave both ends unable to drive each other.

How an offer is applied, acknowledged and persisted — the telemetry destinations as a class of
offer of their own, without a verifying connection or a reconnect — is
[SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md)'s. This ADR
decides what an Agent states about itself, what own telemetry is, what an offer of it means, and
what the Client admits.

**The `supervisor.` namespace is the Supervisor's own statement.** A Package is matched against
the version of the Package a Supervisor installed
([SRV-deployments-and-pinned-rollouts §25](SRV-deployments-and-pinned-rollouts.md)), and the Client
Agent is told apart by the kinds it reports
([CLT-wrappers-act-through-the-core §24](CLT-wrappers-act-through-the-core.md)). An operator who
could state
a key in that namespace could make every Agent of the host a Client Agent or choose the version a
Package is held against.

## Decision

We will describe each Agent by the type its Supervisor states, the operator's name for it and the
conventions' `os.*` and `host.*` attributes, admit from a Managed Process only its own `service.*`
attributes, and report the Agent's own telemetry through the OpenTelemetry Rust SDK over OTLP/HTTP
to the destinations the Server offers, in TLS 1.3 and in cleartext only to a loopback IP literal.

1. **`service.name` is the Agent type, identifying, and the Supervisor states it.** It is resolved
   in this order, first hit wins:

   | Source | When |
   |---|---|
   | The kind's own type | a wrapped kind states it, the type of the one agent it wraps; a block of such a kind carrying `service_name` fails at startup naming the derived value ([CLT-wrappers-act-through-the-core](CLT-wrappers-act-through-the-core.md)) |
   | The block key `service_name` | `collector` and `command`; must not be empty, and is not bound by the instance-name grammar, so a reverse FQDN is accepted |
   | The program's file name | the fallback: `otelcol`, `promtail` |

   - The Client reads the fallback from the configuration. It never parses it out of a program's
     output.
   - What the Managed Process reports is never a source of the type (clause 8).
   - The Baseline's reverse-FQDN shape is a recommendation, and the Client does not enforce it.
   - The Client Agent reports the constant `supervisor`
     ([CLT-installed-service-and-state](CLT-installed-service-and-state.md)), never its configured
     name.
   - A Selector or a Configuration's type
     ([SRV-configurations-compose-one-map](SRV-configurations-compose-one-map.md)) on
     `service.name` aims at a type.

2. **`service.instance.name` is the operator's name for the Agent**, non-identifying. It is the
   `[[supervisor]]` block's `name`, or the top-level `name` for the Client Agent. It is always
   reported. The key is this project's, because the conventions have none for a human instance
   name. It is chosen to read as the partner of `service.instance.id`.

3. **The block `name` keeps its local meaning.** It is the Supervisor's directory name and the
   uniqueness key across blocks. It is bound by the instance-name grammar of
   [CLT-installed-service-and-state](CLT-installed-service-and-state.md): lowercase, digits, `-`, no
   dots. A type may be a reverse FQDN; an instance name is a path component on three operating
   systems. That is why the two cannot share a key.

4. **The identifying attributes** are:
   - `service.name`;
   - `service.namespace`, only when the top-level `service_namespace` key in `supervisor.toml` is
     set, since only an operator knows the deployment;
   - `service.version`: for the Client Agent its baked version
     ([OPS-cargo-decides-product-version](OPS-cargo-decides-product-version.md)); for a
     Supervisor-backed Agent only what the Managed Process reports, never invented from the
     Client's;
   - `service.instance.id`, the Instance UID.

5. **Where the Agent runs is reported best effort, and absent rather than blank.**
   - Beside `service.instance.name`, the non-identifying attributes are:
     - `os.type` and `host.arch`, in the conventions' vocabulary
       ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md));
     - `os.name`, `os.version`, `os.build_id` and `os.description`;
     - `host.name`, `host.id`, `host.cpu.model.name`, `host.ip` and `host.mac`.
   - A Supervisor-backed Agent also reports `supervisor.package.version`, the version of the
     Package its Supervisor installed. It reports it when, and from what record,
     [SRV-deployments-and-pinned-rollouts §25](SRV-deployments-and-pinned-rollouts.md) states.
   - The Client leaves out what the platform cannot answer. A placeholder is something false a
     Selector can match.
   - Nothing is taken from the connection.

6. **`host.ip` and `host.mac` follow the conventions.**
   - The Client enumerates them with `sysinfo`'s `network` feature, the crate it already carries
     for its own telemetry. No dependency is added.
   - It excludes whole an interface whose every address is loopback, its MAC too. An all-zero MAC
     is no answer.
   - IPv4 is written dotted-quad, IPv6 in RFC 5952 form (what `IpAddr`'s `Display` writes), MACs
     hyphen-separated in upper case. Both lists are deduplicated and sorted, so enumeration order
     never re-reports an unchanged host.
   - The Client reads them live on every description, so a DHCP move is reported. An empty list
     omits the key.
   - On the wire each is an `ArrayValue` of strings. The Server's view joins a string array with
     `, `. Selectors match string values only, so an array is displayed and searched, never
     matched.

7. **`host.cpu.model.name` and `os.build_id` come from sources already paid for.**
   - The CPU model comes from the same `sysinfo`, read once per process.
   - The build behind `os.version` comes from what the platform's `os.*` answer already carries:
     - os-release's `BUILD_ID`, absent where a distribution stamps none;
     - `sw_vers`' BuildVersion;
     - on Windows, the build components of the version line (`10.0.26100.2033` → `26100.2033`).

8. **A Managed Process's own description is folded in for its `service.*` attributes alone, never
   for who the Agent is, what type it is or where it runs.**
   - The Client merges what the process reports, through the Supervisor Endpoint or the version
     probe, per attribute. A later report wins on the same key.
   - From the merged report, the Client lays a key over the Supervisor's description only when it
     starts with `service.` and is none of these:
     - `service.name`. The type is the Supervisor's (clause 1). A process that chose it would
       choose which type's Configurations are pinned to its Agent and which Deployment claims it.
     - `service.instance.id` and `service.instance.name`. They are the Supervisor's. A process
       cannot know the Supervisor's identity or what the operator called it.
     - A key whose value the operator set: `service.namespace` where the top-level
       `service_namespace` is set, and every key that the `[attributes]` table puts into the
       description (clause 10).
   - The Client drops every other key the process reports. This covers every `host.*`, `os.*` and
     `supervisor.*` key, `supervisor.kind.*` included, and every key in a namespace of its own.
   - Where the Agent runs is the Supervisor's statement. A process's statement about the host is a
     claim nobody checks, and it decides which Selectors match and which artifact is offered.
   - The rule applies **by key and in both lists**, whichever list the process reports a key in:
     - An admitted key replaces the Supervisor's value in the list the Supervisor states it in.
     - Only where the Supervisor states no such key is it added to the list the process reported
       it in.
     - A key the process reports in both lists is added once, among the identifying attributes,
       with its identifying value, because the Server reads `service.version` from there.

     A key is therefore reported once, and a Selector and the fleet row read the same value.
   - The process's `service.version` is the Agent's version (clause 4).

9. **A type the process reports that differs from the Supervisor's is named in a warning.**
   - When the merged report carries a `service.name` that is not the Agent's type, the Client logs
     a warning. It names the Supervisor, both values and the block key `service_name`. That key is
     how an operator who wants a Collector's `dist.name` as the type sets it.
   - The Client warns when the reported value changes to one that differs from the type, not for
     every report. A process that keeps reporting one value is named once, and one that alternates
     is named at each change.
   - The warning quotes the reported value escaped and cut short. A process can thus neither forge
     a log line nor flood the log with one.
   - The Agent stays healthy. A Collector whose `dist.name` differs from its file name runs under
     the Supervisor's type.
   - A process that reports the Supervisor's type, or no `service.name`, causes no warning.

10. **Operator attributes describe the host, and a Managed Process never overrides one.**
    - The Client-wide `[attributes]` table maps string to string; any other value type is refused.
      The Client reports it as non-identifying attributes of every Agent it presents.
    - The Client adds each attribute only where the code reports nothing under the same key. What
      the code reports always wins, so an attribute can never restate the type or the instance
      name.
    - An attribute that is added stands against the Managed Process: the fold leaves its key alone
      (clause 8).
    - Tagging one Agent among several is a Label's job
      ([SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md)). A `[[supervisor]]`
      block carrying an `attributes` table fails at startup with a message naming the Server's
      Label.
    - The Client Agent also reports the kinds it was compiled with
      ([CLT-wrappers-act-through-the-core](CLT-wrappers-act-through-the-core.md)).
    - An `[attributes]` key that starts with `supervisor.` fails at startup with a message naming
      the key. That holds whether TOML spells it quoted, dotted, or as a sub-table under
      `[attributes.supervisor]`.
    - That namespace is the Supervisor's own statement: `supervisor.kind.*`
      ([CLT-wrappers-act-through-the-core §24](CLT-wrappers-act-through-the-core.md)) and
      `supervisor.package.version` (clause 5). An operator able to state one would make every Agent
      of the host a Client Agent, or choose the version a Package is held against.

11. **The fleet view shows the instance name first and the type beneath it.**
    - `AgentView` carries `service_name` (the type) and `service_instance_name`.
    - The name column of the Server's rudimentary UI falls back from instance name to type to
      Instance UID. A row is thus never blank and never collapses onto its neighbours. A
      production UI stays outside this project (specification, Non-Goal *Shipping a production
      UI*).
    - A Network column shows the first address of each kind, with the rest in the tooltip.
    - Every attribute is shown as a chip and is searchable.

12. **The standard's own implementation, not a copy of its schema.** `opentelemetry`,
    `opentelemetry_sdk` and `opentelemetry-otlp` carry the wire format. The exporter's features are
    stated rather than inherited:

    ```toml
    opentelemetry-otlp = { version = "0.33", default-features = false,
                           features = ["http-proto", "reqwest-client", "trace", "metrics", "logs"] }
    ```

    - `reqwest-client` replaces the default blocking client.
    - `grpc-tonic` stays off, because the schema permits no gRPC destination.
    - `tracing-opentelemetry` joins them at the version built against them: `0.34` against
      `opentelemetry` `0.33`. That pairing is a decision, not a range to widen.
    - No OTLP schema is vendored
      ([API-protocol-pinned-interop-checked](API-protocol-pinned-interop-checked.md) covers OpAMP
      only).

13. **Names come from the standard where it has them.**
    - Metric and attribute names are taken from `opentelemetry-semantic-conventions`, with
      `semconv_experimental`, since the process metrics are experimental upstream.
    - They are never written as string literals, so a moved convention is a compile error on the
      next bump.
    - The operation span names of clause 19 are this project's vocabulary, because OpenTelemetry
      defines none for an agent's own lifecycle. A change must not "correct" them towards a
      convention that does not cover them.

14. **"Own" is what the Client observes from outside.**
    - The Client Agent reports the Client's process.
    - Each Supervisor-backed Agent reports its Managed Process, by the pid the Supervisor already
      holds.
    - No Managed Process's configuration is touched to obtain telemetry.
    - CPU and memory for a pid come from `sysinfo` (pure Rust, Linux, macOS and Windows), the one
      gap the SDK leaves.

15. **Metrics are process metrics per Agent, every 10 s.**
    - The Client records `process.memory.usage`, `process.cpu.utilization` and `process.uptime` as
      gauges.
    - Each sample carries the sampled Agent's `service.instance.id`, `service.instance.name` and
      `service.name`. The Resource is the Client's, so a Managed Process's series would otherwise
      carry the Client's identity.
    - Sampling and export share one interval, 10 s, the Baseline's recommended reporting interval.
      The periodic reader is told it explicitly, since its default is 60 s.
    - A pid that has gone away records nothing.

16. **Logs are the Client's `tracing` output, bridged.**
    - `opentelemetry-appender-tracing`'s `OpenTelemetryTracingBridge` turns events into OTLP log
      records, at the level the log filter already selects.
    - Stderr and the service-mode log file
      ([CLT-installed-service-and-state](CLT-installed-service-and-state.md)) keep everything they
      print. The bridge adds a destination.
    - A record written inside an instrumented operation carries that operation's `TraceId` and
      `SpanId`. That makes a log-to-trace join answerable at the receiving end.

17. **Traces come from the `tracing` spans this Client writes.**
    - `tracing-opentelemetry`'s layer converts them.
    - Instrumentation is written in `tracing` (`#[instrument]`, `tracing::info_span!`). No
      OpenTelemetry type appears outside `telemetry.rs`.
    - An outcome becomes the span status through the crate's reserved fields `otel.status_code` and
      `otel.status_description`, written through `telemetry::failed` and `telemetry::succeeded`.
      The failure text is the same string the Server is told.

18. **Both bridges live in reload slots.**
    - `tracing` has one subscriber per process, installed before any destination is known. The log
      bridge and the span layer therefore each sit in a `tracing_subscriber::reload` slot, held
      open from startup.
    - A slot is filled when an offer puts a provider in force, and emptied when the offer is
      withdrawn.
    - A slot is emptied before its provider shuts down, so an event or span during shutdown never
      reaches a closing exporter.
    - With no destination, an instrumented span costs what a `tracing` span costs with no layer
      interested in it.

19. **Five operations are root spans, and their phases are child spans.**

    | Root span | Phases | Opened in |
    |---|---|---|
    | `package.install` | `download`, `verify`, `stage`, `preflight`, `swap`, `gate`, `rollback` | `transport::process_package_downloads` |
    | `config.apply` (Supervisor set) | `validate`, `stop`, `write`, `purge`, `start` | `reconfigure::apply` |
    | `config.apply` (Managed Process) | `reload` or `restart`, `gate` | `engine::handle`, where the Configuration is handed over |
    | `connection.settings.apply` | `verify`, `store` | `transport::process_connection_offer` |
    | `self.update` | `stage`, `probe`, then `commit` or `roll_back` | `update::installer::install` |

    - The task that received the message begins an install or a Managed Process's apply. The
      Supervisor's own task finishes it. The span therefore travels with the command through the
      Port: `ProcessCommand::ApplyConfig` and `ApplyPackage` each carry one. A trace that ended at
      the hand-over would stop where the interesting failures are.
    - A self-update is always reached from a Package offer. `self.update` is thus in practice a
      child of the `package.install` that downloaded it.
    - The reconnection after `connection.settings.apply` is a field on the root span, not a phase.
      It happens after the operation returns, in the transport loop.

20. **Message handling is not a span.**
    - A transport exchange (a poll, a WebSocket receive), a sampler tick and the Supervisor
      Endpoint's message handling have no outcome and no end. At one exchange per Agent per
      interval they would bury the five operations.
    - A span that belongs to an operation is therefore opened inside the per-exchange function,
      where the operation begins, never on the function itself.
    - A failed exchange stays a logged warning, carrying the trace context of whatever operation is
      in flight.

21. **A self-update stays one trace across its restart.**
    - The trace id and the span id of the `self.update` span go into the update marker
      ([CLT-self-update-signed-package](CLT-self-update-signed-package.md)) as hex.
    - The process that comes up after the restart opens `commit` or `roll_back` as a child of that
      span, its parent marked remote.
    - A marker without ids, or with ids that do not parse, makes the post-restart span open its own
      trace. A trace is never a reason to fail an update.
    - `self.update` itself records no status. The process that stages a version does not learn
      whether it stays up.

22. **No sampler.** Every span is exported. The volume is a handful of fleet operations per host
    per day, and a sampler would first drop the rare failed rollout a trace exists for.

23. **Every signal is attributed to its Agent.**
    - The OTLP Resource carries the `AgentDescription`'s identifying attributes.
    - It also carries four descriptive ones, named one by one, each only where the description
      carries it: `service.instance.name`, `os.type`, `host.arch`, `os.description`.
    - No other non-identifying attribute goes on the Resource, such as `host.ip`, `host.mac` or the
      operator's `[attributes]`. Widening that list is a decision about what leaves the host.

24. **A span attribute is data leaving the host, and is chosen one by one.**
    - A span attribute may be a Supervisor name, a Package name and version, a configuration hash,
      or a count.
    - It is never a whole Configuration, an offer's headers, or a URL that can carry a credential.
    - A download source is labelled by scheme, host and path only, its query dropped, because a
      pre-signed URL puts its signature there.
    - A span field must not defeat the `Debug` impls that hide Package header values.

25. **The exporters send through a client bound to this process's runtime, in TLS 1.3, with a
    bounded export.**
    - The SDK exports from dedicated threads that block on the request and have no Tokio reactor.
      Each request is therefore spawned onto the runtime handle captured when the exporter was
      built.
    - The HTTP client is built from this Client's TLS material: its trust anchors, and the
      certificate of clause 33 when one is offered. Without one it presents none, never the fleet
      identity the Server sees.
    - It follows no redirect, so a destination cannot move an export to another host or to
      plaintext.
    - It speaks TLS 1.3 alone, with TLS 1.3 as its minimum and the three TLS 1.3 suites of the ring
      provider, as every rustls configuration of this project does
      ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)). A destination that speaks only
      TLS 1.2 or older fails the handshake, and that export fails.
    - The client carries a 5 s timeout, half the reporting interval. Neither the periodic reader nor
      a client handed to `opentelemetry-otlp` bounds an export. A destination that stops answering
      would otherwise hold the exporter until the Client restarts.

26. **Destinations come only from the Server.**
    - There is no destination in `supervisor.toml`: the capability is reporting "to the destination
      specified by the Server".
    - With no destination offered, nothing is built and nothing is sent.
    - The offer's lifecycle is
      [SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md)'s:
      applied in place, acknowledged on the same offer, persisted, and put back in force at startup.

27. **The three capabilities are declared unconditionally.** They state an ability: "I can report
    to a destination you name". The offer arms it. If the Client declared them only once a
    destination is in force, the Server would never make the first offer.

28. **An offer that names any telemetry destination states all three; one that names none changes
    nothing.**
    - When any of `own_metrics`, `own_traces`, `own_logs` is present, the offer is the whole truth
      about own telemetry. A signal it does not name is stopped and leaves the persisted state. It
      is not carried over.
    - An offer naming none of the three leaves all three as they are. Such an offer carries OpAMP
      settings only, a certificate, or a heartbeat change. That keeps the classes of offer of
      [SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md)
      independent.

29. **An empty `destination_endpoint` withdraws that signal.**
    - The Client admits it and does not refuse it. It shuts the exporter down, removes the
      destination from the persisted state, reports nothing against it, and acknowledges the offer
      `APPLIED`.
    - It is the only way to stop all three, since by clause 28 an offer naming nothing cannot say
      it.
    - The Server sends it as clause 34 describes.

30. **Cleartext is admitted to the loopback and nowhere else; every other destination is TLS
    1.3.**
    - The Client admits an `http://` destination only when its host is one of the loopback IP
      literals `127.0.0.1` and `::1`.
    - It does not admit the rest of `127.0.0.0/8`, RFC 1918's `10.0.0.0/8`, `172.16.0.0/12` and
      `192.168.0.0/16`, RFC 4193's `fc00::/7`, link-local, or carrier-grade NAT. They are named
      here so that "loopback" does not quietly widen.
    - It admits an `https://` destination and speaks to it in TLS 1.3 alone (clause 25).
    - It refuses an endpoint that is neither, as not an OTLP/HTTP URL.

31. **The judgement is made on a parsed address, never on a name.**
    - The Client extracts the host from the URL, an IPv6 literal from its brackets. It parses the
      host as an IP address and compares it with the two loopback literals. It never uses a prefix
      match (`127.0.0.1.example.com` is refused).
    - It admits no name, `localhost` included, and resolves no name. A name can be made to resolve
      anywhere. An admission test that a re-resolve can flip, on a destination that is persisted
      and re-applied, is not one an operator can reason about.
    - A Collector reached by name is put behind TLS.

32. **A refused destination is reported, never warned about or downgraded.**
    - The Client does not build its exporter. The refusal reaches the Server, with the reason, in
      the offer's `connection_settings_status`
      ([SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md)).
    - The cleartext message names the setting and the two loopback literals the Client would
      accept.
    - A refusal, not a warning, is the specification's answer to every plaintext connection off the
      loopback (Q-1). This one is a continuous stream of identifying attributes and log records.

33. **`certificate`, `tls` and `proxy` behave as they do on the OpAMP settings.**
    - The Client honours an offered `certificate`. The exporter presents its `cert`, paired with
      the key this Client generated for its CSR. That is the client-certificate machinery of
      [SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md), reused as-is.
    - The Client refuses a `private_key` in the offer by name. The private key of a host's client
      certificate is made on the host and never leaves it. The Client accepts none from the Server,
      and the Server hands none (specification, *Threats & Forbidden Actions*).
    - It refuses by name a `cert` with no key on disk to pair with.
    - It does not add `ca_cert` to the trust store, as the Baseline advises.
    - It refuses `tls` and `proxy` and reports them by name.

34. **The Server offers destinations from a `[telemetry_offer]` section.**
    - The section holds `metrics_endpoint`, `traces_endpoint`, `logs_endpoint`, and one
      `[telemetry_offer.headers]` table sent with every signal, typically the backend's access
      token.
    - Endpoints are full OTLP/HTTP URLs with path. The Server appends no `/v1/…`.
    - At startup a section naming no endpoint fails loudly. So does an endpoint that is not an
      `http://` or `https://` URL.
    - The Server judges nothing about cleartext. That is the Agent's rule, decided where the
      packets originate.
    - What the section says, it says about all three signals (clause 28).
    - An endpoint set to `""` is a withdrawal to be sent. It carries no headers, and counts as
      something to offer.
    - Removing the section withdraws nothing. A Server cannot tell "never offered" from
      "withdrawn", and must not tear down a fleet another Server pointed at a Collector.
    - The Server offers each Agent a destination only for the signals it declares.
    - How the destinations ride in the one hashed `ConnectionSettingsOffers`, and when the Server
      declares `OffersConnectionSettings`, is
      [SEC-connection-settings-never-plaintext](SEC-connection-settings-never-plaintext.md)'s.

35. **The departure from the Baseline is recorded, not argued upstream.**
    - Clauses 28, 29 and 34 depart from the schema's "not set means unchanged" and from the
      `destination_endpoint` MUST.
    - [`CONFORMANCE.md`](../CONFORMANCE.md) carries this as a row under *Deviations*. The row names
      the sentences departed from, the implementation followed and the reason.
    - Settling the disagreement between the schema and `opampsupervisor` is upstream's. This
      project takes no position in `opamp-spec`.

**Out of scope:**
- the cloud-shaped conventions (`host.image.*`, `host.type`), which come from provider metadata
  services;
- reporting kinds through `AvailableComponents`;
- bootstrapping a type by starting a Collector once with a generated extension-only configuration;
- a Collector's internal telemetry, which an operator configures the Collector for;
- the Server storing or forwarding telemetry, since the specification keeps this project out of
  the telemetry-backend business;
- `AcceptsOtherConnectionSettings`;
- trace context across a Gateway hop ([CLT-modes-and-gateway](CLT-modes-and-gateway.md));
- a sampling hint from the Server;
- a local switch on the Agent that drops the three capability bits;
- the Supervisor Endpoint's token and which processes are handed it
  ([CLT-modes-and-gateway](CLT-modes-and-gateway.md));
- what a Managed Process can read on its host, which includes the Client's `state_dir` and key
  while both run under one account
  ([CLT-accounts-service-runs-under](CLT-accounts-service-runs-under.md));
- `service.version`, which stays the process's word;
- what a compromised Client reports, since its code builds the whole description and no rule on
  the Client binds it;
- the Server's precedence of the effective description
  ([SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md)).

## Alternatives considered

- **Keep `service.name` as the instance name and add `service.type`.** A Collector's extension
  reports `service.name` and knows nothing of `service.type`, so it would keep overwriting the
  instance name with its type — the collapse this decision prevents.
- **No human instance name at all.** The most conformant reading, but a UID is not a name a person
  can use, and several Supervisors share one `host.name`.
- **`host.name` as the instance name.** A property of the machine, while Supervisor Mode runs *n*
  Agents on one machine ([CLT-modes-and-gateway](CLT-modes-and-gateway.md)).
- **The instance name outside `AgentDescription`**, as Bindplane does. Needs a channel this
  project does not have, and makes the name invisible to Selectors, which pin one Agent by it.
- **The Managed Process's `service.name` as the first source of the type**, above the kind's own
  type, the block's `service_name` and the file name, because `dist.name` is a better type than
  anything a file can infer. A process, or anything it starts, would then choose which type's
  Configurations are pinned to its Agent and which Deployment claims it, and the Server cannot
  tell that statement from the Supervisor's. What the Server assigns by type and what the
  Supervisor installs by type would read different values, so an honest Collector would be offered
  Packages it then refuses. Rejected.
- **Admit the process's `service.name` only where it equals the Supervisor's.** That is this
  decision minus the warning: a differing value is dropped either way. It says nothing to the
  operator whose Collector reports a `dist.name` they expected to see as the type. Rejected for the
  warning of clause 9.
- **Report the process's `service.name` under a key of its own, for display.** The fleet view
  could then show `dist.name` beside the type. It is a new attribute key, read by nothing that
  decides, and a key a process fills is one more claim nobody checks; the warning tells the
  operator the same thing where they can act on it. Not taken now.
- **Mark the Agent unhealthy while the types differ.** It would force a decision, but it would turn
  an honest Collector that runs fine into a failing Agent, and hold back its rollouts, for a naming
  question. Rejected.
- **Let a wrapped kind's type stand against the process, and keep the process's type for
  `collector` and `command`.** `collector` and `command` are the kinds that run a program the
  operator picked without knowing its children, and `command` speaks no OpAMP at all, so a type
  reported on its Endpoint comes from something the operator never meant to state one. Rejected.
- **Stop handing the token to processes that speak no OpAMP.** It narrows who can reach the
  Endpoint, but a Collector's own children still inherit the token, and the type would still be the
  process's word. It is a separate decision about the Supervisor Endpoint, which
  [CLT-modes-and-gateway §28](CLT-modes-and-gateway.md) takes for a kind whose program never
  connects to it.
- **Fold every key a process reports except `service.instance.id` and `service.instance.name`.**
  A process on one host could draw another host's Configurations, an artifact for another
  platform, and a whole host out of its Label-based rollouts. Rejected.
- **A deny-list: drop `host.*`, `os.*`, `supervisor.*` and the operator's keys, and pass the
  rest.** This drops the known attacks. But the Client does not know which keys the Server's Labels
  use, so a process could still shadow a Label such as `rollout` and leave its channel, and a key
  the Supervisor starts to report later would be open until someone adds it to the list. An
  allow-list of `service.*` is closed by default and costs only the attributes a process states in
  a namespace of its own, which nothing in this project reads. Rejected.
- **Let the Server ignore a reported `service.name`, `host.*` and `os.*` for Selectors, the
  Configuration fit and the artifact choice.** The Server receives one description and cannot tell
  which key the process supplied. Ignoring these keys for every Agent would remove the Supervisor's
  true values with the false ones: pinning one host by `host.name`
  ([PKG-signed-packages-and-delivery](PKG-signed-packages-and-delivery.md)) would stop working, the
  artifact could no longer be chosen, and the grouping behind a Gateway
  ([SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md)) would lose its key. The
  Supervisor knows which value came from the process and can decide where the Server cannot.
  Rejected.
- **Let an operator admit further keys per `[[supervisor]]` block.** That would restore a
  Collector's own tags. But it puts a per-Agent setting back into `supervisor.toml`, where tagging
  one Agent is a Label's job (clause 10). It can be decided later if a need appears. Not taken now.
- **Let `[attributes]` add any key the code does not report, `supervisor.*` included.** An
  operator could then state `supervisor.kind.*` and make every Agent of the host a Client Agent,
  or state `supervisor.package.version` and choose the version a Package is held against. That
  namespace is the Supervisor's own statement, so `[attributes]` refuses every key in it at
  startup, however TOML spells it (clause 10).
- **Report no version of the installed Package, and leave the Agent's version as the only one.**
  A Package would then be held against the Agent's own claim, which is the same field for the
  Client Agent and for a Supervisor-backed one
  ([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md), which holds a
  Package against `supervisor.package.version`). The Supervisor reports what it installed under a
  key of its own (clause 5).
- **Derive the type from `<program> --version` output.** A name has no grammar the way a SemVer
  does (`Fluent Bit v3.1.0`, `promtail, version 3.0.0 (branch: main)`); the configuration fallback
  gets the same value without guessing.
- **Bootstrap the type by starting the Collector with an extension-only configuration**, as the
  upstream Supervisor does. Adds a process start, and the type it obtains is the process's own
  word, which clause 8 does not admit.
- **A dedicated interface-enumeration crate** (`if-addrs`, `mac_address`). A new dependency for
  what an existing one's feature flag provides.
- **Joined strings on the wire.** Bakes a display choice into the protocol and departs from the
  conventions' declared type; the view is the place to join.
- **The Server records the connection's peer address.** Reports the NAT or the Gateway, not the
  host.
- **Vendor `opentelemetry-proto` and encode OTLP by hand.** The cheaper build — prost, protox and
  the `reqwest` already present, no new crate. Rejected: a second copied schema is a second
  protocol to keep in sync with none of the Baseline machinery that makes the OpAMP copy safe, and
  its semantic conventions would become string literals nobody re-checks. Owning the bytes is right
  where the bytes are the product; here they are the exhaust.
- **Keep the SDK's default features.** The defaults select `reqwest-blocking-client`, and blocking
  HTTP inside a tokio runtime starves an executor.
- **Send OTLP over gRPC.** The schema requires an OTLP/HTTP/Protobuf receiver.
- **Configure the Collector's internal telemetry from the Supervisor**, as `opampsupervisor` does.
  The richer answer for a Collector, and what an operator from upstream expects. Rejected: this
  Client does not touch a Managed Process's configuration, and inventing an abstraction over it is
  a non-goal.
- **Use the OpenTelemetry trace API directly.** No added dependency, and what the reference
  implementation does in Go. Rejected: it puts OTel types and an explicit `Context` into domain
  modules built as logic with `tracing` on top, and log correlation would be attached by hand at
  every site instead of once by a layer.
- **Metrics and logs only; do not declare `ReportsOwnTraces`.** Rejected: the operations that fail
  in a fleet are multi-phase lifecycles with outcomes, and the trace is the one signal that answers
  why a rollout failed rather than that it did.
- **Span every message**, as `opampsupervisor` does. That supervisor manages one Collector, where
  message handling is the work; a Client here multiplexes n Agents over one connection on every
  host of a fleet. The oracle governs protocol behaviour, and what a Client measures about itself
  never reaches the wire.
- **One span per operation, no child spans.** "Which phase failed" is the question an operator
  asks of a rollout; a single span answers only "it failed", which the reported status already
  says.
- **Link the post-restart span instead of continuing the trace.** Span links are the standard's way
  across a boundary, but "did this update land" is one question and should be one trace. The
  marker carries the ids either way, so this stays reversible.
- **A destination, or a list of trusted networks, in `supervisor.toml`.** A locally configured
  destination is a private extension wearing the capability's name, and a per-host allowlist would
  give a fleet two places to look when telemetry does not arrive — one on the host nobody is
  logged into.
- **Declare the capabilities only while a destination is in force**, as `OffersPackages` is. Those
  describe something the end has; these describe something it can do, and the conditional version
  deadlocks the first offer.
- **Cleartext inside the private address space** — RFC 1918's three ranges and `fc00::/7` besides
  the loopback. Spares the small fleet with one Collector on its LAN a certificate, a name and a
  renewal. Rejected: an unroutable address is not a trusted network, the same address is a
  different machine on another site, and the specification accepts plaintext on the loopback alone
  (Q-1).
- **Admit `localhost` by name.** It is loopback by convention
  ([RFC 6761](https://www.rfc-editor.org/rfc/rfc6761)), but what it resolves to is the host
  resolver's answer, not the URL's, and the OpAMP connection refuses it for the same reason
  ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)).
- **Resolve names and admit those that resolve to the loopback.** The answer would depend on what
  DNS said when the offer arrived, and the name can be re-pointed afterwards without any offer
  changing.
- **Admit TLS 1.2 to a Collector that offers nothing newer.** Reaches older receivers. Rejected:
  every connection that leaves the host is TLS 1.3, and `rustls` is built without TLS 1.2 for the
  whole project, so a per-destination exception would need a second TLS stack.
- **Warn instead of refusing**, as for credentials in cleartext. Proportionate to a credential on
  one request, not to a continuous stream of identifying attributes and logs.
- **The literal reading of the offer, with the gap documented.** Leaves no fleet-driven off switch,
  and misconfigures silently in both directions against the one peer that implements these fields.
- **Complete state per offer without the empty-endpoint withdrawal.** Cannot express "all off": an
  offer naming nothing means "unchanged", so the last remaining signal could never be stopped.
- **Withdraw automatically when `[telemetry_offer]` disappears.** The Server holds no record of
  what it offered, so every Server without the section would tombstone Agents another Server
  configured.
- **A signal of our own** — a `disabled` flag or a custom capability like Bindplane's. Inventing
  protocol semantics is a non-goal, and a bespoke signal would be understood by one Server and one
  Client. Departing towards the implementation everyone runs is the only departure that buys
  interoperability.
- **Raise it against `opamp-spec` and wait.** The protocol is Beta on a slow cadence, and this
  project implements the protocol and records its departures; it does not staff the protocol's
  evolution.

## Sources / Prior art

- [`opamp.proto`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto), `AgentDescription` —
  `service.name` as the type, `service.instance.id` as the identity, and the non-identifying
  clause admitting user-defined attributes.
- [Bindplane — Bring Your Own
  Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — Agent Type from `dist.name`, the human name kept apart.
- [`opampextension`
  `opamp_agent.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go)
  — the identifying `service.name` (the Collector's build name), `service.version` and
  `service.instance.id`; the non-identifying `os.type`, `host.arch`, `host.name` and
  `os.description`; and the configured `agent_description.non_identifying_attributes` and
  `include_resource_attributes`. Recalled, not re-read for this record.
- [OpAMP Supervisor
  `config.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/config/config.go)
  — operator-stated identifying attributes, the need the `service_name` key answers.
- [PR #38809 — Control Collectors with only the OpAMP
  extension](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38809)
  — the bootstrap mechanism weighed and declined.
- [Semantic conventions —
  `service`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/service.md)
  — no attribute for a human instance name.
- [Semantic conventions —
  `host`](https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/)
  and [`os`](https://opentelemetry.io/docs/specs/semconv/registry/attributes/os/) — `host.ip`,
  `host.mac`, `host.cpu.model.name`, `os.build_id`, their formats and loopback exclusion.
- `sysinfo` 0.40 `Networks` API (`mac_address()`, `ip_networks()`), behind its `network` feature.
- The Client's code: the fold and the operator attributes in
  `crates/fleet-agent/src/supervisor/agent/description.rs`; `service_name()` in
  `crates/fleet-agent/src/supervisor/agent.rs`, which `crates/fleet-agent/src/engine.rs` uses for
  the telemetry label; the block's `service_name`, the kind's default and the program's file name
  in `crates/fleet-agent/src/supervisor/block.rs`; the kinds and the `WRAPPED` list, and the
  `supervisor.kind.*` attributes set only on the Client Agent, in
  `crates/fleet-agent/src/supervisor/mod.rs`; `OPAMP_SUPERVISOR_TOKEN` handed to every Managed
  Process, and the version probe that reports `service.version` alone, in
  `crates/fleet-agent/src/supervisor/process.rs`; the bearer token that admits a connection whose
  description is passed on as an event, in `crates/fleet-agent/src/supervisor/endpoint.rs`; the
  refusal of a Package for another type and the high-water mark keyed by the Supervisor's type in
  `crates/fleet-agent/src/supervisor/agent/packages.rs`.
- The Server's code: the Configuration fit by the reported `service.name` and the pinned
  assignment in `crates/fleet-server/src/configs.rs` and `crates/fleet-server/src/fleet.rs`; a
  Selector by attribute equality, identifying attributes first, in
  `crates/fleet-server/src/deployments.rs`; a reported attribute beating every Label, the shadowed
  Label listed, any `supervisor.kind.*` key making a Client Agent and the machine behind a Gateway
  keyed by `host.id`, else `host.name`, in `crates/fleet-server/src/labels.rs`; the group behind a
  Gateway and two Client Agents in a group marking all ambiguous in
  `crates/fleet-server/src/fleet/placement.rs`; the platform from the reported `os.type` and
  `host.arch` and a Package resolved for the reported type in
  `crates/fleet-server/src/packages.rs`.
- `docs/manual/rollout.md`: `channel` in `[attributes]` as a way into a channel.
- `SECURITY.md`: a host that reports another partition's attributes is assigned by that
  partition's next rollout, the same mechanism one level up.
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md) —
  *Own Telemetry Reporting*, and the vendored
  [`opamp.proto`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto): the three capability
  bits, `ConnectionSettingsOffers.own_*` ("If this field is not set then the Agent should assume
  that the settings are unchanged") and `hash`, `TelemetryConnectionSettings` (the
  OTLP/HTTP/Protobuf MUST, the "MAY refuse `http://`", `certificate`'s withdrawal by omission and
  the advice on `ca_cert`), the identifying attributes in the Resource, and the 10 s recommended
  reporting interval.
-
  [`opampsupervisor`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor)
  — `cmd/opampsupervisor/supervisor/supervisor.go`: `updateOwnTelemetryData`,
  `setupOwnTelemetry`, `processOwnTelemetryConnSettingsMessage`,
  `loadLastReceivedOwnTelemetryConfig` and the dispatch in `onMessage`; and the spans it emits,
  [contrib#38724](https://github.com/open-telemetry/opentelemetry-collector-contrib/issues/38724)
  and [contrib#38797](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38797).
- `opamp-go`, `client/internal/receivedprocessor.go` — passes `OwnMetrics`, `OwnTraces`,
  `OwnLogs` through per capability, with no merge and no persistence.
- Elastic `fleet-server`, `docs/opamp.md` (monitoring-only mode, no connection settings
  management), and Bindplane's `com.bindplane.measurements.v1` — self-telemetry routed through the
  configuration channel instead.
- [`opentelemetry-otlp`](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/) and its
  [feature flags](https://lib.rs/crates/opentelemetry-otlp/features);
  [`opentelemetry-proto`](https://github.com/open-telemetry/opentelemetry-proto), the schema
  deliberately not copied.
-
  [`opentelemetry-appender-tracing`](https://docs.rs/opentelemetry-appender-tracing/latest/opentelemetry_appender_tracing/)
  `0.33` — `OpenTelemetryTracingBridge`, its statement that it does not convert spans, and the
  changelog entry removing `experimental_use_tracing_span_context` (fixing
  [opentelemetry-rust#3190](https://github.com/open-telemetry/opentelemetry-rust/issues/3190)).
- [`tracing-opentelemetry`
  0.34.0](https://docs.rs/tracing-opentelemetry/0.34.0/tracing_opentelemetry/)
  — `OpenTelemetryLayer`, the reserved `otel.*` fields, `with_context_activation`; and the
  log/trace correlation between the two crates,
  [opentelemetry-rust#1378](https://github.com/open-telemetry/opentelemetry-rust/issues/1378),
  [#2803](https://github.com/open-telemetry/opentelemetry-rust/issues/2803),
  [#2824](https://github.com/open-telemetry/opentelemetry-rust/issues/2824).
- [`sysinfo`](https://docs.rs/sysinfo/latest/sysinfo/) — cross-platform process CPU and memory.
- [RFC 8446](https://www.rfc-editor.org/rfc/rfc8446) — TLS 1.3; `rustls`'s `ClientConfig`
  protocol versions and `reqwest`'s `ClientBuilder::min_tls_version`.
- [RFC 1918](https://www.rfc-editor.org/rfc/rfc1918),
  [RFC 4193](https://www.rfc-editor.org/rfc/rfc4193) and
  [RFC 6761 section 6.3](https://www.rfc-editor.org/rfc/rfc6761#section-6.3) — the private ranges
  and `localhost`, none of them admitted for cleartext; Rust's `IpAddr` parsing, compared with
  `Ipv4Addr::LOCALHOST` and `Ipv6Addr::LOCALHOST`.

## Consequences

- Positive: an operator's name for an Agent survives its Managed Process reporting for itself; the
  fleet can be aimed by what an Agent is, with no per-host attribute to maintain.
- Positive: no Managed Process, and nothing it starts, can choose its Agent's type, so none can
  draw another type's Configurations or a Deployment aimed at another type. What the Server
  assigns by type and what the Supervisor installs by type read the same value, and the type in
  the fleet view and the telemetry label is the one the operator configured.
- Positive: a Managed Process cannot draw another host's Configurations, be offered an artifact
  for another platform, make its host or another one ambiguous, or override the operator's
  attributes or shadow a Label outside `service.*`. That the Client Agent is recognised by the
  attributes it alone reports ([SRV-fleet-record-agents-hosts](SRV-fleet-record-agents-hosts.md))
  holds on every honest Client.
- Positive: the fleet view shows each host's addresses, the attributes are searchable, and the
  wire stays convention-shaped for any OpenTelemetry-aware consumer.
- Positive: three capability bits from one mechanism, and a fleet's Clients become observable
  without a second agent on the host, at a destination the Server decides.
- Positive: the wire format and the semantic conventions stay upstream's; a moved convention
  arrives as a version bump.
- Positive: a failed rollout is one trace naming the failing phase, and its log lines are joinable
  to it by `TraceId`. With no destination offered, instrumentation costs nothing.
- Positive: own telemetry can be switched off from the fleet — per signal by leaving it out of
  `[telemetry_offer]`, entirely with empty endpoints — and both this Server and a
  supervisor-shaped one can drive this Client and each other's Agents with one reading.
- Positive: own telemetry leaves the host encrypted wherever it goes, and the admission test reads
  the same on every host: the loopback literals are this machine everywhere.
- Negative / trade-offs: `service.instance.name` is a key this project invents; a future
  convention may contradict it, and renaming it would break Selectors matching it.
- Negative / trade-offs: a Collector whose `dist.name` differs from its program's file name
  reports the file name, or the block's `service_name`, as its type. Configurations, Deployments
  or Packages made for `dist.name` do not match such a Collector until the operator sets
  `service_name = "<dist.name>"` in its block; the warning of clause 9 names the value.
- Negative / trade-offs: what a Collector states outside `service.*` does not reach the Server:
  attributes in its own `agent_description.non_identifying_attributes`, its resource attributes,
  and its own `host.name` where that differs from the Supervisor's, as in a container. An operator
  who wants such a tag sets a Label, or an `[attributes]` entry for the whole host.
- Negative / trade-offs: network addresses leave the host and are visible to whoever can read the
  API; virtual adapters appear beside physical ones, because best effort reports what the platform
  says and does not curate.
- Negative / trade-offs: five OpenTelemetry crates and their batching machinery, with a global
  provider model that sits awkwardly beside destinations that change at runtime; and
  `tracing-opentelemetry` is on a release train one step behind the others, so an `opentelemetry`
  bump has a second cadence to wait for.
- Negative / trade-offs: exporter diagnostics and the Client's `tracing` output share a process;
  the bridge must not feed exporter errors back into the exporter.
- Negative / trade-offs: the Resource carries what describes the Agent, and span fields are as easy
  to add as log fields; clauses 23 and 24 are a discipline, not a mechanism.
- Negative / trade-offs: stderr and the log file print the enclosing span on every event inside an
  operation, so familiar lines gain a `package.install{…}:` prefix.
- Negative / trade-offs: the update marker carries two ids unrelated to its safety role.
- Negative / trade-offs: the `connection.settings.apply` that installs the trace exporter is
  itself not exported — there is nothing to export to until it is half done. It corrects itself on
  the next start, when the persisted destination is in force before anything runs.
- Negative / trade-offs: a Collector on the LAN costs a certificate, a name and a renewal, or a
  Collector on each host reached at `127.0.0.1`. `http://localhost:4318` is refused, which costs an
  operator who wrote it the literal. A receiver that speaks nothing newer than TLS 1.2 is
  unreachable until it is upgraded or put behind a TLS 1.3 proxy.
- Negative / trade-offs: this is a recorded deviation. The empty `destination_endpoint` fails the
  schema's MUST, so a strict third-party Client may reject the message; and a third-party Server
  that rotates one signal's headers by sending that signal alone stops this Client's other two.
- Follow-ups: whether the Server should refuse to compile a `[telemetry_offer]` endpoint no Agent
  will accept (an `http://` address off the loopback); surfacing `connection_settings_status` and a
  stopped reporter in the fleet view; watching for upstream settling the offer semantics at the
  next Baseline bump; whether a process that speaks no OpAMP should be handed the Supervisor
  Endpoint's token at all.

## Enforcement

- [`crates/fleet-agent/src/supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs)
  unit tests: `a_process_reporting_its_type_does_not_take_the_operators_name_with_it`,
  `the_supervisors_own_attributes_survive_whichever_list_a_process_reports_them_in`,
  `the_clients_own_agent_reports_its_type_and_its_configured_name_separately`,
  `the_clients_own_agent_type_is_the_one_name_this_program_has`,
  `configured_attributes_are_reported_but_never_shadow_reported_ones`,
  `configured_attributes_cannot_restate_the_type_or_the_instance_name`,
  `the_service_namespace_is_absent_until_configured_and_then_identifies`,
  `the_cpu_model_is_reported`, `the_agent_reports_the_host_name_a_selector_would_pin_it_by`,
  `the_os_is_reported_as_a_name_and_a_version_not_only_as_prose` (clauses 1–7, 10);
  `the_type_is_the_supervisors_whatever_the_process_reports` (clauses 1, 8);
  `a_process_states_its_own_service_attributes_and_nothing_about_the_host`,
  `a_process_never_overrides_what_the_operator_set`,
  `a_key_stated_in_both_lists_is_reported_once_among_the_identifying_attributes` (clauses 8, 10);
  `a_differing_reported_type_is_warned_about_once_per_value`,
  `a_reported_type_reaches_the_log_escaped_and_bounded` (clause 9).
- [`crates/fleet-agent/src/host.rs`](../../crates/fleet-agent/src/host.rs) unit tests:
  `host_addresses_follow_the_conventions`, `os_release_parses_the_fields_the_description_reports`
  (clauses 6, 7).
- [`crates/fleet-agent/src/config.rs`](../../crates/fleet-agent/src/config.rs) unit tests:
  `a_block_may_state_its_agent_type_and_a_reverse_fqdn_is_accepted`,
  `an_empty_agent_type_is_refused_rather_than_treated_as_absent`,
  `attributes_describe_the_host_and_a_block_no_longer_tags_one_agent`,
  `non_string_attributes_are_rejected`,
  `an_attribute_in_the_supervisors_namespace_is_refused_by_name` (clauses 1, 10).
-
  [`crates/fleet-agent/tests/collector_extension_e2e.rs`](../../crates/fleet-agent/tests/collector_extension_e2e.rs):
  `a_collector_extensions_own_report_reaches_the_fleet_as_its_supervisors_agent` — an extension
  report carrying another `host.name`, a `supervisor.kind.*` attribute and a `service.name` that
  differs from the block's type reaches the Server under the block's type, with the Supervisor's
  `host.name`, without the kind attribute, and with its `service.version` and health (clause 8).
- Server: [`crates/fleet-server/src/listing.rs`](../../crates/fleet-server/src/listing.rs) unit
  test `the_view_joins_a_string_array_attribute` (clauses 6, 11).
- [`crates/fleet-agent/src/telemetry.rs`](../../crates/fleet-agent/src/telemetry.rs) tests:
  `no_destination_builds_nothing` (clause 26),
  `cleartext_is_admitted_by_address_and_nowhere_else`, which refuses `localhost`, the rest of
  `127.0.0.0/8`, the private ranges and an address-shaped name,
  `a_cleartext_destination_beyond_the_private_network_is_refused`,
  `a_private_network_destination_is_refused_in_cleartext`, whose refusal names the setting and the
  two loopback literals, `a_loopback_destination_is_allowed_in_cleartext` (clauses 30–32),
  `an_empty_endpoint_stops_reporting_and_refuses_nothing` (clause 29),
  `offered_tls_settings_are_refused_by_name`, `an_offered_certificate_is_presented_by_the_exporter`,
  `an_offered_certificate_without_its_key_is_refused_and_named`,
  `an_offered_private_key_is_refused_by_name` (clause 33),
  `metrics_are_reported_at_the_interval_the_baseline_recommends` (clause 15),
  `the_resource_carries_the_agents_identifying_attributes`, `the_resource_carries_the_platform`,
  `the_resource_carries_no_other_non_identifying_attribute`,
  `an_agent_without_an_instance_name_reports_none` (clause 23),
  `the_exporter_client_speaks_tls_1_3_alone`, which reads the exporter's ClientHello off the wire:
  TLS 1.3 alone in `supported_versions` and only TLS 1.3 suites, so a destination offering only
  TLS 1.2 has nothing to negotiate,
  `an_export_to_a_destination_that_never_answers_gives_up` (clause 25),
  `a_span_this_client_writes_reaches_the_offered_destination` (clauses 17–18),
  `a_trace_survives_being_written_down_and_picked_up_again`,
  `an_unreadable_trace_reference_is_ignored` (clause 21).
- [`crates/fleet-agent/src/connection.rs`](../../crates/fleet-agent/src/connection.rs) tests:
  `an_offer_naming_one_signal_stops_the_others`,
  `an_offer_silent_about_telemetry_leaves_all_three_alone` (clause 28),
  `an_empty_endpoint_withdraws_the_signal` (clause 29).
- [`crates/fleet-agent/src/transport/mod.rs`](../../crates/fleet-agent/src/transport/mod.rs):
  `a_refused_telemetry_destination_is_reported_failed_on_the_same_offer` (clause 32).
- [`crates/fleet-agent/src/update/installer.rs`](../../crates/fleet-agent/src/update/installer.rs):
  `a_marker_carries_its_trace_and_one_written_without_it_still_parses` (clause 21);
  [`crates/fleet-agent/src/packages.rs`](../../crates/fleet-agent/src/packages.rs):
  `the_download_source_drops_whatever_authorises_it` (clause 24).
-
  [`crates/fleet-agent/tests/supervisor_process.rs`](../../crates/fleet-agent/tests/supervisor_process.rs):
  `the_phases_of_an_install_hang_off_the_span_that_came_with_it` (clause 19).
- [`crates/fleet-server/tests/own_telemetry.rs`](../../crates/fleet-server/tests/own_telemetry.rs):
  `every_declared_signal_is_offered_a_destination`,
  `a_withdrawn_signal_is_offered_as_an_empty_destination`,
  `an_undeclared_signal_gets_no_destination`,
  `an_agent_that_reports_no_own_telemetry_is_offered_none` (clause 34); and
  [`crates/fleet-server/src/config.rs`](../../crates/fleet-server/src/config.rs):
  `an_empty_endpoint_is_a_withdrawal_and_a_wrong_one_is_still_an_error` (clause 34).

**Not mechanically decidable:** review decides these on each change to the files this ADR applies
to:

- that no OTLP schema is vendored and no OpenTelemetry type appears outside `telemetry.rs`
  (clauses 12, 17);
- that names come from the semantic conventions (clause 13);
- that message handling stays unspanned and no sampler is configured (clauses 20, 22);
- that a new span field carries nothing that must stay on the host (clause 24);
- that the *Deviations* row stays in step with clauses 28, 29 and 34 (clause 35).
