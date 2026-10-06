# ADR-0012: Selector-targeted Configurations with a content role and an optional Agent type, behind the OpenAPI-described REST API

- **Status:** 🟢 accepted
- **Date:** 2026-08-12
- **Deciders:** Markus Brigl

## Context

The specification names two goals that belong to one decision. Goal 9: a configuration can target a
subset of the fleet via a **Selector**. Goal 5: any UI can drive the fleet through an
**OpenAPI-described** REST API. Selectors force the API to grow a real resource model, and that
model is what the OpenAPI document must describe.

The forces are largely fixed. The specification demands hash-gated distribution (goal 3), forbids
inventing an abstraction over an agent's configuration language (non-goal), and defines the
vocabulary: **Selector**, **Remote configuration**. ADR-0005 binds axum; ADR-0032 puts the REST
API on the Operator plane's listener. ADR-0008 binds TOML with loud typo rejection for the Server's
and Client's own configuration files. ADR-0010 fixes a name grammar for things that become file
names. ADR-0011 has the Collector Supervisor pass **every config-map entry as its own `--config`
argument** and lets the Collector do its own merging — no YAML manipulation in Rust.

One protocol force does the heavy lifting: OpAMP's `AgentConfigMap` is a **map of named
configuration entries**, and `config_hash` identifies the map as a whole. Composing an Agent's
configuration out of several named parts is therefore the protocol's own mechanism, not an
invention of this project.

Prior art (see Sources) splits into two camps. **Exclusive assignment**: Elastic Fleet enrolls each
agent in exactly one policy — simple, but composition (a fleet-wide base plus a team-specific
overlay) is impossible and assignment is a workflow of its own. **Attribute matching**: Grafana
Fleet Management matches configuration pipelines to collectors by attributes, sorts multiple
matches **alphabetically by name**, and merges them into one configuration; BindPlane selects
agents by Kubernetes-style label selectors (`=`, `in`, `exists`, …) attached to a fleet; both let
operators attach extra labels/attributes on the agent side to steer matching. For the API side,
`utoipa` is the most widely adopted code-first OpenAPI generator in the Rust ecosystem, with
first-class axum bindings (`utoipa-axum`) that derive the document from the registered routes so
the two cannot drift.

**Not every entry is top-level configuration.** The Baseline (`v0.19.0`) carries
`AgentConfigFile.role`, *"the role of the content in the body field. The values and their semantics
are Agent type-specific."* The motivation upstream
([opamp-spec#184](https://github.com/open-telemetry/opamp-spec/issues/184), implemented by
[#350](https://github.com/open-telemetry/opamp-spec/pull/350)) is a case this project has. The
Collector takes several configuration files on its command line and merges them — which is exactly
how a composed configuration is delivered here: every Configuration in the map becomes one named
entry, and the Collector plugin writes each entry to the Supervisor's config directory and passes it
as its own `--config`. But a Collector configuration can also *reference* files that are not
configuration at all — a fragment pulled in with `${file:ruleset.yaml}`, a certificate, any artifact
read at startup. Those files must be on disk next to the configuration and must **not** be handed to
the Collector as `--config`. Without a role, every entry is treated as top-level configuration, so a
certificate distributed as a Configuration would be passed to the Collector as configuration and
break the process it was meant to configure.

Making the field usable touches the public contract: a Configuration is a REST API resource, and the
REST API is the thing goal 5 promises portals can generate clients from. Adding a field to it is not
an implementation detail, and neither is deciding what a role *means* to a Supervisor plugin: the
specification's non-goal *"Forking or extending the protocol"* forbids inventing protocol semantics,
so whatever this project understands by a role value has to be plugin-level convention, honestly
labelled as such.

**The Agent type is the other thing a Selector was made to carry.** `service.name` is a reported
attribute like any other, `matches()` compares it
([`configs.rs`](../../crates/server/src/configs.rs#L110-L125), tested at
[`configs.rs:311`](../../crates/server/src/configs.rs#L311-L334)), and the bundled UI offers
`service.name=…` as a clickable Selector chip. So "limit a Configuration to an Agent type" is
possible as a Selector pair — but only as an opt-in pair buried among the aim, and
[ADR-0016](0016-a-package-is-a-versioned-set.md) has already judged that shape
for packages: "a rule that is only ever right when remembered is not the rule this needs."
[ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) made the Agent type a
fact every Agent reports.

What forgetting the pair does: an empty Selector matches every Agent (clause 2), and every Agent
this Client presents declares `AcceptsRemoteConfig` unconditionally
([`agent.rs:30`](../../crates/client/src/supervisor/agent.rs#L30)) — the supervised Collectors,
every Foreign Agent, and the Client's own self-Agent alike. A Collector YAML saved without a
`service.name` pair is therefore composed into the config map of every one of them: each Foreign
Agent is restarted into a configuration entry its program cannot read (the apply grace catches it,
at the price of a restart-and-rollback cycle per Agent), and the self-Agent stores it as applied
([`agent.rs:852`](../../crates/client/src/supervisor/agent.rs#L852)). The blast radius is smaller
than a wrong-type *binary* — ADR-0016's case — because a configuration is health-gated per entry
and rolled back, not installed over a program. It is still a fleet-wide churn reachable by
omitting one pair.

There is also a purity cost ADR-0016 already named for packages: with the type expressed as a
Selector pair, the Selector carries *what kind of Agent* and *which of them* in one field, and the
two cannot be read apart in the UI or the API.

## Decision

We will replace a single fleet-wide configuration with named **Configurations**, each carrying a
**Selector** matched against the attributes an Agent reports, an optional content **role**, and an
optional **Agent type**; an Agent's Configurations are delivered **as named entries of one
`AgentConfigMap`**; and we will make the REST API the project's contract: **versioned under
`/api/v1`, described by an OpenAPI document generated code-first with `utoipa`**.

### The Configuration and its Selector

1. **The Configuration resource.** A Configuration is `{name, selector, body}`, plus the optional
   `role` (clause 8) and `service_name` (clause 13): a name following the ADR-0010 grammar (it
   becomes a file name and a config-map key), a Selector, and an opaque text body (the Managed
   Process's own format — never interpreted by the Server, per the specification's non-goal). A
   fleet-wide configuration is the degenerate case: a Configuration whose Selector is empty.

2. **Selector semantics: equality, AND, over reported attributes.** A Selector is a string-to-string
   map; an Agent matches when **every** pair equals an attribute the Agent reported in its
   `AgentDescription` (identifying and non-identifying alike, e.g. `service.name`, `os.type`,
   `host.arch`, `service.instance.id` for pinning a single Agent). The **empty Selector matches
   every Agent**. An Agent that has not reported a description yet matches only empty Selectors.
   Set-based operators (`in`, `notin`, `exists` — the BindPlane/Kubernetes grammar) are deferred:
   they are additive and nothing needs them yet.

3. **Operator-defined attributes on the Client.** `client.toml` gains an optional `attributes`
   table (string → string) — top-level for the Client's self-Agent and per `[[supervisor]]` block —
   folded into the non-identifying attributes of the respective Agent's description. Without a way
   to tag an Agent (`env = "prod"`), Selectors could only target what the code happens to report.
   Reported attributes win over configured ones on key collision. Server-set labels join the
   matched attribute set as well (ADR-0027).

4. **Composition by the protocol, merging by the process.** An Agent's Remote configuration is one
   `AgentConfigMap` whose entry keys are the Configuration names, in name order (deterministic like
   Grafana FM's alphabetical rule); `config_hash` is the SHA-256 over the sorted `(name, body)`
   pairs, so the hash gate (goal 3) works per Agent. Which Configurations enter that map is the
   Agent's rollout assignment, with matching computing the candidate (ADR-0030 clauses 2 and 3).
   The Server never merges bodies: the Collector Supervisor passes each entry as its own `--config`
   and the Collector merges natively (ADR-0011); a Custom Supervisor receives the named files and
   decides plugin-specifically. Multi-entry config maps are handled end to end by the Client.

5. **No match, no offer.** An Agent matching no Configuration is sent nothing and keeps running
   what it already runs — exactly goal 9's wording. How an assigned Configuration is taken away
   again is decided by ADR-0030 clause 7.

6. **Persistence: one JSON file per Configuration.** The Server persists each Configuration as
   `<config_dir>/<name>.json` (serde serialization of the API resource), written atomically
   (temp file + rename), restored at startup; `DELETE` removes the file. The `config_dir` setting
   in `server.toml` defaults to `fleet-configs/`. No database: a handful of small files needs none.
   The store also retains every revision an assignment still references (ADR-0030 clause 2).

7. **The REST API v1.** Routes live under `/api/v1`: `GET /api/v1/agents` (the fleet, including
   every reported attribute and the names plus hash of the Configurations currently matching each
   Agent — the operator must see what a Selector would select; ADR-0030 clause 4 adds what is
   waiting per Agent),
   `GET /api/v1/configurations`, and `GET`/`PUT`/`DELETE /api/v1/configurations/{name}`. The
   OpenAPI document is generated code-first with `utoipa`/`utoipa-axum` — handlers and schemas
   annotated where they live, routes registered once, so document and behaviour cannot drift — and
   served at `/api/v1/openapi.json`. There are no unversioned routes, and the bundled UI uses v1.
   No Swagger UI is bundled: the document is the contract. The routes the Operator plane serves
   are listed in ADR-0032.

### The content role

8. **An optional `role` string on the Configuration resource** is carried through the REST API into
   the `AgentConfigFile.role` of every entry composed from it. Supervisor plugins honour two values
   (clauses 9 and 10) while passing the field on verbatim: the value travels unchanged in
   `AgentConfigFile.role`, so an Agent that interprets roles itself — a Collector with its own
   `opampextension`, reached through the Supervisor Endpoint — sees exactly what the operator set,
   not this project's reading of it.

9. **Empty (the default)** — top-level configuration: written to the Supervisor's config directory
   and passed to the Managed Process as configuration.

10. **`supplementary`** — written to the same directory under the Configuration's name, but **not**
    passed as configuration. It is content the Managed Process reads by path, not by being told
    about it: fragments, certificates, rule files.

11. **Any other value is written like `supplementary` and reported as received**; it is never
    guessed at. A Supervisor kind may define a vocabulary of its own: ADR-0033 clause 23 gives
    `icinga2` the role `main`.

12. **`role` is absent from a Configuration's JSON and stays absent in responses when unset**, so
    every stored Configuration without it and every generated client keeps working unchanged.

### The Agent type

13. **`service_name` joins the Configuration**, beside `selector`, `body`, and `role` — in the
    `PUT /api/v1/configurations/{name}` body and the persisted JSON, absent when unset (like
    `role`, clause 12). No sub-resource: unlike a package's type it is not identity (ADR-0016) and
    not immutable, it is one more field of the one writable resource.

14. **Fit before aim.** Composition drops every Configuration whose `service_name` is set and is
    not the Agent's reported `service.name`, then runs the Selector over what is left. The type is
    compared **raw**, no canonicalisation — ADR-0016 point 5's rule, for its reason: there is no
    canonical set of Agent types to normalise against.

15. **Unset means every type.** The fleet-wide Configuration — clause 1's degenerate case — and
    cross-type `supplementary` content (a certificate bundle, clause 10) stay expressible. This is
    deliberately *not* ADR-0016's "no type, offered to nobody": there the unset state hid a
    mismatched-binary outage, here it is the documented base case of the resource.

16. **An Agent that reports no `service.name` matches only untyped Configurations.** Equality
    against a missing attribute fails, exactly as a Selector pair against a missing attribute
    fails — no new rule, stated for the record.

17. **A store without the field loads unchanged.** Absent field, untyped, same matching; no hash
    moves, no Managed Process restarts on upgrade.

18. **The bundled UI shows the type as its own input**, beside the Selector, suggesting the types
    the fleet currently reports — so choosing one is a click, not a remembered convention — and
    shows it in each Configuration's chip.

## Alternatives considered

- **Exclusive assignment — one policy per Agent (Elastic Fleet model).** Rejected. It forbids
  composition (fleet-wide base + narrower overlay), turns membership into a managed workflow and
  API surface of its own, and still needs grouping criteria — which are Selectors by another name.
- **Priority ordering with first-match-wins (exactly one Configuration per Agent).** Rejected.
  A total order between unrelated Configurations is hidden coupling, and it forecloses the
  composition the protocol's own config map provides for free. Grafana FM's deployed answer to
  overlap is deterministic ordering plus merge, not exclusion.
- **Server-side body merging (Grafana Alloy style).** Rejected. Merging YAML (or any format) on
  the Server is precisely the specification's non-goal — an abstraction over the agent's
  configuration language — and would drag a YAML stack into Rust. The Collector merges its own
  `--config` list; a Foreign Agent's plugin knows its own format.
- **Set-based Selector operators now.** Deferred as YAGNI. Equality-AND covers the concrete needs
  (platform, name, operator tags, single-Agent pinning); a richer grammar extends the same field
  compatibly when a need appears.
- **A hand-written, spec-first OpenAPI document.** Rejected. A document maintained beside the code
  drifts exactly like an unchecked conformance matrix; `utoipa` derives it from the routes and
  schemas at compile time, making drift a compile error rather than a review hope.
- **`aide` instead of `utoipa`.** Rejected. Both integrate with axum; `utoipa` is the most widely
  adopted, actively maintained choice with dedicated axum bindings (`utoipa-axum`), and its
  code-first model fits a contract that must follow the code. Not a one-way door — the document,
  not the generator, is the contract.
- **SQLite for Configuration storage.** Rejected for now. It buys transactions and history for a
  resource counted in dozens; atomic per-file writes carry the present need without a database
  dependency. Audit/history can supersede this in its own ADR.
- **Unversioned `/api/*` routes (or versioning by header).** Rejected. Renaming after portals have
  generated clients is the expensive variant. Path versioning is the form generated clients and
  reverse proxies handle most simply.
- **Leave `role` unset.** Rejected. It leaves goal 7's heterogeneous fleet unable to receive a
  certificate or a config fragment, and leaves a field of the protocol permanently unexpressed,
  which goals 12 and 13 push against.
- **A separate `supplementary` boolean on the Configuration resource.** Rejected. It reads more
  clearly than a free string, but it cannot carry any other agent type's vocabulary, and it would
  have to be mapped onto `role` on the wire anyway — inventing a second model of the same thing.
- **A dedicated "supplementary files" resource, distinct from Configurations.** Rejected as bigger
  than the problem: Selector targeting, hashing, persistence, and the whole REST surface would be
  duplicated for content that differs from a Configuration in exactly one respect.
- **Pass `role` through the API but have plugins ignore it.** Rejected as the worst of both: the
  operator can set a role, the Server dutifully ships it, and the Supervisor still hands a
  certificate to the Collector as `--config`. A field that changes nothing where it must change
  something is a trap.
- **Wait for the Collector's supervisor to define role values, then follow.** Tempting, and the
  reason the vocabulary here is kept to one value. Rejected as a blocker: nothing upstream defines
  values yet, and the case is reachable. Divergence is handled the way this project handles it
  elsewhere — a superseding ADR when upstream settles.
- **Leave the Agent type to the Selector and document it.** Works at zero cost, and unlike
  ADR-0016's packages the failure is health-gated, not a bricked binary. Rejected: the failure is
  still fleet-wide churn behind one forgotten pair, the type stays unreadable apart from the aim,
  and the UI cannot guide what it cannot distinguish. ADR-0016 rejected this shape with the blast
  radius as the *tiebreaker*, not the argument.
- **A mandatory type — untyped Configurations reach nobody** (ADR-0016 point 8 verbatim).
  The consistent mirror. Rejected: it abolishes the fleet-wide degenerate case (clause 1) and
  cross-type supplementary content, both legitimate and in use; and it needs a migration in which
  every existing Configuration goes silently inert on upgrade, for a state that is not unsafe. For
  packages the unset state hid an outage; here it is a feature with a name.
- **Require the type only for top-level Configurations and not for `supplementary` ones.**
  Splits the safety rule along the role, which the Server treats as opaque beyond one known value
  — a conditional mandate hanging off a field whose vocabulary this project deliberately does not
  own. Rejected for rule complexity that buys the mandate only partially.
- **Pattern or prefix matching for the type** (`otelcol*`). Rejected in ADR-0016 for types;
  nothing about Configurations weakens that reasoning — two types are two Configurations.

## Sources / Prior art

- [Grafana Fleet Management architecture](https://grafana.com/docs/grafana-cloud/send-data/fleet-management/introduction/architecture/)
  — attribute matching between collectors and configuration pipelines; multiple matches sorted
  alphabetically by name and merged into one remote configuration: deterministic ordering plus
  process-native merge, the model this decision adopts.
- [Bindplane: Fleets](https://docs.bindplane.com/feature-guides/deployment-and-management/fleets)
  — Kubernetes-style label selectors (`=`, `!=`, `in`, `notin`, `exists`) matching agent labels to
  a fleet's configuration; agents carry operator-set labels for exactly this purpose.
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the comparable product models the Agent Type as a first-class object and hangs what an agent
  may receive off it; already cited by ADR-0016 as evidence that type-as-a-property is the shape
  that holds up in a shipping fleet manager.
- [Elastic Agent policies](https://www.elastic.co/docs/reference/fleet/agent-policy) — the
  exclusive-assignment alternative: each agent is enrolled in exactly one policy.
- [Kubernetes labels and selectors](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/)
  — the equality-based/set-based selector grammar the ecosystem converged on; this decision takes
  the equality subset first.
- [OpAMP specification: Configuration](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — `AgentConfigMap` as a map of named entries and `config_hash` over the whole offer: the
  protocol's own composition mechanism (Baseline, see [`CONFORMANCE.md`](../CONFORMANCE.md)).
- [OpAMP specification `v0.19.0`, `AgentDescription`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — `service.name` as the attribute that "uniquely identifies the Agent type", the value the type
  fit reads.
- [`AgentConfigFile` in the Baseline](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/proto/opamp/v1/opamp.proto)
  (`v0.19.0`) — the `role` field, and its "values and semantics are Agent type-specific" wording.
- [opamp-spec#184](https://github.com/open-telemetry/opamp-spec/issues/184) — the originating issue:
  top-level configuration versus supplementary content, with the Collector's `--config` merging and
  `${file:...}` substitution as the concrete case; it also weighs the alternatives upstream
  considered (a second `AgentRemoteConfig` field, a separate file list) before settling on a role
  string.
- [opamp-spec#350](https://github.com/open-telemetry/opamp-spec/pull/350) — the change as released.
- [OpenTelemetry Collector configuration](https://opentelemetry.io/docs/collector/configuration/) —
  multiple `--config` flags are merged, and `${file:...}` reads content that is not itself passed as
  configuration: the behaviour the two roles map onto.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) (`v0.23.0`) and the Collector's
  `opampsupervisor` — checked as the behavioural oracle this project follows: neither defines role
  values yet, which is why the role vocabulary is kept to a single value and expects to be
  superseded rather than extended if upstream settles on different words.
- [utoipa](https://github.com/juhaku/utoipa) and
  [`utoipa-axum`](https://docs.rs/utoipa-axum/latest/utoipa_axum/) — code-first, compile-time
  OpenAPI generation with axum bindings; the most widely adopted Rust option.
- [ADR-0016](0016-a-package-is-a-versioned-set.md) — the model for the type:
  a first-class fit step in front of the Selector, compared raw; and the argued divergence points
  (mandatory there, optional here) with their reasons.
- [ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) — what made the
  type a reliably reported attribute at all.

## Consequences

- Positive: goals 5 and 9 are met — an external portal can generate a client from
  `/api/v1/openapi.json` and roll a configuration out to a chosen subset; the hash gate (goal 3)
  works per Agent; package rollouts (goal 10) inherit Selectors instead of inventing targeting of
  their own.
- Positive: the mechanism is almost entirely server-side. The Client's protocol behaviour is
  untouched (multi-entry config maps flow end to end, ADR-0011); its only addition is the optional
  `attributes` table.
- Positive: a fleet can be given the files its configuration *refers to*, not just the
  configuration itself — the case that motivated the role field upstream, and one a heterogeneous
  fleet (goal 7) hits as soon as an agent reads anything by path. The field is expressed end to
  end, so `CONFORMANCE.md` can record it as implemented rather than as a gap.
- Positive: limiting a Configuration to an Agent type is a stated property with its own input, not
  a remembered Selector convention — and the Selector goes back to being purely about aim (rings,
  environments, single Agents), the same clean split ADR-0016 bought for packages.
- Positive: the UI can warn meaningfully — a typed Configuration whose type no Agent in the fleet
  reports is visibly aimed at nobody, which the same value hidden in a Selector pair never was.
- Positive: stores, hashes, and fleets without the type field are untouched on upgrade.
- Negative / trade-offs: two server-side dependencies (`utoipa`, `utoipa-axum`); macro-derived
  documentation puts OpenAPI annotations next to handlers and schemas.
- Negative / trade-offs: `supplementary` is this project's word, chosen before upstream has one. If
  the Collector's supervisor adopts different values, operators will have configured the wrong
  vocabulary and a superseding ADR has to carry a migration.
- Negative / trade-offs: a new field on a public API resource is permanent — it can be deprecated
  but not withdrawn. `role` adds a second kind of entry the Supervisor must reason about when
  composing what the Managed Process is started with, and `service_name` means `ConfigurationSpec`
  grows a field and generated API clients regenerate.
- Negative / trade-offs: an optional type is forgettable — the fleet-wide churn of an untyped
  Collector body remains reachable, merely harder to reach by accident once the UI asks the
  question. This is the deliberate price of keeping the degenerate case.
- Negative / trade-offs: a mistyped type is a silent no-op (`otelcol-contib` reaches nobody),
  exactly ADR-0016's trade-off, with the same mitigation: show the reach.
- Negative / trade-offs: two ways to say "only Collectors" exist (the field and the pair) — the
  documentation names the field as the one to use, and the pair keeps working rather than being
  rejected, because refusing `service.name` in a Selector would break stores that are correct.
- Follow-ups (by topic): set-based Selector operators; configuration history and audit (possibly
  SQLite); staged rollouts (canary counts/percentages) on top of Selectors; whether a Supervisor
  should *restart* its Managed Process when only supplementary content changed — the safe answer
  (restart, as with any other entry) is assumed and deserves revisiting once an agent that reloads
  such files without restarting is managed; a warning in the fleet/configuration view when a typed
  Configuration matches no Agent; whether the type should one day become mandatory for top-level
  Configurations after a deprecation window.
