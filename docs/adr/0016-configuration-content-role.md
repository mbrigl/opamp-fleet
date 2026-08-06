# ADR-0016: Carry the Baseline's `AgentConfigFile.role` through the Configuration model

- **Status:** 🟢 accepted
- **Date:** 2026-08-04
- **Deciders:** Markus Brigl

## Context

Baseline `v0.19.0` added one field to the wire that this project cannot express:
`AgentConfigFile.role`, *"the role of the content in the body field. The values and their semantics
are Agent type-specific."*

The motivation upstream ([opamp-spec#184](https://github.com/open-telemetry/opamp-spec/issues/184),
implemented by [#350](https://github.com/open-telemetry/opamp-spec/pull/350)) is a case this project
already has. The Collector takes several configuration files on its command line and merges them —
which is exactly how [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md)
delivers a composed configuration: every matching Configuration becomes one named entry of the
`AgentConfigMap`, and the Collector plugin writes each entry to the Supervisor's config directory
and passes it as its own `--config`. But a Collector configuration can also *reference* files that
are not configuration at all — a fragment pulled in with `${file:ruleset.yaml}`, a certificate, any
artifact read at startup. Those files must be on disk next to the configuration and must **not** be
handed to the Collector as `--config`. Today this project cannot distribute one: every entry it
receives is treated as top-level configuration, so a certificate distributed as a Configuration
would be passed to the Collector as configuration and break the process it was meant to configure.

The field is optional and unset costs nothing, which is why the Baseline bump left it unset. Making
it usable is a different matter: a Configuration is a REST API resource
(`PUT /api/v1/configurations/{name}` with `selector` and `body`), and the REST API is this project's
**public contract** — the thing goal 5 promises portals can generate clients from. Adding a field to
it is not an implementation detail, and neither is deciding what a role *means* to a Supervisor
plugin: the specification's non-goal *"Forking or extending the protocol"* forbids inventing
protocol semantics, so whatever this project understands by a role value has to be plugin-level
convention, honestly labelled as such.

## Decision

We will carry an **optional `role` string on the Configuration resource** through the REST API into
the `AgentConfigFile.role` of every entry composed from it, and have Supervisor plugins honour two
values while passing the field on verbatim:

- **empty (the default)** — top-level configuration, handled exactly as today: written to the
  Supervisor's config directory and passed to the Managed Process as configuration.
- **`supplementary`** — written to the same directory under the Configuration's name, but **not**
  passed as configuration. It is content the Managed Process reads by path, not by being told about
  it: fragments, certificates, rule files.

Any other value is written like `supplementary` and reported as received; it is never guessed at.
The value travels unchanged in `AgentConfigFile.role`, so an Agent that interprets roles itself —
a Collector with its own `opampextension`, reached through the Supervisor Endpoint — sees exactly
what the operator set, not this project's reading of it.

`role` is absent from an existing Configuration's JSON and stays absent in responses when unset, so
every stored Configuration and every generated client keeps working unchanged.

## Alternatives considered

- **Leave `role` unset, as the Baseline bump did.** Rejected as an end state, though it is where the
  code stands until this ADR is decided. It leaves goal 7's heterogeneous fleet unable to receive a
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
  values yet, and the case is already reachable today. Divergence is handled the way this project
  handles it elsewhere — a superseding ADR when upstream settles.

## Sources / Prior art

- [`AgentConfigFile` in the Baseline](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/proto/opamp/v1/opamp.proto)
  (`v0.19.0`) — the field, and its "values and semantics are Agent type-specific" wording.
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
  values yet, which is why this ADR keeps its vocabulary to a single value and expects to be
  superseded rather than extended if upstream settles on different words.

## Consequences

- Positive: a fleet can be given the files its configuration *refers to*, not just the configuration
  itself — the case that motivated the field upstream, and one a heterogeneous fleet (goal 7) hits
  as soon as an agent reads anything by path.
- Positive: the field is expressed end to end, so `CONFORMANCE.md` can record it as implemented
  rather than as a gap.
- Negative / trade-offs: `supplementary` is this project's word, chosen before upstream has one. If
  the Collector's supervisor adopts different values, operators will have configured the wrong
  vocabulary and a superseding ADR has to carry a migration.
- Negative / trade-offs: a new field on a public API resource is permanent — it can be deprecated
  but not withdrawn — and it adds a second kind of entry the Supervisor must reason about when
  composing what the Managed Process is started with.
- Follow-ups: whether a Supervisor should *restart* its Managed Process when only supplementary
  content changed is a real question this decision opens; the safe answer (restart, as with any
  other entry) is assumed here and deserves revisiting once an agent that reloads such files
  without restarting is actually managed.
