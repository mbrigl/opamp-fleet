# ADR-0017: Selector-targeted packages, chosen by the Server rather than named on each host

- **Status:** 🟡 proposed
- **Date:** 2026-08-05
- **Deciders:** Markus Brigl

## Context

Package delivery works end to end (ADR-0015): an operator uploads an artifact through the REST API
and every capable Agent downloads, verifies, applies, and health-gates it, reporting progress on the
way. What it cannot do is aim.

**A package is offered to the whole fleet.** Configurations are targeted by a Selector
([ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md)) — matched against the
attributes an Agent reports — but packages carry no Selector at all. Every Agent whose Supervisor
names the package is offered it, at once. Goal 9 states that a change may address *"the whole fleet
or a chosen subset of it, so a configuration can be rolled out to part of the fleet before all of
it"*, and the vision applies that same expectation to software: *"it can update an agent's binary in
place"* as part of the same control loop. For binaries the ability is missing, and a binary is
precisely the change an operator wants to try on five hosts before three hundred.

**Which package an Agent takes is decided on the host, not on the Server.** A `[[supervisor]]` block
opts in by naming one: `package = "otelcol"`. So the two things an operator most needs to steer —
*who* gets an update and *which* artifact they get — are settings in a file on every managed
machine. A fleet spanning `linux/amd64`, `linux/arm64`, and Windows needs one package name per
platform and a matching `client.toml` on each host, even though every Agent already reports
`host.arch` and `os.type` in its description. Nothing about that is reachable from the REST API or
the bundled UI, which is what goal 5 promises an operator.

The protocol is not what is in the way. The Baseline describes `PackagesAvailable` as *"the packages
that are available on the Server **for this Agent**"* — the offer is per-Agent by design, and its
`all_packages_hash` gates re-offering per Agent as well. The Server already composes a per-Agent
remote configuration this way; it composes one global package offer only because nothing made it do
otherwise.

## Decision

We will give a **Package a Selector**, exactly as a Configuration has one, and make the **Server**
decide which artifact an Agent is offered — so a rollout is aimed from the REST API and the UI, not
from a file on each host.

Four parts:

1. **A Package carries a Selector.** An empty Selector means the whole fleet, as with Configurations.
   The Server offers an Agent only the packages whose Selector matches its reported attributes, and
   computes `all_packages_hash` over *that* Agent's set, so the Baseline's re-offer gate keeps working
   per Agent.

2. **A Supervisor says only whether it accepts updates, never which one.** `accepts_packages = true`
   means "this Managed Process's binary is updated from the top-level package the Server selects for
   me"; absent, the Supervisor takes no package offers, as today. The current `package = "name"` key
   is **removed**: choosing the artifact is the Server's job, and leaving a second way to choose it
   on the host would keep the decision in the place this ADR is moving it out of. A `client.toml`
   still carrying `package = "…"` fails at startup with a message naming the replacement — loudly,
   as ADR-0008 requires, never silently ignored.

   Pinning one host to a specific artifact does not disappear; it moves to the Server, where a
   Selector on that host's `host.name` (or an operator attribute) expresses it — and, unlike a line
   in a file on that machine, is visible in the fleet view.

3. **At most one top-level package may match an Agent.** The Baseline states there is *"normally only
   one top-level package, which implements the primary functionality of the Agent"*. Two matching
   top-level packages are an operator error the Server refuses to resolve by guessing: it rejects the
   Selector that would create the overlap, naming the other package.

4. **The Selector is set through its own sub-resource**, `PUT /api/v1/packages/{name}/selector`, with
   the same JSON shape a Configuration uses. The artifact upload keeps its current contract
   (`PUT /api/v1/packages/{name}?version=…` with the artifact as the body); a package without a
   Selector behaves exactly as it does today, so no stored package and no generated client breaks.

## Alternatives considered

- **Leave targeting to `client.toml`, as today.** Rejected. It works — the platform case is solved by
  naming `otelcol-linux-amd64` per host — but it puts the rollout decision on three hundred machines
  and outside the API that goal 5 makes the integration contract. A staged rollout then means editing
  and redistributing host configuration, which is the problem this project exists to remove.
- **Reuse Configurations to carry the package name.** Rejected. It would let an operator aim a
  rollout with the mechanism that already aims, but it conflates two lifecycles: a configuration is
  applied by restarting on new files, a package is verified, swapped, health-gated, and rolled back.
  Tying them means a config change and a binary change cannot be reasoned about — or reverted —
  separately.
- **Keep `package = "name"` alongside the new opt-in, as an explicit pin.** Rejected, after weighing
  it as the compatible option. It would spare existing `client.toml` files a change, and a pin is
  occasionally what an operator wants — a test host that must not follow the fleet. But it leaves two
  ways to decide the same thing, one of them on the machine this ADR is trying to stop editing, and
  the pin is expressible on the Server anyway: a Selector matching that host's `host.name`. One
  mechanism, visible in the fleet view, beats two that can disagree.
- **One key with two meanings (`package = true` or `package = "name"`).** Rejected. It is the most
  compact spelling and breaks nothing, but the same key then means "whether" in one form and "which"
  in the other — a distinction a reader has to know rather than see. Config keys are read under
  pressure; this one would be read wrong.
- **Split the upload into metadata and artifact** (`PUT /api/v1/packages/{name}` for JSON including
  the Selector, `PUT /api/v1/packages/{name}/file` for the bytes). Rejected for now: it is the
  tidier REST shape and mirrors Configurations better, but it breaks the published v1 contract for a
  gain that a sub-resource delivers additively. Worth revisiting if the package model grows further.
- **Target by Instance UID instead of a Selector.** Rejected. Naming Agents individually pins a
  rollout to identities that are reassignable (`AgentIdentification`) and says nothing about *why*
  those hosts were chosen. A Selector over reported attributes survives re-identification and is
  self-documenting — and it is already the project's vocabulary.

## Sources / Prior art

- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"The PackagesAvailable message describes the packages that are available on the Server for this
  Agent"*, and *"There is normally only one top-level package"*: the per-Agent offer and the
  single-top-level-package expectation this decision builds on.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — checked as the behavioural oracle: its
  example Server only logs `PackagesAvailable`/`ServerProvidedAllPackagesHash` and never offers a
  package, so there is no upstream targeting behaviour to copy. The model is ours to choose, within
  what the protocol already provides.
- [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md) — the Selector semantics
  this reuses verbatim (every pair must equal a reported attribute; an empty Selector matches all),
  so an operator learns one targeting mechanism, not two.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) — the delivery, verification, and
  health-gating this leaves untouched; only *who is offered what* changes.

## Consequences

- Positive: a binary rollout can be staged — a Selector on `role`, `host.name`, or an operator
  attribute reaches five hosts first, and widening it is one API call. This is goal 9 applied to
  software rather than only to configuration.
- Positive: a heterogeneous fleet stops needing per-host wiring. `host.arch` and `os.type` are
  already reported, so one package per platform, each with a Selector, updates every machine from
  the Server — and the UI can show which Agents a package targets, as it already does for
  Configurations.
- Negative / trade-offs: `all_packages_hash` becomes per-Agent, so the Server can no longer keep one
  precomputed aggregate. The cost is small (a hash over the matching set per exchange) but it is a
  real change to the gate that stops re-offering, and getting it wrong means either a re-offer loop
  or a missed update — it needs its own test.
- Negative / trade-offs: **this breaks every `client.toml` that names a package.** `package = "…"`
  is refused at startup and must become `accepts_packages = true`, with the artifact choice moved
  into a Selector on the Server. A one-line edit per host, but a required one, and a Client that is
  updated before its Server has Selectors configured will accept an offer chosen by an empty
  Selector — i.e. the fleet-wide package — which is the old behaviour and therefore safe, but worth
  saying out loud in the release note.
- Negative / trade-offs: an operator can create an overlap — two top-level packages whose Selectors
  match the same Agent — which the Server has to refuse. A refusal at the API is the right place for
  it, but it is a new class of error message, and it can appear when *widening* a Selector that was
  fine before.
- Negative / trade-offs: a Selector that stops matching an Agent does **not** uninstall anything —
  the Agent keeps running what it installed. That is deliberate (the protocol has no "revert to the
  previous artifact" and a silent downgrade would be worse), but "remove from the Selector" reading
  as "leave it as it is" will surprise someone.
- Follow-ups: a deliberate rollback — pinning a fleet back to a previous artifact — is still a
  re-upload, because the store keeps one version per package name. Version history in the package
  store is a separate decision. So is a bulk restart across a selected set of Agents, which the
  fleet view's unused checkboxes already suggest.
