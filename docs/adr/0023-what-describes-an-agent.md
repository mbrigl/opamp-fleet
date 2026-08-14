# ADR-0023: What describes an Agent — `service.name` carries the type, `service.instance.name` the operator's name

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Extends [ADR-0011](0011-supervisor-mode-core-and-plugins.md) and
[ADR-0012](0012-selector-targeted-configurations-and-rest-api.md) rather than replacing
either: a Supervisor is still one Agent, and a Selector still matches reported attributes by
equality. What changes is *what one attribute means*. The `name` grammar of
[ADR-0010](0010-client-os-service.md) and the platform fit of
[ADR-0017](0017-package-artifacts.md) are untouched.

Beside what an Agent reports, the Server attaches labels of its own, which Selectors match but
which never restate a reported attribute.

Among what an Agent reports are the host's network addresses, its CPU model and its OS build.

## Context

### An Agent's type and its instance name share one key

A `[[supervisor]]` block's `name` is used for three unrelated things at once: it is the directory
under `<supervisor_dir>/` that the Supervisor owns (ADR-0019), it is the uniqueness key across
blocks, and it is reported verbatim as the Agent's `service.name`
([`agent.rs:813`](../../crates/client/src/supervisor/agent.rs#L813)). The first two are local
bookkeeping and are fine. The third is wrong, and it is wrong in the one place the Baseline is
explicit.

**The Baseline reserves `service.name` for the Agent *type*.** `AgentDescription` says it "should be
set to a reverse FQDN that uniquely identifies the Agent **type**, e.g.
`io.opentelemetry.collector`", and names `service.instance.id` separately as what "uniquely
identifies the Agent". This Client reports `service.instance.id` correctly — it is the instance UID
([`agent.rs:827`](../../crates/client/src/supervisor/agent.rs#L827)) — and then puts the instance's
*name* into the slot reserved for its type. Two meanings, one key.

**The collision is not theoretical: the Managed Process wins it.** A Collector carrying the
`opampextension` reports its own description, and the Supervisor folds it over its own with every
key overwriting except `service.instance.id`
([`agent.rs:865-874`](../../crates/client/src/supervisor/agent.rs#L865-L874)). `service.name` is not
exempt, so the moment the extension connects, the operator's `otelcol-edge-01` is replaced by
whatever the Collector's build info says — typically `otelcol-contrib`, identically on every host of
that distribution. The fleet view reads exactly that key
([`fleet.rs:1068`](../../crates/server/src/fleet.rs#L1068)) and the bundled UI puts it in the name
column ([`index.html:458`](../../crates/server/static/index.html#L458)), so three managed Collectors
become three rows all called `otelcol-contrib`, distinguishable only by the UID printed underneath.
The name an operator chose is not merely in the wrong field — it is destroyed by a process that
starts working correctly.

**And the type, which is the more useful of the two, is unreachable when it would matter most.**
Where the extension is absent — the core `otelcol` distribution, every Foreign Agent — nothing
reports a type at all, and there is no way to state one. So the fleet cannot answer "which of these
are Collectors", and ADR-0016's aiming has no attribute for it. A rollout that should reach the
Collectors and nothing else has to be expressed through an operator-invented attribute that every
host must carry, which is the per-host wiring ADR-0016 exists to remove. ADR-0017 closed the same
hole for the platform by making the fit mandatory; the type is the other half of "is this artifact
meant for this Agent", and it is still open.

**A reference implementation resolves this the way the Baseline reads.** Bindplane derives its Agent
Type from `dist.name` in the collector's build manifest — "This value is reported by the collector
via OpAMP and can be found in the manifest used to build your collector" — which travels as
`BuildInfo.Command` and is emitted by `opampextension` as the identifying `service.name`. The
instance is `service.instance.id` plus a separate human `agent_name` field that is *not* an
`AgentDescription` attribute at all. So the type is an attribute, the human name is not, and the two
never share a key.

That last detail is the one genuinely open question here, and OpAMP does not answer it: the Baseline
has `service.instance.id` and no notion of a human-readable instance name. Bindplane invented a
product field for it. This Client cannot — its Server learns about an Agent only through the
protocol.

### Rollout rings are a file on the host

[ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md) aimed packages at part of a fleet so that a rollout
could be tried on a few hosts first, and [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)
did the same for Configurations. Both match a Selector against the attributes an Agent **reports**,
and the attribute a staged rollout actually wants — `rollout = "canary"` — is one an operator invents.
Without Server-set labels it can only be invented in one place: the `[attributes]` table in
`client.toml`, on the machine.

So moving a host from the canary ring into the general one is a file edit plus a restart, on that
host. Separating an Agent's type from its name leaves exactly this behind: attributes usable for
staged rollouts (`rollout = "canary"`) live in a file on each host, so moving a host between rings
is a file edit plus a restart — the per-host wiring ADR-0016 set out to remove, in the one place it
remains. [ADR-0017](0017-package-artifacts.md) came back to it and
called Server-set labels *"now more clearly worth it"*, because with the Agent type doing the
fitting, a Selector's remaining job is almost entirely rollout rings.

It is the specification's own Problem statement — *"changing what a hundred agents do means reaching
a hundred machines"* — surviving inside the mechanism built to abolish it.

**Two forces decide the shape.**

- **Reported attributes are load-bearing facts, not annotations.** `os.type` and `host.arch` choose
  which artifact an Agent is offered (ADR-0017); `service.name` decides which packages fit it at all
  (ADR-0017). These are matched by the same `matches` function a Selector uses
  ([`configs.rs`](../../crates/server/src/configs.rs)), against the same attribute set. Anything that
  can rewrite that set can hand a Windows binary to a Linux host.
- **The identity a label hangs on has to survive.** The fleet is a map in memory that a restart
  empties, and an Agent can be forgotten deliberately (ADR-0026). A ring assignment that evaporated
  with either would be worse than none, because it would evaporate quietly.

### The host's network addresses are not reported

The Client describes where an Agent runs with the Baseline's `os.*` and `host.*` attributes —
`host.name`, `host.id`, `host.arch` and the `os.*` set, every one best effort and absent rather
than blank when the platform cannot answer. The host's network addresses are not among them, and
an operator looking at the fleet cannot see which addresses a host holds. The Server does see a
peer address on every connection, but it is the wrong fact to show: behind NAT it is the
translator's, and behind a Gateway (ADR-0025) it is the Gateway's — a hop that invents nothing
must not become the source of an attribute about someone else's host.

The OpenTelemetry semantic conventions already define both keys: `host.ip` and `host.mac`, each a
**string array**, each "excluding loopback interfaces", IPv6 in RFC 5952 form and MACs "in IEEE RA
hexadecimal form: as hyphen-separated octets in uppercase". The fleet view renders every reported
non-identifying attribute generically, so anything reported under these keys is displayed, and
searched, without a UI change.

## Decision

### The type and the instance name

We will **separate the two meanings into two attributes**: `service.name` carries the Agent *type*,
and a new `service.instance.name` carries the operator's name for the instance.

1. **`service.name` is the type**, resolved in this order, first hit wins:

   | Source | When |
   |---|---|
   | What the Managed Process reports | unchanged — the fold already does this, and it is `dist.name` for a Collector |
   | A new optional block key `service_name` | the operator states the type for a process that cannot |
   | The program's file name (`binary` / `command`) | the fallback: `otelcol`, `promtail` |

   The fallback is read from the configuration, never parsed out of a program's output. It is what
   the operator already wrote; extracting a name from `--version` text would be per-tool guesswork,
   and the rule at [`agent.rs:828-831`](../../crates/client/src/supervisor/agent.rs#L828-L831)
   forbids inventing an attribute a Selector could then match. The Baseline says a type "should" be
   a reverse FQDN; neither `dist.name` nor a program file name generally is one, so this ADR treats
   the FQDN as the recommendation it is and does not enforce a shape.

   For the Client's own Agent the type is the constant `opamp-fleet-client` (ADR-0022), not the
   configured `name`.

2. **`service.instance.name` is the operator's name for this Agent**, non-identifying: the
   `[[supervisor]]` block's `name` for a Supervisor-backed Agent, the top-level `name` for the
   Client's own. It is reported always, and it is **exempt from the fold** — added beside
   `service.instance.id` in the exemption at
   [`agent.rs:868`](../../crates/client/src/supervisor/agent.rs#L868), for the same reason: a
   Managed Process cannot know what the operator called the Supervisor that owns it, so a value it
   reports under that key is not an improvement on the configured one.

   The key is this project's, not the semantic conventions'. The Baseline explicitly admits "any
   other relevant Resource attributes" and "any user-defined attributes the end user would like to
   associate with this Agent" among non-identifying attributes, which is the licence being used;
   the name is chosen to read as the obvious partner of `service.instance.id` rather than to imply
   a convention that does not exist.

3. **`AgentView` gains `service_instance_name`**, and the bundled UI's name column shows it with the
   type on the sub-line, the reverse of the single-key display. The fallback chain becomes instance
   name → type → UID, so a row is never blank and never collapses onto its neighbours
   ([`fleet.rs:876`](../../crates/server/src/fleet.rs#L876),
   [`index.html:351`](../../crates/server/static/index.html#L351)).

4. **Nothing changes about the block `name` locally.** It stays the directory name and the
   uniqueness key, and it stays bound by the ADR-0010 grammar
   ([`cli.rs:207`](../../crates/client/src/cli.rs#L207)) — lowercase, digits, `-`, no dots. A type
   *may* be a reverse FQDN; an instance name may not, because it is a path component on three
   operating systems. This is why the two cannot share a key even if the semantics allowed it.

5. **A Selector on `service.name` aims at a type.** That is the point, and it is also the break:
   any existing Selector written against `service.name` was matching an instance name and will stop
   matching. As with the `host.arch` change in ADR-0017, this is silent unless stated, so it is named
   in `CHANGELOG.md` in the same change.

### Server-set labels

We will let the Server attach **labels** to an Agent — operator-set key/value pairs that join the
attribute set Selectors match against, for Configurations and packages alike, and that can never
restate anything the Agent reports.

6. **`PUT /api/v1/agents/{instance_uid}/labels` sets the whole set.** Body `{"labels": {…}}`,
   replacing what was there; an empty map clears them. Whole-map replacement rather than
   add-and-remove operations, for the same reason a package's Selector is set that way (ADR-0016):
   the resource is small, the write is idempotent, and an operator can see the resulting state in the
   request they sent. `AgentView` carries `labels`, so the fleet view shows them.

7. **Labels join what a Selector matches, everywhere one is matched.** Both Configuration targeting
   (ADR-0012) and package targeting (ADR-0016, and the fit of ADR-0017) resolve against
   an Agent's `AgentDescription`. Labels are merged into a derived **effective description** and that
   is what matching sees — one seam, both consumers, so a label cannot mean one thing for a
   Configuration and another for a package.

8. **A label can never restate a reported attribute.** This is the crux, and it is where this
   decision deliberately parts company with Bindplane. A key the Agent already reports is **refused**
   at the API, naming it; and at merge time the reported value wins regardless, with the shadowed
   label surfaced on the fleet row rather than silently dropped (ADR-0014's rule). The reason is the
   first force: labels that could override `os.type`, `host.arch`, or `service.name` would let a
   typo in the UI offer an Agent an artifact built for another machine. Labels annotate; they do not
   correct.

9. **Labels are the operator's, and they outlive the Agent record.** They are keyed by Instance UID —
   the only identity that is unique across a fleet, since `service.instance.name` is a name an
   operator may reuse on every host (point 2) — and persisted, so they survive a Server
   restart. In particular **forgetting an Agent (ADR-0026) does not clear its labels**: forgetting
   drops what the Server *learned*, and a label is something the operator *decided*. A host that
   comes back is in the ring it was put in. Clearing them is its own act: an empty map.

10. **They never travel to the Agent.** No new capability, no new message, nothing added to what the
   Client reports about itself. A label is an input to matching on the Server, and the Agent
   experiences it the only way that matters — as the Configuration and the packages it is offered.
   That also keeps the reported attributes honest: everything an Agent reports is still something it
   observed about itself.

11. **Setting labels takes effect at once.** The write bumps the push revision the WebSocket loops
   watch, exactly as editing a Configuration does, so a connected Agent that has just been moved into
   a ring receives its new Configuration on the spot rather than at its next poll.

12. **Stored beside the Configurations, one file per Agent.** `<config_dir>/labels/<instance-uid>.json`
   — a directory the Configuration store's loader ignores, and one file per Agent so a write touches
   nothing else and clearing is a deletion. Configurations already persist here and labels are the
   same kind of thing: operator intent about the fleet, which must be there after a restart.

### The host's network addresses, CPU model, and OS build

We will report `host.ip` and `host.mac` from every Agent this Client presents, as the conventions
define them.

13. **Enumeration** comes from `sysinfo`, which the Client already carries for its own telemetry
   (ADR-0024) — the `network` feature is enabled on the existing dependency; no new crate.

14. **Loopback interfaces are excluded whole** — their MACs too; an unspecified MAC (`00-…-00`) is
   no answer. Everything else is best effort: a host with nothing to report omits the key rather
   than reporting an empty array.

15. **Formats are the conventions'**: IPv4 dotted-quad and IPv6 RFC 5952 (both what `IpAddr`'s
   `Display` writes), MACs hyphen-separated uppercase. Both lists are deduplicated and sorted so an
   unchanged host never re-reports a description over enumeration order.

16. **Read live on each description**, not once at start, so a DHCP move is reported.

17. **Arrays stay arrays on the wire** (`ArrayValue` of strings — the typed original); the Server's
   read-only view joins string arrays with a comma. Selectors keep matching string values only —
   an array-valued attribute is displayed and searched, not matched.

Two more conventions ride along, both plain strings and both from sources already paid for:

18. **`host.cpu.model.name`** — the processor's model designation, from the same `sysinfo`; read
   once, since hardware does not change under a running process.

19. **`os.build_id`** — the build behind `os.version`, read where the platform's existing `os.*`
   answer already carries it: os-release's `BUILD_ID` (absent on distributions that stamp none),
   `sw_vers`' BuildVersion, and on Windows the build components of the version line
   (`10.0.26100.2033` → `26100.2033`, matching the conventions' Windows example `22621`).

## Alternatives considered

### For the type and the instance name

- **Leave `service.name` as the instance name and add `service.type`.** Smaller and breaks no
  Selector. Rejected: it puts this project's private key in the position the Baseline already
  defined, so a Collector's `opampextension` — which reports `service.name` and knows nothing about
  `service.type` — would keep overwriting the instance name with its type, and the fleet-view
  collapse this decision exists to fix would survive. It also diverges from the one reference
  implementation checked, for no gain beyond avoiding a rename.
- **Report no human instance name at all**, matching the Baseline exactly: type in `service.name`,
  identity in `service.instance.id`, host in `host.name`. Genuinely tempting, and the most
  conformant. Rejected because a UID is not a name a person can use, and several Supervisors share
  one `host.name`, so an operator managing three Collectors on one host would have three rows with
  the same type, the same host, and two UUIDs to tell apart. The Baseline's own escape hatch for
  exactly this is the user-defined non-identifying attribute clause used in point 2.
- **Use `host.name` as the instance name.** Free, standard, already reported. Rejected on the same
  case: it is a property of the machine, and Supervisor Mode's whole premise (ADR-0003) is *n*
  Agents on one machine.
- **Put the instance name outside `AgentDescription`, as Bindplane does with `agent_name`.** The
  closest thing to the prior art. Rejected: it needs a channel this project does not have — a
  product-specific field, a header, or a Server-side store keyed by UID — and it makes the name
  invisible to Selectors, when pinning one Agent by name is precisely what ADR-0016 twice offers as
  the way to aim a rollout at a single host.
- **Derive the type from `<program> --version` output** — the first token of
  `otelcol-contrib version 0.114.0` is `BuildInfo.Command`, i.e. exactly `dist.name`. Rejected: the
  version probe works because SemVer is a strict grammar recognisable anywhere in free text
  ([`process.rs:606`](../../crates/client/src/supervisor/process.rs#L606)), and a name has no
  grammar — `Fluent Bit v3.1.0` and `promtail, version 3.0.0 (branch: main)` both defeat "the token
  before *version*". The upstream OpAMP Supervisor had the same option and also declined it,
  bootstrapping through the extension instead. Point 1's configuration fallback gets the
  same value with none of the guessing.
- **Bootstrap the type by starting the Collector once with a generated opamp-extension-only config**,
  as the upstream Supervisor does within its `bootstrap_timeout`. Rejected for now on
  simplicity-first grounds: it adds a process start to the start path and only works for
  distributions that *have* the extension — which are exactly the ones that already report their
  type through the fold. It solves nothing that point 1 does not, for the cases that
  need solving. Worth revisiting only if a case appears where the extension exists but connects too
  late.

### For Server-set labels

- **Let a Server label win over a reported attribute** — what Bindplane does: once an agent's labels
  are *bootstrapped*, *"labels in the agent description are not applied"*, and an `overwrite` flag
  exists for changing a pre-existing value. Rejected on the first force. Bindplane can afford it
  because its collector fit is not attribute-driven the way this project's is; here `os.type`,
  `host.arch`, and `service.name` choose which binary an Agent installs (ADR-0017), so a
  label that outranks them turns a mislabelling into a mis-installation. Refusing the collision costs
  an operator one error message and removes that failure entirely.
- **Flat tags rather than key/value pairs** — Elastic Fleet's model, where an Agent carries a list of
  strings and the API adds and removes them. Simpler to type. Rejected: this project's Selectors are
  equality over key/value attributes (ADR-0012), so a flat tag would need a second matching rule
  beside the one that already exists. Bindplane's labels are key/value for the same reason, and its
  progressive rollout stages match *"all collectors whose labels include every label specified by the
  stage"* — which is precisely this project's Selector semantics already.
- **Add/remove operations** (`tagsToAdd` / `tagsToRemove`, as Elastic's bulk endpoint takes).
  Genuinely better for bulk work, and the natural companion to a fleet-wide "move these fifty hosts
  to stable". Rejected *now* as the first shape: a whole-map `PUT` is one operation with no ordering
  questions, and bulk deserves a decision of its own rather than being smuggled in as a second body
  format.
- **Push labels down to the Client and have it report them**, so the fleet view's attributes remain a
  single list. Rejected twice: it makes an Agent report as *observed* something it was merely told,
  and it puts a round trip between labelling a host and being able to target it — so the first
  Configuration aimed at a fresh label would match nobody.
- **Key labels by `service.instance.name`** so they survive a re-keyed Instance UID. Tempting, since
  the UID is exactly what `AgentIdentification` may change. Rejected: that name is the operator's name
  for an Agent within a Client (point 2) and nothing makes it unique across hosts, so
  labelling one host would silently label its namesakes. The Instance UID is the identity the whole
  Server is keyed on.
- **A separate `labels_dir` in `server.toml`.** Consistent with `config_dir` and `packages_dir`, and
  rejected only on surface: it is a third path an operator must know, back up, and get right, for data
  that belongs to the same store as the Configurations. It can be split out later without changing
  what a label means.
- **Do nothing and let operators put `rollout` in `client.toml`.** It works — for a fleet small
  enough to reach by hand. Rejected because that is the definition of the problem this project exists
  to solve, and because ADR-0017 has already narrowed Selectors to almost exactly this job.

### For the host's addresses

- **A dedicated interface-enumeration crate** (`if-addrs`, `mac_address`, `network-interface`) —
  a new dependency for what an existing one's feature flag already provides.
- **Joined strings on the wire** — renders everywhere without a Server change, but bakes a display
  choice into the protocol and departs from the conventions' declared type; the view is the place
  to join.
- **The Server records the connection's peer address** — no Client change, but it reports the NAT
  or the Gateway rather than the host, precisely the invention ADR-0025 forbids the hop.

## Sources / Prior art

- The Baseline's `AgentDescription`
  ([`opamp.proto:690-727`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L690-L727)) — the
  direct authority for the type and the instance name: `service.name` "should be set to a reverse
  FQDN that uniquely identifies the Agent type, e.g. `io.opentelemetry.collector`",
  `service.instance.id` separately as what identifies the Agent, and the non-identifying clause
  admitting "any user-defined attributes that the end user would like to associate with this Agent"
  that point 2 relies on.

- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the behavioural oracle for a shipping OpAMP server: an Agent Type identified by `dist.name`
  "reported by the collector via OpAMP", with the human `agent_name` kept as a separate field rather
  than folded into the same attribute.
- [`opampextension` `opamp_agent.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go)
  — `createAgentDescription()` sets exactly three identifying attributes (`service.instance.id`,
  `service.name`, `service.version`) and derives the rest from the host. This is what actually
  arrives at a Supervisor Endpoint, and therefore what the fold in point 2 has to
  coexist with.
- [OpAMP Supervisor `config.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/config/config.go)
  — the comparable configuration surface: `agent.description.identifying_attributes` lets an
  operator state identifying attributes directly, which is the same need point 1's
  `service_name` key answers, scoped to the one key that has a defined meaning.
- [PR #38809 — Control Collectors with only the OpAMP extension](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38809)
  — the bootstrap mechanism weighed and rejected in Alternatives, and the evidence that upstream
  chose the extension over parsing a CLI flag.
- [OpenTelemetry semantic conventions — `service`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/service.md)
  — checked to confirm what point 2 admits: the registry defines `service.name`,
  `service.namespace`, `service.version`, and `service.instance.id`, and has **no** attribute for a
  human-readable instance name. `service.instance.name` is this project's, deliberately named to
  parallel the one that exists.

- **[Bindplane labels and the agents API](https://docs.bindplane.com/cli-and-api/api/agents)** — the
  closest precedent for labels, and the one this decision measures itself against. Labels are
  key/value, set per agent (`PATCH /agents/{id}/labels`) or in bulk (`PATCH /agents/labels`), with an
  `overwrite` flag documented as *"if true, overwrite any existing labels with the same names"* and an
  empty value meaning the label is deleted. Its collision rule is the opposite of point
  8: `LabelsBootstrapped` *"is true if the labels have been 'bootstrapped' … When this
  is true, labels in the agent description are not applied"* — the Server wins. Point
  8 explains why this project cannot follow it.
- **[Bindplane progressive rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/progressive-rollouts)**
  — what labels are *for*, and confirmation that the matching semantics needed are the ones this
  project already has: a stage *"will include all collectors whose labels include every label
  specified by the stage"*, and an operator advances the rollout stage by stage. It also shows where
  this decision stops: staged rollouts as a first-class object are a further step, and labels are
  their prerequisite.
- **[Elastic Fleet agent tags](https://www.elastic.co/docs/reference/fleet/filter-agent-list-by-tags)**
  and the [bulk update endpoint](https://www.elastic.co/docs/api/doc/kibana/operation/operation-post-fleet-agents-bulk-update-agent-tags)
  — the other model: flat string tags, added and removed in bulk
  (`POST /api/fleet/agents/bulk_update_agent_tags` with `tagsToAdd` / `tagsToRemove`, up to 10 000
  agents), used for filtering the agent list. The source of the second and third label alternatives:
  its bulk shape is better than a per-agent `PUT` for fleet-scale work, and its flat tags are worse
  for a Selector model that is already key/value.
- This repository: [ADR-0017](0017-package-artifacts.md), which named
  labels as the next decision; [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)
  and [ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md) for the Selector model labels feed;
  [ADR-0017](0017-package-artifacts.md) for the platform fit point 8
  protects; [ADR-0026](0026-the-servers-agent-records.md), whose forget point 9 deliberately
  does not undo; and [ADR-0014](0014-server-driven-connection-settings.md) for the rule that what is
  dropped gets said.

- OpenTelemetry semantic conventions, host registry (`host.ip`, `host.mac`, `host.cpu.model.name`,
  formats and loopback exclusion):
  <https://opentelemetry.io/docs/specs/semconv/registry/attributes/host/> (retrieved 2026-08-11).
- OpenTelemetry semantic conventions, os registry (`os.build_id`, examples including Windows'
  `22621`): <https://opentelemetry.io/docs/specs/semconv/registry/attributes/os/> (retrieved
  2026-08-11).
- `sysinfo` 0.37 `Networks` API (`mac_address()`, `ip_networks()`), gated behind its `network`
  feature.

## Consequences

- Positive: **an operator's name for an Agent survives its Managed Process reporting for itself.**
  The defect of a working `opampextension` erasing `otelcol-edge-01` and collapsing three fleet rows
  into three identically named ones is fixed at its cause rather than worked around.
- Positive: **the fleet can be aimed by what an Agent *is*.** A Selector of
  `{"service.name": "otelcol-contrib"}` reaches the Collectors of that distribution and nothing
  else, with no per-host attribute to maintain — the role half of the "is this artifact meant for
  this Agent" question that ADR-0017 answered for the platform. It does not become mandatory the way
  platform fit did, so the mismatched-role package is discouraged, not made impossible.
- Positive: the type is available for the distributions that cannot report it, through configuration
  rather than through a probe that would sometimes be wrong.
- Positive: one fewer overloaded key. `name` in `client.toml` keeps exactly the local meaning its
  grammar was designed for, and stops being a protocol-visible identifier whose value a process can
  overwrite.

- Positive: the last per-host wiring goes away. A rollout ring becomes a Server-side decision, so
  moving a host between rings is one API call instead of an edit and a restart on that host — and it
  applies to both halves of what a Selector aims, configuration and software.
- Positive: labels cost no protocol surface at all. No capability, no message, nothing the Agent has
  to support — they are a Server-side input to a matching function that already exists.
- Positive: a package or Configuration Selector written for `rollout = "canary"` has something to
  match that an operator can change, which is what ADR-0016's staged-rollout argument assumed all
  along.

- Positive: the fleet view shows each host's addresses — generically in the attribute chips, and
  at a glance in a dedicated Network column (first address of each kind, the rest in the tooltip);
  the attributes are searchable; the wire stays convention-shaped for any OTel-aware consumer.

- Negative / trade-offs: **a Selector on `service.name` changes meaning and silently stops
  matching.** This is the same failure mode ADR-0017 accepted for `host.arch` and it needs the same
  treatment — a `CHANGELOG.md` entry, because nothing in the system can detect it.
- Negative / trade-offs: **`AgentView` grows a field and the UI's name column changes what it
  shows.** Generated API clients and anything reading `service_name` as a display name see different
  content under an unchanged key, which is worse than a rename would be. A rename of the JSON field
  is worth considering at review.
- Negative / trade-offs: **`service.instance.name` is a key this project invents.** Every invented
  attribute is one a future convention may contradict, and it is matched raw by Selectors, so
  renaming it later breaks them exactly as point 5 breaks `service.name`.
- Negative / trade-offs: the resolution order in point 1 means an Agent's reported type
  can change when a Collector gains the extension — from the configured or file-name value to
  `dist.name`. That is the correct value winning, but it is still a Selector that stops matching at
  an unrelated moment.

- Negative / trade-offs: **the fleet's targeting depends on Server-side state that no Agent
  reports.** Reading `client.toml` on a host does not tell you what that host will be sent. That is
  the point, and it is also a new place to look when a rollout surprises someone — which is why the
  labels are on the fleet row rather than only in the store.
- Negative / trade-offs: labels are keyed by Instance UID, and `AgentIdentification` may re-key an
  Agent (on a duplicate UID, which this Server does deliberately). A re-keyed Agent loses its labels
  and silently falls back to whatever its reported attributes match. Rare, and the fleet row shows the
  empty set, but it is a real hole that a name-based key would not have — and that a name-based key
  would have paid for with cross-host collisions.
- Negative / trade-offs: refusing a colliding key (point 8) means an operator who
  genuinely wants to correct a badly reported attribute cannot. That is deliberate — the fix belongs
  on the host, where the wrong value is — but it will read as an arbitrary restriction the first time
  someone hits it, so the error says which attribute and where it came from.
- Negative / trade-offs: labels outliving a forgotten Agent (point 9) means a
  decommissioned host's labels stay in the store. They are small, and they are what makes a returning
  host land in the right ring, but nothing prunes them.

- Negative / trade-offs: network addresses leave the host and sit in the fleet view — visible to
  whoever can read the API (the fleet is an operator tool, that is its purpose, but it is data that
  is otherwise not transmitted). Virtual adapters (bridges, tunnels) appear alongside physical ones —
  best effort reports what the platform says, it does not curate. Selectors cannot match
  array-valued attributes.

- Follow-ups: whether the Server should warn when a package's Selector names a `service.name` no
  Agent in the fleet reports, which is the type-side equivalent of the fits-no-Agent warning ADR-0017
  left as a follow-up.
- Follow-ups: bulk labelling, which is where Elastic's shape is plainly better than a per-agent `PUT`
  and which a fleet of any size will want; staged rollouts as a first-class object on top of labels,
  which is what Bindplane builds with them and which would answer "roll this out to canary, then to
  the rest" rather than leaving the operator to move the labels; pruning labels for Agents nobody has
  seen in a long time, which meets the retention question ADR-0026 also left open; and set-based
  Selector operators (ADR-0012's own follow-up), which labels make considerably more useful.

- Follow-ups: the cloud-shaped conventions (`host.image.*`, `host.type`) remain unreported — they
  come from provider metadata services, a separate decision if the fleet ever needs them.
