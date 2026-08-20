# ADR-0024: An Agent reports its own telemetry over OTLP/HTTP, through the OpenTelemetry SDK, to the destinations the Server offers

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

With [ADR-0013](0013-agent-admission-on-the-opamp-endpoint.md) implemented, 17 of the
Baseline's 23 capability bits are implemented and 23 of the 24 behaviour rows in
[`CONFORMANCE.md`](../CONFORMANCE.md) hold. Four bits remain unimplemented, and **three of them are
one feature**: `ReportsOwnTraces` (`0x0020`), `ReportsOwnMetrics` (`0x0040`), and `ReportsOwnLogs`
(`0x0080`), all `[Beta]`. Taking them closes the largest remaining gap in one decision and leaves
exactly one bit undone — `AcceptsOtherConnectionSettings`, which cannot be honoured without
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
That road is closed: [ADR-0011](0011-supervisor-mode-core-and-plugins.md) has the Collector
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

### A telemetry destination is not an OpAMP endpoint

[ADR-0014](0014-server-driven-connection-settings.md)'s flow for OpAMP settings is: acknowledge
`APPLYING`, **verify by actually connecting**, persist, **reconnect**, acknowledge `APPLIED`.

**That flow cannot be run for a telemetry destination.** The thing to be proved is an OTLP receiver,
not the OpAMP endpoint; connecting to it proves nothing about the OpAMP connection, and there is no
connection to re-establish afterwards. Applying a destination "exactly as the `opamp` settings are"
describes an operation that has no meaning for the object it is applied to.

**The Server sends a telemetry destination on its own.** With a `[telemetry_offer]` and no
`[connection_offer]`, it composes an offer whose `opamp` field is absent and sends it — `fleet.rs`,
with the comment *"An Agent that accepts no OpAMP settings may still report telemetry, so the two are
gated separately: with only a telemetry destination to offer, that is the whole offer."*
`crates/server/tests/own_telemetry.rs` asserts exactly that shape.

**A Client that acts only on `opamp` makes the feature inert and the protocol loop.** If
`AgentState::handle` acts on an offer only `if offers.opamp.is_some()`, a Server with only
`[telemetry_offer]` sends the offer and the Client ignores it entirely — no `APPLYING`, no
`connection_settings_status`, no exporters. The Server's hash gate compares a reported
`last_connection_settings_hash` that never arrives, so it re-offers on **every** exchange, forever.
Nothing logs an error on either side: each end is doing what it was built to do.

**The Baseline settles which end is wrong, and it is not the Server.** *Connection Settings
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

Gating the Server's capability bit on `[connection_offer]` alone would leave the Server exercising
`OffersConnectionSettings` without declaring it — the very thing its own `capabilities()` comment
promises never happens.

What has to be settled is whether this project has **one** class of connection-settings offer with
one lifecycle, or the Baseline's three with sequences of their own.

### A Server's capabilities bind the Client too

The Baseline makes capability negotiation symmetrical and mandatory. *Interoperability of Partial
Implementations*: *"After the Server learns about the capabilities of the particular Agent the
Server MUST stop using the capabilities that the Agent does not support. Similarly, after the Agent
learns about the capabilities of the Server the Agent MUST stop using the capabilities that the
Server does not support."*

The first half is the easy one. The Server gates what it offers on what each Agent declares —
configuration only to `AcceptsRemoteConfig`, telemetry destinations per signal, restart only to
`AcceptsRestartCommand`. The second half asks more of the Client. It caches `ServerCapabilities` on
every reply, and a cached bit changes nothing until a gate reads it: without one, `package_statuses`
rides to a Server that never declared `AcceptsPackagesStatus`, and `connection_settings_status` to
one that never declared `OffersConnectionSettings`.

Against this project's own Server that costs nothing, because it declares what it exercises. The
target that makes it matter is [ADR-0027](0027-interoperability-against-opamp-go.md): `opamp-go` as
the behavioural oracle, and third-party Servers generally. A Server is entitled to implement the two
required bits and nothing else, and this Client is obliged to notice.

**The two oldest gates disagree with each other for good reasons.** `server_accepts_effective_config`
starts optimistic — report until told otherwise — because a Server that has not yet replied has said
nothing, and silence is not a denial. `server_signs_certificates` starts pessimistic — never send a
CSR until the bit is seen — because a CSR to a Server that does not sign them earns a `BadRequest`.
Both are right, for different reasons, and doc comments are a poor home for those reasons. Adding a
third gate means picking a default a third time, and the next person to add one needs something to
consult.

**A naive reading of the rule deadlocks two live features.** The obvious gate for
`connection_settings_status` is `OffersConnectionSettings`. But a Server can send an offer without
declaring that bit — this project's own may, and a third-party Server may for reasons of its own. If
the Client then withholds the acknowledgement, the Server's hash gate never closes and it re-offers
forever. The rule meant to improve interoperability would, applied literally, produce an infinite
loop out of two conforming implementations. Something has to outrank the bitmask, and it has to be
written down before it is coded, not discovered in a bug report.

**And one plausible gate would be actively harmful.** `remote_config_status` looks like it belongs
behind `OffersRemoteConfig`. It does not, and the reasons are specific enough that a future reader
will re-derive the wrong answer unless they are recorded. What needs deciding is not each gate but
the rule that produces them, including the rule for when *not* to gate.

### Where cleartext telemetry may go

The refusal of cleartext has a reason: the OTLP Resource carries the Agent's identifying attributes
and the log records carry whatever the Client logs, so the Baseline's *"MAY refuse to send the
telemetry if the URL begins with `http://`"* is taken — one step firmer than the credential warning
of [ADR-0013](0013-agent-admission-on-the-opamp-endpoint.md), because this is a continuous stream rather than
a single request. What needs care is where the line is drawn.

**Loopback is the only line with a definition of its own.** It is not a judgement about networks —
it is the statement *nothing leaves the machine*, which needs no assumption about the deployment to
be true. It fits the shape this repository ships: [`.devcontainer/`](../../.devcontainer/) publishes
the Collector's OTLP/HTTP port to the host precisely so `http://localhost:4318` means the same thing
inside the workspace container as outside it, and `config/server.toml`'s first worked example says
so in as many words.

**What a loopback-only line makes impossible is the ordinary small-fleet shape.** One Collector on a
host of its own, Agents on the same LAN, one hop between them: the configuration a fleet reaches the
moment it stops being one machine, and long before it becomes something with an ingress and a
certificate lifecycle. Under a loopback-only line an operator in that position has exactly two
options — put a Collector on every host, or terminate TLS in front of the one they have. The second
means a certificate, a name to put it on, and a renewal, all so that a stream can cross a network
segment the operator already owns and where nothing on the path is anyone else's.

**The risk has an address range attached to it.** *"Someone between the Agent and the receiver can
read the identifying attributes and the logs"* is a statement about who is on the wire. Loopback
answers it with *nobody*; the public internet answers it with *anyone*. The private address space —
[RFC 1918](https://www.rfc-editor.org/rfc/rfc1918)'s `10/8`, `172.16/12` and `192.168/16`, and
[RFC 4193](https://www.rfc-editor.org/rfc/rfc4193)'s `fc00::/7` — answers it with *whoever the
operator has put on their own network*, and those ranges are not routable across the public
internet, so the answer does not quietly change when a route does. That is a weaker guarantee than
loopback's and a categorically stronger one than a public address's, and it is the same line this
project already draws elsewhere: `crates/server/src/api.rs` deliberately does *not* block the RFC
1918 ranges when it validates a listener.

**A string comparison cannot draw this line.** A check that compares the host against literals
cannot even get loopback right: a URL brackets an IPv6 literal, so `http://[::1]:4318/v1/logs` split
on `:` yields `[`, and the loopback address is refused as cleartext. The predicate has to parse an
address rather than compare strings, which is what makes range membership answerable at all.

What has to be settled is whether the admission test asks *does this leave the machine* or *does
this leave the operator's network* — and, once it is the second, how the answer is established.

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

7. **Destinations come only from the Server.** `own_metrics`, `own_traces`, and `own_logs`
   arrive in the Server's `ConnectionSettingsOffers`, are persisted, and are put back in force at
   start before the first exchange. They are a class of offer of their own, applied as points
   12–15 decide.
   There is no destination in `client.toml` — the whole point of the capability is that the Server
   names it. With no destination offered, the Client sends nothing and costs nothing.

8. **`https://`, or cleartext inside the operator's own network, nothing else.** The
   Baseline's "MAY refuse" is taken: a destination on plain `http://` outside the admitted set of
   points 25–28 is refused and reported, because the Resource carries
   identifying attributes and the records carry whatever the Client logs. This mirrors the warning
   ADR-0013 already emits for credentials in cleartext, one step firmer.

9. **The three capabilities are declared unconditionally.** Unlike `OffersPackages` or
   `[client_ca]`, the capability here states an *ability* the Client always has — "I can report to a
   destination you name" — and the Server's offer is what arms it. Declaring it conditionally on a
   destination already being in force would mean the Server could never make the first offer.

10. **`certificate`, `tls`, and `proxy` behave exactly as they do on the OpAMP settings.**
   `TelemetryConnectionSettings` carries the same three fields; the certificate machinery of
   ADR-0013 is reused as-is, and `tls`/`proxy` are refused and *reported* rather than dropped in
   silence.

11. **The Server has a `[telemetry_offer]` section** — `metrics_endpoint`,
   `traces_endpoint`, `logs_endpoint`, and optional headers per signal — compiled into the same
   hash-gated `ConnectionSettingsOffers` that `[connection_offer]` produces. Without a
   `[connection_offer]`, the offer carries no `opamp` field and the telemetry destinations are the
   whole offer. Without a `[telemetry_offer]`, the Server offers no destination. Which capability the
   Server declares is point 16.

### A telemetry destination is an offer of its own class

We will treat a telemetry destination as **an offer of its own class**: applied and acknowledged
without a connection to prove it and without restarting the OpAMP connection. The Baseline's three
classes become three classes here. This narrows ADR-0014's verify-by-connecting rule to the class of
offer it was written for, without weakening it there.

12. **An offer is actionable when it carries anything this Client can put in force** — OpAMP settings,
   or any of `own_metrics`, `own_traces`, `own_logs`. Not `opamp` alone. An offer carrying only
   `other_connections` is **not** actionable while `AcceptsOtherConnectionSettings` is undeclared:
   acknowledging what cannot be applied is the lie this whole path exists to prevent, and a
   conforming Server does not send it.

13. **Verification proves what it is able to prove.** The `opamp` half is verified by actually
   connecting, exactly as ADR-0014 requires and for exactly ADR-0014's reason — not losing access to
   the Server. A telemetry destination is not verified by connecting: reachability of an OTLP
   receiver is not this Client's to establish at offer time, and a receiver that is down is not an
   offer that is wrong. What *is* checked before it is put in force is what this Client can decide
   on its own — the cleartext refusal (point 8) and the unhonoured fields
   (point 10). Those checks are the telemetry class's admission test, and a failure is
   reported, not swallowed.

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

16. **The Server declares `OffersConnectionSettings` whenever it can offer anything** — a
   `[connection_offer]`, a `[telemetry_offer]`, or a `[client_ca]` whose issued certificate travels
   as an ordinary offer (ADR-0013). Declaring a capability the Server exercises is the Baseline's
   rule, and the comment on `capabilities()` — *"an undeclared capability is never exercised, a
   declared one never hollow"* — holds rather than stays aspirational.

17. **What is persisted says only what was offered.** The stored `connection-settings.pb` carries an
   `opamp` block only when the offer or the state it folds into had one. A file that claims the
   Server offered OpAMP settings it never offered is a lie in the one artefact an operator is told to
   inspect and delete.

18. **`other_connections` joins this class when it lands.** It is the Baseline's third class and it
   names destinations that are not the OpAMP endpoint, so the same reasoning applies: no
   verification by connecting, no reconnect, one acknowledgement. Implementing
   `AcceptsOtherConnectionSettings` therefore does not reopen this question — which is the point of
   deciding it here rather than case by case.

### A Server's capabilities bind what the Client reports

We will make the Client's use of Server capabilities a stated rule rather than a per-field
judgement: optimistic until the Server speaks, and an offer outranks its bitmask.

19. **Optimistic until the Server has spoken; binding once it has.** Until a `ServerToAgent` carrying
   a non-zero `capabilities` field has been seen, every report rides. From then on the Server's
   declaration governs. This is the Baseline's own timing — *"After the Agent learns about the
   capabilities of the Server"* — and it is what makes the first status report, which necessarily
   precedes any declaration, legal. A `capabilities` of zero is *"MAY be omitted in subsequent
   ServerToAgent messages"*, not a retraction, so the last non-zero declaration stands.

20. **A received message outranks the bitmask for the report that answers it.** If the Server has
   sent an offer, this Client reports on that offer regardless of the corresponding bit. An
   exercised capability is a stronger statement about what a Server accepts than its bitmask is, and
   the alternative is a loop: a Server that offers and then learns nothing can never stop offering.
   Concretely, a received `connection_settings` latches the connection-settings status on for the
   life of the Agent, even when the offer was one this Client could not act on.

21. **Reporting is gated where the Server's bit governs the report itself.** `package_statuses` is
   withheld from a Server that has declared capabilities without `AcceptsPackagesStatus`;
   `connection_settings_status` from one without `OffersConnectionSettings`, subject to point
   20. A withheld report is not queued for later — the dirty flag clears as usual, and
   the report returns through the full snapshot that follows any reconnect or `ReportFullState`.
   Holding the flag would deliver a stale status the moment the bit appeared.

22. **Sending is gated pessimistically only where the message would be an error.** A CSR to a Server
   that does not sign certificates is answered with `BadRequest`, so it is withheld until the bit is
   seen — the `server_signs_certificates` behaviour, with a stated reason. This is the deliberate
   exception to point 19, and the test for it is narrow: the message would be
   *rejected*, not merely unused.

23. **`remote_config_status` is never gated, and that is a decision rather than an omission.** Three
   reasons, recorded here and at the code:
   - **The bit does not say what the gate would assume.** `OffersRemoteConfig` is *"The Server can
     offer remote configuration to the Agent"* — a statement about what the Server sends. What
     licenses an inbound status report is `AcceptsStatus`, which every Server MUST set. There is no
     `AcceptsRemoteConfigStatus`, and substituting an outbound-offer bit for one is this project
     inventing a reading the protocol does not have.
   - **Where it would bite, it bites hard.** `last_remote_config_hash` is the only input to the
     Server's re-offer decision and to `in_sync`. A Server that stops declaring the bit — a restart
     with the section removed, an intermediary that rewrites capabilities — would silence the hash
     and put the fleet into a permanent re-offer loop.
   - **Where it would not bite, it does nothing.** The status is unset until a configuration has
     actually been offered, so against a Server that never offers one the field is already absent.
     The gate would be a no-op in exactly the case it was meant to cover.

24. **A capability that is undeclared is not exercised, on this end too.** Point 23 is the
   exception that proves the rule, not a licence: where a Server bit genuinely governs what the
   Client sends, the Client honours it. New capabilities are added under points 21 and
   22, and a decision not to gate is recorded in `CONFORMANCE.md` with its reason, the
   way point 23 is.

### Cleartext reaches the private address space — by address, never by name

We will admit a cleartext `http://` telemetry destination **inside the private address space**, and
refuse it everywhere else. The shape of the refusal stays point 13's admission test: a
destination this Client will not send to is *reported*, never warned about and never downgraded.

25. **The admitted set is loopback plus the private ranges.** `127.0.0.0/8` and `::1`; RFC 1918's
   `10.0.0.0/8`, `172.16.0.0/12` and `192.168.0.0/16`; RFC 4193's unique-local `fc00::/7`. An
   `https://` destination is unaffected — this rule governs cleartext only.

26. **The judgement is made on an address, never on a name.** `localhost` stays admitted, because it
   *is* loopback by definition ([RFC 6761](https://www.rfc-editor.org/rfc/rfc6761)) rather than by
   resolution. No other name is resolved to decide this. An admission test whose answer a re-resolve
   can flip is not one an operator can reason about, and DNS is the part of the path an attacker
   would move; a Collector reached by name over cleartext is therefore refused, and the answer is to
   name it by address or to put TLS in front of it.

27. **Membership is decided by parsing, not by prefix.** `192.168.0.1.example.com` is a host name that
   begins with a private address and is a public destination; `172.32.0.5` is one character from
   `172.16.0.0/12` and outside it. Both are refused because the host is parsed as an IP address
   first and tested for range membership second. This is also what admits the bracketed IPv6
   loopback literal, which a comparison of strings refuses.

28. **Ranges that are private but not the operator's are not admitted.** Link-local
   (`169.254.0.0/16`, `fe80::/10`) is autoconfiguration rather than a network anyone deployed, and on
   a cloud host `169.254.169.254` is the instance metadata service — not a place to stream logs at.
   Carrier-grade NAT (`100.64.0.0/10`) is shared with other subscribers of the same provider, which
   is the *someone else's wire* this refusal exists for. Neither is admitted, and neither is a
   judgement call left to the code: they are named here so that a later reading of "private" does not
   quietly widen.

29. **The refusal keeps its voice.** A destination outside the admitted set is refused and reported
   back with the reason, exactly as point 8 has it — not warned about, not downgraded,
   not dropped in silence. The message names what would be accepted, because *"use https://"* is
   unhelpful advice to an operator whose Collector is one hop away.

30. **This is the Agent's rule and it stays there.** The Server validates that a `[telemetry_offer]`
   endpoint is a full OTLP/HTTP URL with a path and nothing further; whether a destination is
   reachable in cleartext is decided where the packets originate. A Server may therefore offer a
   private address to a fleet, and each Agent answers for itself.

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
  `OffersPackages` and `AcceptsConnectionSettingsRequest` are declared. Rejected in point
  9: those describe something the end *has*, this describes something it *can do*, and
  the conditional version deadlocks the first offer.

### On the class of a telemetry offer

- **Always carry an `opamp` block, so that one lifecycle covers every offer.** The smaller diff, and
  it needs no new class. Rejected on three counts. It makes the Server synthesise a block it has
  nothing to put in, purely so the Client's guard passes. Each such offer then costs a
  verify-and-reconnect of the whole fleet for a change that never touched the OpAMP connection — a
  telemetry endpoint move would disconnect every Agent. And it contradicts the Baseline twice over:
  the *"3 classes"* structure, and the SHOULD that a Server send telemetry settings in its first
  reply *"unless there is no OTLP backend"* — with no mention of needing OpAMP settings to carry them.
- **Document the gap in `CONFORMANCE.md` and require `[connection_offer]` alongside
  `[telemetry_offer]`.** Honest, and it is what the Deviations table exists for. Rejected because
  the failure is silent on both ends and the workaround is a coupling with no reason behind it — an
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

### On the Server's capabilities

- **Leave it as three `if`s with doc comments.** Where the code is heading anyway, and it is honest
  about being local. Rejected because the two oldest gates default in opposite directions with the
  reasoning buried in comments, and the third gate cannot be written correctly without the rule in
  point 20 — which is not a coding detail but a choice about which signal wins when a
  peer contradicts itself.
- **Gate everything on the corresponding bit, no exceptions.** The literal reading. Rejected: it
  deadlocks the connection-settings acknowledgement against Servers that offer without declaring,
  and it breaks `remote_config_status` for the reasons in point 23. A rule that produces
  an infinite loop out of two conforming peers is a misreading of a rule whose stated purpose is
  interoperability.
- **Gate nothing and rely on the Server ignoring what it does not want.** Defensible on the wire —
  an unwanted sub-message costs a few bytes and a Server must tolerate it. Rejected because the
  Baseline states a MUST, ADR-0027 makes third-party Servers a target rather than a hypothetical,
  and `CONFORMANCE.md` claims this row implemented. The cheapest honest options were to implement it
  or to write it down as a deviation; this project's stated commitment is the former.
- **Derive the gates from the capability enum mechanically — a table from bit to field.** Attractive
  and it would prevent the next omission. Rejected for now because the mapping is not one-to-one in
  either direction: `AcceptsStatus` licenses several fields, `OffersConnectionSettings` governs a
  report about something the *Server* sends, and point 23 is a deliberate non-mapping. A
  table would have to carry exceptions, and a table of exceptions is the `if`s again with more
  machinery. Worth revisiting if the count of gated fields grows.
- **Put this in ADR-0004 or in `CONFORMANCE.md` rather than in an ADR.** Rejected. ADR-0004 decides
  how conformance is *tracked*; this decides behaviour. `CONFORMANCE.md` records what is implemented,
  and a rule that governs how every future capability is added is not a matrix row.

### On the cleartext line

- **Loopback only.** It needs no assumption about the deployment. Rejected because the cost lands on
  the deployment this project is actually for: a fleet of some tens of hosts with one Collector
  among them, told to obtain and renew a certificate for a stream that never leaves its own network.
  A rule at that price is one operators work around — by putting the Collector on every host, or by
  not reporting own telemetry at all — and neither workaround is better for the thing the refusal
  protects.
- **Admit `192.168.0.0/16` alone**, which is the range the request that prompted this line named.
  Rejected: `10.0.0.0/8` and `172.16.0.0/12` are the same class of network under the same RFC, and
  which of the three a site uses is an addressing-plan accident. A rule that admits one and refuses
  the others is one an operator cannot predict, and the first person to hit it would read it as a
  bug rather than a decision.
- **Make the admitted set configurable on the Client** — a list of trusted networks in
  `supervisor.toml`. Rejected on point 7: nothing about own telemetry is configured on
  the Client, because the whole content of the capability is *"report to the destination you
  name"*. Splitting the decision across a Server-named destination and a per-host allowlist would
  give a fleet two places to look when telemetry does not arrive, and the second one is on the host
  nobody is logged into.
- **Resolve host names and admit those that resolve into the private space.** The friendly option:
  an operator's `collector.lan` would just work. Rejected because it makes the admission test depend
  on what DNS answered at the instant the offer arrived. The offer is persisted and re-applied across
  restarts, the name can be re-pointed afterwards without any offer changing, and the check that
  said yes would never be consulted again. A test that cannot be re-run to the same answer is not a
  security boundary.
- **Drop the refusal and emit a warning instead**, as ADR-0013 does for credentials. Rejected for
  the reason point 8 gives: a warning is proportionate to a credential on one request
  and not to a continuous stream of identifying attributes and log records. Moving the line is not
  the same as removing it.

## Sources / Prior art

- [OpAMP specification — Agent's own telemetry](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  and the vendored schema:
  [the three capability bits](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L744-L756),
  [`TelemetryConnectionSettings`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L347-L375),
  [the `own_*` offer fields](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L521-L545) — the
  OTLP/HTTP/Protobuf requirement, the identifying attributes in the Resource, the process-metrics
  expectation, and the "MAY refuse `http://`" provision.
- **The Baseline `v0.20.0`, *Connection Settings Management*** — the *"3 classes of destinations"*
  framing and *"Depending on which connection settings are offered the sequence of operations is
  slightly different"* are the structural claim point 12 follows; the per-class
  capability gating listed there is what point 16 aligns the Server with.
- **The Baseline, `ConnectionSettingsOffers.opamp`** — the verification MUST and its stated
  justification (*"does not lose access to the OpAMP Server"*), which is what scopes it to one field.
- **The Baseline, *Own Telemetry Reporting*** — the sequence with no verification step, the SHOULD
  that the Server offers in its first message *"unless there is no OTLP backend that can be used"*,
  and the SHOULD that the Agent sets `connection_settings_status` *"when new settings are received"*.
- **The Baseline `v0.20.0`, *Interoperability of Partial Implementations*** — the symmetrical MUST,
  and its timing (*"After the Agent learns about the capabilities of the Server"*), which is what
  makes point 19's optimistic start conformant rather than a shortcut.
- **The Baseline, `ServerToAgent.capabilities`** — *"This field MUST be set in the first
  ServerToAgent sent by the Server and MAY be omitted in subsequent ServerToAgent messages by setting
  it to UnspecifiedServerCapability value"*, which is why zero is read as silence and not as a
  retraction.
- **The Baseline, `ServerCapabilities`** — the enum comments that distinguish `OffersRemoteConfig`
  ("can offer") from `AcceptsStatus` ("MUST be set, since all Server MUST be able to accept status
  reports"), which is the textual basis for point 23's first reason.
- **The Baseline `v0.20.0`, `TelemetryConnectionSettings`** — *"The Agent MAY refuse to send the
  telemetry if the URL begins with `http://`"*. A MAY, which is what leaves this project free to
  decide where the line falls; the decision is this project's, not the protocol's.
- [`opampsupervisor`](https://github.com/open-telemetry/opentelemetry-collector-contrib/tree/main/cmd/opampsupervisor)
  — the reference answer, which configures the Collector's own telemetry from the offer. Read as the
  alternative this decision declines, for the reason ADR-0011 states.
- **`opampextension` (opentelemetry-collector-contrib)** — a widely deployed Client that implements
  `ReportsEffectiveConfig`, `ReportsHealth` and `ReportsAvailableComponents` and *not* the
  connection-settings capabilities, which is why interoperability here cannot be checked against it
  and had to be read out of the specification instead.
  It is also an OpAMP client that declares a small subset and is documented as doing so — the mirror
  image of the problem points 19–24 address, and the reason a peer's
  declaration cannot be assumed generous.
- [`opentelemetry-otlp`](https://docs.rs/opentelemetry-otlp/latest/opentelemetry_otlp/) and its
  [feature flags](https://lib.rs/crates/opentelemetry-otlp/features) — confirms the feature set point
  1 pins: `http-proto` is the OTLP/HTTP protobuf encoding, `reqwest-client` the async
  client against the default `reqwest-blocking-client`, and `grpc-tonic` the only thing that pulls a
  gRPC stack in.
- [`opentelemetry-appender-tracing`](https://docs.rs/opentelemetry-appender-tracing/latest/opentelemetry_appender_tracing/)
  — `OpenTelemetryTracingBridge`, the standard layer from `tracing` events to OTLP log records,
  registered on the subscriber registry beside the existing `fmt` layer.
- [`opentelemetry-proto`](https://github.com/open-telemetry/opentelemetry-proto) — the schema, which
  this decision deliberately does **not** copy: `opentelemetry-otlp` already depends on the crate
  generated from it, so it is maintained upstream rather than here.
- [`sysinfo`](https://docs.rs/sysinfo/latest/sysinfo/) — cross-platform process CPU and memory, pure
  Rust over `libc`.
- **[RFC 1918](https://www.rfc-editor.org/rfc/rfc1918)** (private IPv4 address space) and
  **[RFC 4193](https://www.rfc-editor.org/rfc/rfc4193)** (unique-local IPv6) — the definitions point
  25 takes verbatim, and the reason the set is exactly three IPv4 ranges and one IPv6
  range.
- **[RFC 6761](https://www.rfc-editor.org/rfc/rfc6761) §6.3** — `localhost` resolves to loopback by
  specification, which is what lets point 26 keep it as the single admitted name without
  resolving it.
- **[RFC 3927](https://www.rfc-editor.org/rfc/rfc3927)** (IPv4 link-local) and
  **[RFC 6598](https://www.rfc-editor.org/rfc/rfc6598)** (carrier-grade NAT) — the two ranges point
  28 names and excludes.
- **Rust's standard library** — `Ipv4Addr::is_private` is the RFC 1918 trio exactly, and
  `Ipv4Addr::is_loopback` is `127.0.0.0/8`; `Ipv6Addr::is_unique_local` is still unstable, so
  `fc00::/7` is spelled out at the code with a note saying why.
- **This project's own `crates/server/src/api.rs`** — `is_internal`, which decides where a
  client-supplied artifact URL may steer the Server. It blocks link-local (*"where `169.254.169.254`
  lives"*) and the CGNAT range, and deliberately does *not* block loopback or the RFC 1918 /
  unique-local ranges, *"an operator's mirror legitimately lives on an internal network"*. Points
  25 and 28 draw the same line for the same reasons, arrived at independently
  and reconciled on purpose: two admission tests in one codebase disagreeing about what "private"
  means is how a gap gets found by somebody else.
- **This project's own Server** — `fleet.rs::settings_offer` and
  `crates/server/tests/own_telemetry.rs`, which implement and assert the standalone shape.
- **ADR-0014's verification rule** and its reasoning, which point 13 narrows in scope and
  leaves untouched in force.
- **This project's two oldest gates** — `server_accepts_effective_config` and
  `server_signs_certificates` — which embody points 19 and 22 and are
  generalised rather than changed by them.
- **[ADR-0027](0027-interoperability-against-opamp-go.md)** — `opamp-go` as the behavioural oracle,
  which turns the Client's side of capability negotiation from housekeeping into a requirement.

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

### Of the offer class

- **Positive: the feature works in the configuration it was designed for.** A Server holding only
  `[telemetry_offer]` reaches its Agents. The re-offer loop closes because there is an
  acknowledgement to close it with.
- **Positive: an operator gets metrics without configuring credential rotation.** The two settings
  are independent, which is what they always were on the wire.
- **Positive: a telemetry endpoint move does not disconnect the fleet.** Under the alternative it
  would — every change would carry a synthetic `opamp` block through verify-and-reconnect.
- **Positive: `AcceptsOtherConnectionSettings` is unblocked rather than deferred.** Point
  18 answers in advance the question that would otherwise have to be reopened when it
  lands.
- **Negative: two lifecycles exist where one would be simpler.** "Every offer is proved by
  connecting" would be a rule with no exceptions and easy to hold in the head; it holds for one
  field of three. The mitigation is that the seam is the Baseline's own and is named in one
  predicate rather than spread through the transports — but it is a second case to reason about,
  and this is where that cost is admitted.
- **Negative: a refused telemetry destination fails the whole offer.** By point 14, a
  cleartext `own_logs` endpoint outside the admitted set makes an offer `FAILED` that also carried a
  perfectly good credential rotation — which did apply, since verification succeeded, but is
  reported as part of a failed offer. The `error_message` names what was dropped, so the Server can
  tell; a Server that reads only the status enum sees a failure it may not deserve. Accepted as the
  price of one hash.
- **Negative: the Server declaring `OffersConnectionSettings` more often is visible to peers.** A
  Server with only `[client_ca]` declares a capability whose standing offer is empty until a CSR
  arrives. That is what the bit means — it *can* offer — but it is a wide declaration, and a peer
  Client may probe it.
- **Follow-ups:** `AgentSummary` exposes `remote_config_status` but not `connection_settings_status`,
  so a stalled or failed rotation is invisible in the fleet view and in the REST API — which is
  precisely what an operator needs to see once offers can fail for a second reason. Worth its own
  change; not decided here. Whether the telemetry class should also carry the interim `APPLYING`
  state, given there is no lengthy verification to be in the middle of, is left as written — it is
  reported, because the Baseline's status enum has it and a Server may be watching for it.

### Of the capability rule

- **Positive: a stated default.** The next capability added has a rule to follow and two worked
  examples of each branch, instead of a coin toss between the two oldest gates.
- **Positive: the deadlock is designed out rather than patched.** Point 20 is what makes
  a conforming-but-terse Server safe to talk to, and it is written before the code that needs it.
- **Positive: the `CONFORMANCE.md` row stops overstating.** "Capability negotiation — implemented"
  rests on every bit that governs a report, not on two of seven.
- **Negative: point 20 weakens the gate it qualifies.** A Server that sends one offer
  receives connection-settings status for the rest of that Agent's life, even if it later stops
  declaring the bit. That is the intended trade — a loop is worse than an unwanted sub-message — but
  it means the gate is not the clean function of the bitmask that point 21 suggests in
  isolation.
- **Negative: a withheld status is genuinely lost until the next full snapshot.** Point
  21 chooses a stale-free report over a prompt one. A Server that declares
  `AcceptsPackagesStatus` late learns the package state at the next reconnect or `ReportFullState`
  rather than immediately.
- **Negative: point 23 is a documented non-conformance to a literal reading.** A reviewer
  applying the MUST field by field will find `remote_config_status` going to a Server without
  `OffersRemoteConfig` and have to be argued out of it. That is why the reasons are recorded in
  three places — here, at the code, and in the matrix — and it is a cost that recurs at every review.
- **Follow-ups:** whether the Server's own use of Agent capabilities is complete under the same rule
  has not been audited here — this rule looks at one direction. The mechanical bit-to-field table is
  left open for the point at which the number of gated fields makes the exceptions cheaper than the
  repetition.

### Of the cleartext line

- **Positive: the shape between "one machine" and "an ingress with a certificate" is
  configurable.** A Collector on a LAN address is offered to the fleet as a plain `http://` URL, and
  the Agents report to it. That is the configuration most fleets of this size are actually in.
- **Positive: the bracketed IPv6 loopback literal is admitted.** `http://[::1]:4318/v1/logs` is
  loopback and a string comparison refuses it. Nobody reported it, which says something about how
  much IPv6 loopback gets used, but it was wrong.
- **Positive: the development stack is untouched.** Loopback is still loopback, the published ports
  in `.devcontainer/` still exist for the reason they always did, and no existing
  configuration changes meaning.
- **Negative: the test is on the address's *class*, not on reachability or on which network the
  Agent is actually on.** `http://192.168.10.5:4318` is admitted by every Agent in the fleet,
  including one on a different site where that address belongs to a different machine — and that
  Agent will send its logs there, in cleartext, to whatever answers. Loopback has no such ambiguity:
  it is always the same host. The mitigation is that the destination is one fleet-wide operator
  decision rather than something an Agent discovers, but the ambiguity is real and this is where it
  is admitted.
- **Negative: a private network is not a trusted network, and this rule assumes it is.** A flat
  office LAN with a guest VLAN bridged onto it satisfies every point here. What is being decided is
  that the operator's network is the operator's problem, which is a reasonable division of
  responsibility and is nevertheless weaker than what a loopback-only line guarantees.
- **Negative: cleartext by name stays refused, and names are what operators use.** An operator whose
  Collector is `collector.lan` on DHCP has to pin an address or terminate TLS. Point
  26 accepts that friction deliberately, but it is friction, and it will be reported as a
  bug at least once.
- **Follow-ups:** whether the Server should refuse to *compile* a `[telemetry_offer]` it can tell no
  Agent will accept — a public `http://` address, accepted by the Server and refused by every Agent
  it reaches — is a separate decision about where configuration errors are caught, and is not taken
  here.
