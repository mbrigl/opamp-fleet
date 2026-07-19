# ADR-0017: `AcceptsOtherConnectionSettings` — deferred until a concrete consumer exists

- **Status:** 🟢 accepted
- **Date:** 2026-07-19
- **Deciders:** Maintainer

## Context

OpAMP's `ServerToAgent.connection_settings.other_connections` is a **map of named connection settings**
for connections the Agent makes *for its own purposes* — not the OpAMP control connection, and not the
own-telemetry destinations. An Agent that declares **`AcceptsOtherConnectionSettings`** receives these
named offers and is expected to apply them to whatever those named connections drive.
[ADR-0010](0010-collector-supervisor-own-telemetry.md) deferred this capability to its own ADR; this is
that ADR.

The problem: for the agents this project supervises, **there is no consumer for these offers**.

- The **Collector Supervisor** manages an OpenTelemetry Collector whose outbound connections (receivers,
  exporters to backends) are defined by the Collector *configuration* — which already arrives as remote
  config ([ADR-0008](0008-collector-supervisor-go-reference-compat.md)) and, for its own telemetry, as the
  own-telemetry offer ([ADR-0010](0010-collector-supervisor-own-telemetry.md)). There is no *additional*
  named connection a Collector makes that `other_connections` would configure.
- The **Custom Supervisor** ([ADR-0009](0009-plugin-hexagonal-supervisor-host.md)) writes a foreign
  agent's config file; any connection that agent makes is likewise defined by *its* config, not by an
  out-of-band OpAMP map with no defined mapping into that file.

A firm principle from [ADR-0008](0008-collector-supervisor-go-reference-compat.md) and
[ADR-0010](0010-collector-supervisor-own-telemetry.md) governs this: **declare a capability only when we
act on it — never claim a capability we do not implement.** Declaring `AcceptsOtherConnectionSettings`
with nowhere to route the offers would break exactly that rule.

## Decision

We will **not implement `AcceptsOtherConnectionSettings` now.** We will not declare the capability, and we
will continue to ignore `other_connections` offers, because there is no consumer to route them to and
declaring it would violate our "declare only what we act on" rule.

We record the **condition that would reverse this**: a concrete use case where a named, Server-offered
connection must reach a managed agent — for example, a Custom Supervisor whose foreign agent has a
templated config slot for a named backend endpoint/credentials that the fleet should set centrally. When
such a consumer is specified, a superseding ADR will scope `other_connections` to *that* mapping (which
named connection maps to which config slot), not to a generic, sink-less pass-through.

## Alternatives considered

- **Implement a minimal pass-through now** (accept and persist `other_connections`, expose them somewhere).
  Rejected: it declares a capability with no sink, contradicting the project's own principle, and invents a
  mapping (offers → collector/foreign config) that no requirement defines — speculative generality (YAGNI).
- **Inject `other_connections` into the Collector config as extra exporters/receivers.** Rejected: the
  Collector's connections are the *remote config's* job; a second, parallel channel for them would create
  two sources of truth for the same thing and an ambiguous merge.
- **Declare the capability but no-op the offers.** Rejected outright: that is precisely the false-promise
  the capability-declaration rule forbids — the Server would believe the agent honours offers it drops.

## Sources / Prior art

- OpAMP specification — `OtherConnectionSettings` / `other_connections` and
  `AcceptsOtherConnectionSettings`:
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>.
- The capability-declaration principle: [ADR-0008](0008-collector-supervisor-go-reference-compat.md),
  [ADR-0010](0010-collector-supervisor-own-telemetry.md) (which deferred this capability here). The agents
  whose connections are config-driven: [ADR-0009](0009-plugin-hexagonal-supervisor-host.md).

## Consequences

- Positive: no false capability is advertised; no sink-less plumbing or invented mapping is added; the
  config model stays single-source (remote config owns the managed agent's connections).
- Negative / trade-offs: a named Go/OpAMP capability stays unimplemented, so a Server that *depends* on
  `other_connections` would find this agent does not accept it — acceptable, because acting on it without a
  consumer would be worse (a false promise).
- Follow-ups: a superseding ADR when a concrete consumer is specified (most plausibly a templated
  Custom-Supervisor config slot), scoping `other_connections` to that mapping only.
