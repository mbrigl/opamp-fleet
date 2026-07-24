# ADR-0012: Selector-targeted Configurations and the OpenAPI-described REST API

- **Status:** 🟡 proposed
- **Date:** 2026-07-23
- **Deciders:** Markus Brigl

## Context

The control loop holds end to end: real processes behind Supervisors, honest apply status, n Agents
over one connection (ADR-0011). The specification's strategy names what comes next — *"Targeting a
subset of the fleet and updating an agent's software are core goals, built on top of that loop once
it holds"* — and two goals sit exactly there. Goal 9 (a configuration can target a subset via a
**Selector**) is unimplemented: the Server holds exactly **one** fleet-wide configuration
(`DesiredConfig` in `crates/server/src/fleet.rs`), offered identically to every Agent. Goal 5 (any
UI can drive the fleet through an **OpenAPI-described** REST API) is only rudimentarily met: three
hand-rolled routes (`crates/server/src/api.rs`) and **no OpenAPI document at all** — the promised
integration contract does not exist yet. The two belong to one decision, because Selectors force
the API to grow a real resource model, and that model is what the OpenAPI document must describe.

The forces are largely fixed. The specification demands hash-gated distribution (goal 3), forbids
inventing an abstraction over an agent's configuration language (non-goal), and defines the
vocabulary: **Selector**, **Remote configuration**. ADR-0005 binds axum and one listener for OpAMP,
REST API, and UI. ADR-0008 binds TOML with loud typo rejection for the Server's and Client's own
configuration files. ADR-0010 fixes a name grammar for things that become file names. ADR-0011 has
the Collector Supervisor pass **every config-map entry as its own `--config` argument** and lets
the Collector do its own merging — no YAML manipulation in Rust.

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

## Decision

We will replace the single fleet-wide configuration with named **Configurations**, each carrying a
**Selector** matched against the attributes an Agent reports; every Agent receives **all matching
Configurations as named entries of one `AgentConfigMap`**; and we will make the REST API the
project's contract: **versioned under `/api/v1`, described by an OpenAPI document generated
code-first with `utoipa`**, served from the same listener.

Concretely this binds:

- **The Configuration resource.** A Configuration is `{name, selector, body}`: a name following
  the ADR-0010 grammar (it becomes a file name and a config-map key), a Selector, and an opaque
  text body (the Managed Process's own format — never interpreted by the Server, per the
  specification's non-goal). Today's fleet-wide configuration is the degenerate case: a
  Configuration whose Selector is empty.
- **Selector semantics: equality, AND, over reported attributes.** A Selector is a string-to-string
  map; an Agent matches when **every** pair equals an attribute the Agent reported in its
  `AgentDescription` (identifying and non-identifying alike, e.g. `service.name`, `os.type`,
  `host.arch`, `service.instance.id` for pinning a single Agent). The **empty Selector matches
  every Agent**. An Agent that has not reported a description yet matches only empty Selectors.
  Set-based operators (`in`, `notin`, `exists` — the BindPlane/Kubernetes grammar) are deferred:
  they are additive and nothing needs them yet.
- **Operator-defined attributes on the Client.** `client.toml` gains an optional `attributes`
  table (string → string) — top-level for the Client's self-Agent and per `[[supervisor]]` block —
  folded into the non-identifying attributes of the respective Agent's description. Without a way
  to tag an Agent (`env = "prod"`), Selectors could only target what the code happens to report.
  Reported attributes win over configured ones on key collision.
- **Composition by the protocol, merging by the process.** All Configurations matching an Agent
  form its Remote configuration: one `AgentConfigMap` whose entry keys are the Configuration
  names, in name order (deterministic like Grafana FM's alphabetical rule); `config_hash` is the
  SHA-256 over the sorted `(name, body)` pairs, so the existing hash gate (goal 3) works
  unchanged, per Agent. The Server never merges bodies: the Collector Supervisor already passes
  each entry as its own `--config` and the Collector merges natively (ADR-0011); a Custom
  Supervisor receives the named files and decides plugin-specifically. No client change is needed —
  multi-entry config maps are already handled end to end.
- **No match, no offer.** An Agent matching no Configuration is sent nothing and keeps running
  what it already runs — exactly goal 9's wording. Consequently there is **no revocation**:
  narrowing a Selector stops future offers but does not reset an Agent that already applied one
  (recorded as a follow-up, not smuggled in).
- **Persistence: one JSON file per Configuration.** The Server persists each Configuration as
  `<config_dir>/<name>.json` (serde serialization of the API resource), written atomically
  (temp file + rename), restored at startup; `DELETE` removes the file. The `config_dir` setting
  (default `fleet-configs/`) **replaces** `fleet_config_file` in `server.toml` — an old file
  naming the removed key fails loudly at startup (ADR-0008), and the operator migrates by `PUT`ting
  the old body as a Configuration with an empty Selector. No database: a handful of small files
  needs none.
- **The REST API v1.** Routes move under `/api/v1`: `GET /api/v1/agents` (the fleet, now including
  every reported attribute and the names plus hash of the Configurations currently matching each
  Agent — the operator must see what a Selector would select), `GET /api/v1/configurations`,
  and `GET`/`PUT`/`DELETE /api/v1/configurations/{name}`. The OpenAPI document is generated
  code-first with `utoipa`/`utoipa-axum` — handlers and schemas annotated where they live, routes
  registered once, so document and behaviour cannot drift — and served at `/api/v1/openapi.json`.
  The unversioned routes are removed and the bundled UI moves onto v1; versioning starts **now**
  because this is the last cheap moment — before any external portal generates a client. No
  Swagger UI is bundled: the document is the contract, the rudimentary UI stays the one page
  (ADR-0005).

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
- **Keeping the unversioned `/api/*` routes (or versioning by header).** Rejected. Three routes
  and one bundled UI page are the entire installed base today; renaming later, after portals have
  generated clients, is the expensive variant. Path versioning is the form generated clients and
  reverse proxies handle most simply.

## Sources / Prior art

- [Grafana Fleet Management architecture](https://grafana.com/docs/grafana-cloud/send-data/fleet-management/introduction/architecture/)
  — attribute matching between collectors and configuration pipelines; multiple matches sorted
  alphabetically by name and merged into one remote configuration: deterministic ordering plus
  process-native merge, the model this decision adopts.
- [Bindplane: Fleets](https://docs.bindplane.com/feature-guides/deployment-and-management/fleets)
  — Kubernetes-style label selectors (`=`, `!=`, `in`, `notin`, `exists`) matching agent labels to
  a fleet's configuration; agents carry operator-set labels for exactly this purpose.
- [Elastic Agent policies](https://www.elastic.co/docs/reference/fleet/agent-policy) — the
  exclusive-assignment alternative: each agent is enrolled in exactly one policy.
- [Kubernetes labels and selectors](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/)
  — the equality-based/set-based selector grammar the ecosystem converged on; this decision takes
  the equality subset first.
- [OpAMP specification: Configuration](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — `AgentConfigMap` as a map of named entries and `config_hash` over the whole offer: the
  protocol's own composition mechanism (Baseline `v0.18.0`, see [`CONFORMANCE.md`](../CONFORMANCE.md)).
- [utoipa](https://github.com/juhaku/utoipa) and
  [`utoipa-axum`](https://docs.rs/utoipa-axum/latest/utoipa_axum/) — code-first, compile-time
  OpenAPI generation with axum bindings; the most widely adopted Rust option.

## Consequences

- Positive: goals 5 and 9 become real — an external portal can generate a client from
  `/api/v1/openapi.json` and roll a configuration out to a chosen subset; the hash gate (goal 3)
  keeps working per Agent; later package rollouts (goal 10) inherit Selectors instead of inventing
  targeting of their own.
- Positive: the change is almost entirely server-side. The Client's protocol behaviour is
  untouched (multi-entry config maps already flow end to end, ADR-0011); its only addition is the
  optional `attributes` table.
- Negative / trade-offs: no revocation — an Agent that leaves every Selector keeps its last
  applied configuration silently. Breaking changes to `server.toml` (`fleet_config_file` →
  `config_dir`) and to the API paths — accepted deliberately now, pre-contract, rather than ever
  after. Upgrading a running fleet re-hashes every offer (named entries replace today's single
  unnamed entry), so each Managed Process restarts once on the new Server's first offer.
- Negative / trade-offs: two new server-side dependencies (`utoipa`, `utoipa-axum`); macro-derived
  documentation puts OpenAPI annotations next to handlers and schemas.
- Follow-ups (by topic): set-based Selector operators; configuration revocation/reset semantics;
  configuration history and audit (possibly SQLite); staged rollouts (canary counts/percentages)
  on top of Selectors; REST-API authentication — the OpAMP-side authentication row in
  [`CONFORMANCE.md`](../CONFORMANCE.md) and the API contract both need it before the listener
  faces anything but an operator's loopback.
