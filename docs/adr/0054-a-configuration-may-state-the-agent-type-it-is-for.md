# ADR-0054: A Configuration may state the Agent type it is for — and then reaches no Agent of another

- **Status:** 🟢 accepted
- **Date:** 2026-08-12
- **Deciders:** Markus Brigl

Extends [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md): Selector
semantics survive unchanged, and this adds a type fit in front of them — the shape
[ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md) gave packages, applied to
Configurations. It builds on
[ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md), which made the Agent
type a fact every Agent reports.

## Context

A Configuration is `{name, selector, body, role}` (ADR-0012, ADR-0016), and its Selector is the
only targeting it has. The type *is* expressible today — `service.name` is a reported attribute
like any other, `matches()` compares it
([`configs.rs`](../../crates/server/src/configs.rs#L110-L125), tested at
[`configs.rs:311`](../../crates/server/src/configs.rs#L311-L334)), and the bundled UI offers
`service.name=…` as a clickable Selector chip. So "limit a Configuration to an Agent type" is
possible — but only as an opt-in pair buried among the aim, and ADR-0034 has already judged that
shape for packages: "a rule that is only ever right when remembered is not the rule this needs."

What forgetting it does here: an empty Selector matches every Agent (ADR-0012 binds this — the
fleet-wide Configuration is the degenerate case), and every Agent this Client presents declares
`AcceptsRemoteConfig` unconditionally
([`agent.rs:30`](../../crates/client/src/supervisor/agent.rs#L30)) — the supervised Collectors,
every Foreign Agent, and the Client's own self-Agent alike. A Collector YAML saved without a
`service.name` pair is therefore composed into the config map of every one of them: each Foreign
Agent is restarted into a configuration entry its program cannot read (the apply grace catches it,
at the price of a restart-and-rollback cycle per Agent), and the self-Agent stores it as applied
([`agent.rs:852`](../../crates/client/src/supervisor/agent.rs#L852)). The blast radius is smaller
than a wrong-type *binary* — ADR-0034's case — because a configuration is health-gated per entry
and rolled back, not installed over a program. It is still a fleet-wide churn reachable by
omitting one pair.

There is also a purity cost ADR-0034 already named for packages: with the type expressed as a
Selector pair, the Selector carries *what kind of Agent* and *which of them* in one field, and the
two cannot be read apart in the UI or the API.

## Decision

We will make the **Agent type** an optional, first-class property of a Configuration, matched for
equality against the `service.name` an Agent reports — before the Selector, and independent of it.

1. **`service_name` joins the Configuration**, beside `selector`, `body`, and `role` — in the
   `PUT /api/v1/configurations/{name}` body and the persisted JSON, absent when unset (like
   `role`, ADR-0016). No sub-resource: unlike a package's type it is not identity (ADR-0052) and
   not immutable, it is one more field of the one writable resource.

2. **Fit before aim.** Composition drops every Configuration whose `service_name` is set and is
   not the Agent's reported `service.name`, then runs the Selector over what is left. The type is
   compared **raw**, no canonicalisation — ADR-0034 point 2's rule, for its reason: there is no
   canonical set of Agent types to normalise against.

3. **Unset means every type.** The fleet-wide Configuration — ADR-0012's degenerate case — and
   cross-type `supplementary` content (a certificate bundle, ADR-0016) stay expressible. This is
   deliberately *not* ADR-0034's "no type, offered to nobody": there the unset state hid a
   mismatched-binary outage, here it is the documented base case of the resource.

4. **An Agent that reports no `service.name` matches only untyped Configurations.** Equality
   against a missing attribute fails, exactly as a Selector pair against a missing attribute
   fails today — no new rule, stated for the record.

5. **A store written before this ADR loads unchanged.** Absent field, untyped, same matching as
   today; no hash moves, no Managed Process restarts on upgrade.

6. **The bundled UI shows the type as its own input**, beside the Selector, suggesting the types
   the fleet currently reports — so choosing one is a click, not a remembered convention — and
   shows it in each Configuration's chip.

## Alternatives considered

- **Leave it to the Selector and document it.** Works today at zero cost, and unlike ADR-0034's
  packages the failure is health-gated, not a bricked binary. Rejected: the failure is still
  fleet-wide churn behind one forgotten pair, the type stays unreadable apart from the aim, and
  the UI cannot guide what it cannot distinguish. ADR-0034 rejected this shape with the blast
  radius as the *tiebreaker*, not the argument.
- **A mandatory type — untyped Configurations reach nobody** (ADR-0034 point 3 verbatim).
  The consistent mirror, and the starting assumption. Rejected: it abolishes ADR-0012's
  fleet-wide degenerate case and cross-type supplementary content, both legitimate and in use;
  and it needs a migration in which every existing Configuration goes silently inert on upgrade —
  the shape ADR-0043 point 4 refused for a state that is not unsafe. For packages the unset state
  hid an outage; here it is a feature with a name.
- **Require the type only for top-level Configurations and not for `supplementary` ones.**
  Splits the safety rule along ADR-0016's role, which the Server treats as opaque beyond one
  known value — a conditional mandate hanging off a field whose vocabulary this project
  deliberately does not own. Rejected for rule complexity that buys the mandate only partially.
- **Pattern or prefix matching** (`otelcol*`). Rejected in ADR-0034 for types; nothing about
  Configurations weakens that reasoning — two types are two Configurations.

## Sources / Prior art

- [ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md) — the model: type as a
  first-class fit step in front of the Selector, compared raw; and the argued divergence points
  (mandatory there, optional here) with their reasons.
- [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md) — Selector semantics
  and the empty-Selector degenerate case this decision preserves.
- [ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md) — what made the
  type a reliably reported attribute at all.
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the comparable product models the Agent Type as a first-class object and hangs what an agent
  may receive off it; already cited by ADR-0034 as evidence that type-as-a-property is the shape
  that holds up in a shipping fleet manager.
- [OpAMP specification `v0.19.0`, `AgentDescription`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — `service.name` as the attribute that "uniquely identifies the Agent type", the value this fit
  reads.

## Consequences

- Positive: limiting a Configuration to an Agent type becomes a stated property with its own
  input, not a remembered Selector convention — and the Selector goes back to being purely about
  aim (rings, environments, single Agents), the same clean split ADR-0034 bought for packages.
- Positive: the UI can warn meaningfully — a typed Configuration whose type no Agent in the fleet
  reports is visibly aimed at nobody, which the same value hidden in a Selector pair never was.
- Positive: existing stores, hashes, and fleets are untouched on upgrade.
- Negative / trade-offs: optional means forgettable — the fleet-wide churn of an untyped Collector
  body remains reachable, merely harder to reach by accident once the UI asks the question. This
  is the deliberate price of keeping the degenerate case; revisiting it (a mandatory type after a
  deprecation window) stays open as a follow-up.
- Negative / trade-offs: a mistyped type is a silent no-op (`otelcol-contib` reaches nobody),
  exactly ADR-0034's trade-off, with the same mitigation: show the reach.
- Negative / trade-offs: `ConfigurationSpec` grows a field; generated API clients regenerate.
  Two ways to say "only Collectors" now exist (the field and the pair) — the documentation names
  the field as the one to use, and the pair keeps working rather than being rejected, because
  refusing `service.name` in a Selector would break stores that are correct today.
- Follow-ups: warn in the fleet/configuration view when a typed Configuration matches no Agent;
  whether the type should one day become mandatory for top-level Configurations after a
  deprecation window.
