# ADR-0033: An Agent's type and its instance name are two attributes — `service.name` carries the type, `service.instance.name` the operator's name

- **Status:** 🟢 accepted
- **Date:** 2026-08-08
- **Deciders:** Markus Brigl

Extends [ADR-0011](0011-supervisor-mode-hexagonal-core-and-plugins.md) and
[ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md) rather than replacing
either: a Supervisor is still one Agent, and a Selector still matches reported attributes by
equality. What changes is *what one attribute means*. The `name` grammar of
[ADR-0010](0010-client-os-service-and-cli.md) and the platform fit of
[ADR-0031](0031-per-platform-package-variants.md) are untouched.

## Context

A `[[supervisor]]` block's `name` is used for three unrelated things at once: it is the directory
under `<supervisor_dir>/` that the Supervisor owns (ADR-0021), it is the uniqueness key across
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
are Collectors", and ADR-0017's aiming has no attribute for it. A rollout that should reach the
Collectors and nothing else has to be expressed through an operator-invented attribute that every
host must carry, which is the per-host wiring ADR-0017 exists to remove. ADR-0031 closed the same
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

## Decision

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

   For the Client's own Agent the type is the constant `opamp-fleet-client` (ADR-0028), not the
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
   type on the sub-line, the reverse of today. The fallback chain becomes instance name → type →
   UID, so a row is never blank and never collapses onto its neighbours
   ([`fleet.rs:876`](../../crates/server/src/fleet.rs#L876),
   [`index.html:351`](../../crates/server/static/index.html#L351)).

4. **Nothing changes about the block `name` locally.** It stays the directory name and the
   uniqueness key, and it stays bound by the ADR-0010 grammar
   ([`cli.rs:207`](../../crates/client/src/cli.rs#L207)) — lowercase, digits, `-`, no dots. A type
   *may* be a reverse FQDN; an instance name may not, because it is a path component on three
   operating systems. This is why the two cannot share a key even if the semantics allowed it.

5. **A Selector on `service.name` now aims at a type.** That is the point, and it is also the break:
   any existing Selector written against `service.name` was matching an instance name and will stop
   matching. As with the `host.arch` change in ADR-0031, this is silent unless stated, so it is named
   in `CHANGELOG.md` in the same change.

## Alternatives considered

- **Leave `service.name` as the instance name and add `service.type`.** Smaller and breaks no
  Selector. Rejected: it puts this project's private key in the position the Baseline already
  defined, so a Collector's `opampextension` — which reports `service.name` and knows nothing about
  `service.type` — would keep overwriting the instance name with its type, and the fleet-view
  collapse this ADR exists to fix would survive. It also diverges from the one reference
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
  invisible to Selectors, when pinning one Agent by name is precisely what ADR-0017 twice offers as
  the way to aim a rollout at a single host.
- **Derive the type from `<program> --version` output** — the first token of
  `otelcol-contrib version 0.114.0` is `BuildInfo.Command`, i.e. exactly `dist.name`. Rejected: the
  version probe works because SemVer is a strict grammar recognisable anywhere in free text
  ([`process.rs:606`](../../crates/client/src/supervisor/process.rs#L606)), and a name has no
  grammar — `Fluent Bit v3.1.0` and `promtail, version 3.0.0 (branch: main)` both defeat "the token
  before *version*". The upstream OpAMP Supervisor had the same option and also declined it,
  bootstrapping through the extension instead. Point 1's configuration fallback gets the same value
  with none of the guessing.
- **Bootstrap the type by starting the Collector once with a generated opamp-extension-only config**,
  as the upstream Supervisor does within its `bootstrap_timeout`. Rejected for now on
  simplicity-first grounds: it adds a process start to the start path and only works for
  distributions that *have* the extension — which are exactly the ones that already report their
  type through the fold. It solves nothing that point 1 does not, for the cases that need solving.
  Worth revisiting only if a case appears where the extension exists but connects too late.

## Sources / Prior art

- The Baseline's `AgentDescription`
  ([`opamp.proto:690-727`](../../crates/opamp/proto/v0.19.0/opamp/v1/opamp.proto#L690-L727)) — the
  direct authority for this decision: `service.name` "should be set to a reverse FQDN that uniquely
  identifies the Agent type, e.g. `io.opentelemetry.collector`", `service.instance.id` separately as
  what identifies the Agent, and the non-identifying clause admitting "any user-defined attributes
  that the end user would like to associate with this Agent" that point 2 relies on.
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the behavioural oracle for a shipping OpAMP server: an Agent Type identified by `dist.name`
  "reported by the collector via OpAMP", with the human `agent_name` kept as a separate field rather
  than folded into the same attribute.
- [`opampextension` `opamp_agent.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/extension/opampextension/opamp_agent.go)
  — `createAgentDescription()` sets exactly three identifying attributes (`service.instance.id`,
  `service.name`, `service.version`) and derives the rest from the host. This is what actually
  arrives at a Supervisor Endpoint, and therefore what the fold in point 2 has to coexist with.
- [OpAMP Supervisor `config.go`](https://github.com/open-telemetry/opentelemetry-collector-contrib/blob/main/cmd/opampsupervisor/supervisor/config/config.go)
  — the comparable configuration surface: `agent.description.identifying_attributes` lets an
  operator state identifying attributes directly, which is the same need point 1's `service_name`
  key answers, scoped to the one key that has a defined meaning.
- [PR #38809 — Control Collectors with only the OpAMP extension](https://github.com/open-telemetry/opentelemetry-collector-contrib/pull/38809)
  — the bootstrap mechanism weighed and rejected in Alternatives, and the evidence that upstream
  chose the extension over parsing a CLI flag.
- [OpenTelemetry semantic conventions — `service`](https://github.com/open-telemetry/semantic-conventions/blob/main/docs/registry/attributes/service.md)
  — checked to confirm what point 2 admits: the registry defines `service.name`,
  `service.namespace`, `service.version`, and `service.instance.id`, and has **no** attribute for a
  human-readable instance name. `service.instance.name` is this project's, deliberately named to
  parallel the one that exists.

## Consequences

- Positive: **an operator's name for an Agent survives its Managed Process reporting for itself.**
  The present defect — a working `opampextension` erasing `otelcol-edge-01` and collapsing three
  fleet rows into three identically named ones — is fixed at its cause rather than worked around.
- Positive: **the fleet can be aimed by what an Agent *is*.** A Selector of
  `{"service.name": "otelcol-contrib"}` reaches the Collectors of that distribution and nothing
  else, with no per-host attribute to maintain — the role half of the "is this artifact meant for
  this Agent" question that ADR-0031 answered for the platform. It does not become mandatory the way
  platform fit did, so the mismatched-role package is discouraged, not made impossible.
- Positive: the type is available for the distributions that cannot report it, through configuration
  rather than through a probe that would sometimes be wrong.
- Positive: one fewer overloaded key. `name` in `client.toml` keeps exactly the local meaning its
  grammar was designed for, and stops being a protocol-visible identifier whose value a process can
  overwrite.
- Negative / trade-offs: **a Selector on `service.name` changes meaning and silently stops
  matching.** This is the same failure mode ADR-0031 accepted for `host.arch` and it needs the same
  treatment — a `CHANGELOG.md` entry, because nothing in the system can detect it.
- Negative / trade-offs: **`AgentView` grows a field and the UI's name column changes what it
  shows.** Generated API clients and anything reading `service_name` as a display name see different
  content under an unchanged key, which is worse than a rename would be. A rename of the JSON field
  is worth considering at review.
- Negative / trade-offs: **`service.instance.name` is a key this project invents.** Every invented
  attribute is one a future convention may contradict, and it is matched raw by Selectors, so
  renaming it later breaks them exactly as point 5 breaks `service.name` now.
- Negative / trade-offs: the resolution order in point 1 means an Agent's reported type can change
  when a Collector gains the extension — from the configured or file-name value to `dist.name`. That
  is the correct value winning, but it is still a Selector that stops matching at an unrelated
  moment.
- Follow-ups: **server-side labelling.** Attributes usable for staged rollouts (`rollout = "canary"`)
  still live in a file on each host, so moving a host between rings is a file edit plus a restart —
  the per-host wiring ADR-0017 set out to remove, in the one place it remains. A decision on
  Server-set labels merged into an Agent's matchable attributes is the natural next one, and is
  independent of this ADR. Also open: whether the Server should warn when a package's Selector names
  a `service.name` no Agent in the fleet reports, which is the type-side equivalent of the
  fits-no-Agent warning ADR-0031 left as a follow-up.
