# SRV-configurations-compose-one-map: Configurations compose into one config map, and an OpenAPI document describes the REST API

- **Status:** 🟢 accepted
- **Date:** 2026-08-12
- **Deciders:** Markus Brigl
- **Applies to:** crates/fleet-server/src/configs.rs, crates/fleet-server/src/api.rs, every route and the OpenAPI document under /api/v1, role handling in crates/fleet-agent/src/storage.rs and the Supervisor plugins

## Context

The specification asks for three things this ADR answers:

- The Server is driven through an OpenAPI-described REST API (G-5).
- The Server sends an Agent a configuration a rollout released to it without being asked, and never
  again once the Agent reports its hash (G-1, G-3).
- The Server distributes each Managed Process's own configuration without an abstraction over its
  language (Non-Goal *Replacing an agent's configuration language*).

A Configuration is a named piece of an agent's own configuration language, written for one Agent
type. It reaches Agents through the Deployments that hold it, which aim it and release it
([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md)). This ADR fixes how
the Configurations released to an Agent travel to it and what the Client does with them.

OpAMP's `AgentConfigMap` is a map of named entries whose `config_hash` covers the whole map.
Composing an Agent's configuration out of several named parts is therefore the protocol's own
mechanism. A Collector takes several `--config` files and merges them natively.

The Baseline's `AgentConfigObject.role` describes *"the role of the content in the body field.
The values and their semantics are Agent type-specific."* Upstream added it for a case this project
has: a Collector configuration can reference files that are not configuration at all, such as a
fragment pulled in with `${file:ruleset.yaml}` or a certificate. Those files must be on disk beside
the configuration without being handed to the Collector as `--config`.

A Configuration's name becomes a file name on the Server, a config-map key on the wire, and an
entry file on every Client.

## Decision

We will compose every Configuration released to an Agent into one `AgentConfigMap` of named
entries that the Managed Process merges itself, carry each Configuration's role verbatim, and
make a REST API under `/api/v1`, described by an OpenAPI document generated code-first with
`utoipa`, the Server's contract.

1. **The name follows the instance-name grammar** of
   [CLT-installed-service-and-state](CLT-installed-service-and-state.md):
   - 1 to 32 characters of lowercase letters, digits, and `-`;
   - not starting or ending with `-`;
   - not a Windows device name.

   The name becomes a file name on the Server, a config-map key on the wire, and an entry file on
   every Client.

2. **An Agent's Remote configuration is one `AgentConfigMap` whose entries are Configurations.**
   - Each entry is keyed by the Configuration's name. The entries are in name order. Each carries
     the body and the role.
   - The Server never merges bodies. The Collector plugin passes each configuring entry as its own
     `--config`, and the Collector merges. Every other kind decides for itself what the named
     files mean.

3. **The hash covers what the Agent must do, never whom it reaches.**
   - `config_hash` is SHA-256 over the length-prefixed `(name, body, role)` of every entry in name
     order.
   - An empty role contributes nothing, so a Configuration without one hashes as if the field did
     not exist.
   - The Selector and the type are never hashed.
   - The Server makes an offer only to an Agent declaring `AcceptsRemoteConfig`, and only while the
     hash it reports as last received differs.

4. **Nothing released, nothing offered.**
   - The Server composes an Agent's map from the Configurations an operator's rollout released to
     it, never from matching alone
     ([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md)). Matching
     computes the candidates.
   - An Agent with nothing released to it is offered nothing and keeps running what it runs.

5. **The role travels verbatim; the Server owns no vocabulary.** The `role` of a Configuration goes
   unchanged into `AgentConfigObject.role` of its entry. An Agent that interprets roles itself
   therefore sees exactly what the operator set. Two readings are this project's:
   - **empty (the default)**: top-level configuration, which the Managed Process is configured
     with;
   - **`supplementary`**: content the Managed Process reads by path, such as fragments,
     certificates, rule files.

   A kind that defines nothing further treats any other value like `supplementary` and never
   guesses at it. A kind may give a value a meaning of its own, which that kind's ADR decides.

6. **On the Client every entry is written, and only unroled entries configure.**
   - Each entry lands in the Supervisor's configuration directory under its name, readable and
     writable by the Client's account alone, so a `${file:...}` reference resolves.
   - The Client records the role of every roled entry beside them in `.supplementary`, one
     `<name> <role>` per line. It writes that file only while some entry has a role.
   - A plugin that configures its process from the entries must leave every roled one out. The
     Collector gets one `--config` per unroled entry, in sorted order.

7. **The REST API is versioned by path and described by a generated document.**
   - Every route lives under `/api/v1`.
   - The OpenAPI document is generated code-first with `utoipa` and `utoipa-axum`. Handlers and
     schemas are annotated where they live, and routes are registered once, so document and
     behaviour cannot drift.
   - The Server serves the document at `/api/v1/openapi.json`.
   - The API is served on the Operator plane
     ([SEC-tls13-two-planes-bounded-input](SEC-tls13-two-planes-bounded-input.md)), behind its
     authentication ([SEC-admission-by-client-certificate](SEC-admission-by-client-certificate.md)),
     beside the rendered docs page ([DEV-six-crates-tokio-axum](DEV-six-crates-tokio-axum.md)).

**Out of scope:**

- what a Configuration holds, how it is aimed and released, its routes, and what deleting one does
  ([SRV-deployments-and-pinned-rollouts](SRV-deployments-and-pinned-rollouts.md));
- where Configurations are stored
  ([SRV-state-stores-contract-pair](SRV-state-stores-contract-pair.md));
- the meaning of a kind's own role values (each kind's own ADR);
- configuration history and audit.

## Alternatives considered

- **Server-side body merging.** Precisely the abstraction over an agent's configuration language
  the specification rules out, and it would drag a YAML stack into the Server.
- **Priority ordering between Configurations, first match wins.** A total order between unrelated
  Configurations is hidden coupling. It forecloses the composition the protocol's config map
  provides for free.
- **A hand-written OpenAPI document**, or **`aide`** instead of `utoipa`. A document maintained
  beside the code drifts. `utoipa` is the most widely adopted code-first generator with dedicated
  axum bindings. The document, not the generator, is the contract.
- **Unversioned routes, or versioning by header.** Generated clients and reverse proxies handle
  path versioning most simply. Renaming routes after portals have generated clients is the
  expensive variant.
- **A `supplementary` boolean instead of a role string.** Clearer to read, but it cannot carry any
  other Agent type's vocabulary and would have to be mapped onto `role` on the wire anyway.
- **A separate resource for supplementary files.** It duplicates targeting, hashing, persistence,
  and the REST surface for content that differs in exactly one respect.
- **Carry `role` but let the plugins ignore it.** The operator sets a role, the Server ships it,
  and the Supervisor still hands a certificate to the Collector as `--config`.

## Sources / Prior art

- [Grafana Fleet Management
  architecture](https://grafana.com/docs/grafana-cloud/send-data/fleet-management/introduction/architecture/):
  several matching pipelines sorted by name and merged.
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  and [`opamp.proto`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto) at the Baseline
  ([`CONFORMANCE.md`](../CONFORMANCE.md)): `AgentConfigMap` of named entries, `config_hash` over
  the offer, `AgentConfigObject.role`, `service.name` as the Agent type.
- [opamp-spec#184](https://github.com/open-telemetry/opamp-spec/issues/184) and
  [opamp-spec#350](https://github.com/open-telemetry/opamp-spec/pull/350): the case for `role`,
  top-level configuration versus supplementary content.
- [OpenTelemetry Collector configuration](https://opentelemetry.io/docs/collector/configuration/):
  several `--config` files merged, `${file:...}` reading content that is not configuration.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) and the Collector's `opampsupervisor`:
  neither defines role values, which is why this project's vocabulary is one value.
- [utoipa](https://github.com/juhaku/utoipa) and
  [`utoipa-axum`](https://docs.rs/utoipa-axum/latest/utoipa_axum/): code-first OpenAPI generation
  with axum bindings.

## Consequences

- Positive: a portal can generate a client from `/api/v1/openapi.json`, and the hash gate works per
  Agent.
- Positive: a fleet can be given the files its configuration refers to, not only the configuration.
- Negative / trade-offs: `supplementary` is this project's word, chosen before upstream has one. If
  the Collector's supervisor settles on different values, operators will have configured the wrong
  vocabulary. A field on a public resource can be deprecated but never withdrawn.
- Negative / trade-offs: two Server dependencies (`utoipa`, `utoipa-axum`), and OpenAPI annotations
  next to every handler and schema.
- Follow-ups: configuration history and audit; whether a change to supplementary content alone
  ought to restart the Managed Process (it does today, like any entry).

## Enforcement

- [`configs.rs`](../../crates/fleet-server/src/configs.rs) unit tests:
  `composition_is_name_sorted_and_hash_stable`,
  `a_role_travels_into_the_composed_entry_and_into_the_hash`,
  `an_empty_role_leaves_the_hash_where_it_was`, `the_store_round_trips_and_survives_a_reopen`,
  `the_store_rejects_bad_names_and_empty_bodies`;
  [`fs/configs.rs`](../../crates/fleet-server/src/fs/configs.rs): `a_role_survives_a_reopen`.
- [`rest_api.rs`](../../crates/fleet-server/tests/rest_api.rs): `configurations_crud_round_trips`,
  `a_configuration_carries_an_optional_role`, `invalid_configurations_are_rejected_loudly`,
  `the_openapi_document_describes_the_contract`, `configurations_survive_a_server_restart`.
- [`ws_transport.rs`](../../crates/fleet-server/tests/ws_transport.rs):
  `a_configuration_role_reaches_the_agent_verbatim`.
- Client: [`storage.rs`](../../crates/fleet-agent/src/storage.rs) unit tests
  `a_roled_entry_is_written_but_not_offered_as_configuration`,
  `an_unknown_role_is_treated_like_supplementary`, `a_roles_value_is_readable_per_entry`,
  `the_state_and_configuration_are_kept_owner_only`; and
  [`supervisor.rs`](../../crates/fleet-agent/tests/supervisor.rs)
  `a_collector_supervisor_leaves_supplementary_entries_out_of_its_config_flags`.
