# ADR-0016: What a package is, and how the Server — not the host — aims it at Agents

- **Status:** 🟢 accepted
- **Date:** 2026-08-05
- **Deciders:** Markus Brigl

## Context

Package delivery works end to end (ADR-0015): an operator uploads an artifact through the REST API
and every capable Agent downloads, verifies, applies, and health-gates it, reporting progress on the
way. What it cannot do on its own is aim.

**Without a Selector, a package is offered to the whole fleet.** Configurations are targeted by a
Selector ([ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)) — matched
against the attributes an Agent reports. A package that carries no Selector at all is offered to
every Agent whose Supervisor names it, at once. Goal 9 states that a change may address *"the whole
fleet or a chosen subset of it, so a configuration can be rolled out to part of the fleet before all
of it"*, and the vision applies that same expectation to software: *"it can update an agent's binary
in place"* as part of the same control loop. A binary is precisely the change an operator wants to
try on five hosts before three hundred.

**Naming the package on the host decides on the host, not on the Server.** A `[[supervisor]]` block
that opts in by naming one (`package = "otelcol"`) makes the two things an operator most needs to
steer — *who* gets an update and *which* artifact they get — settings in a file on every managed
machine. A fleet spanning `linux/amd64`, `linux/arm64`, and Windows then needs one package name per
platform and a matching `client.toml` on each host, even though every Agent already reports
`host.arch` and `os.type` in its description. Nothing about that is reachable from the REST API or
the bundled UI, which is what goal 5 promises an operator.

The protocol is not what is in the way. The Baseline describes `PackagesAvailable` as *"the packages
that are available on the Server **for this Agent**"* — the offer is per-Agent by design, and its
`all_packages_hash` gates re-offering per Agent as well. The Server already composes a per-Agent
remote configuration this way; a single global package offer has no reason to exist.

### Undoing a rollout

A rollout can be aimed, watched, and — when the new binary will not stay up — undone by the Agent
itself: the Supervisor restores the binary it replaced and reports `InstallFailed` (ADR-0015). The
other kind of undo is the one an operator asks for an hour later: *the new version starts fine and
behaves badly, put the fleet back.*

A store that keeps **one** package per name answers that only by producing the old artifact again —
a new upload or a new source replaces what was there, and nothing remembers what it was. So an
operator either re-uploads the file they hope they still have, or re-points the source at the
previous release. Neither is hard, and for a **referenced** package (ADR-0017) it is genuinely
cheap: the previous URL and its checksum are both on the release page, so a rollback is two fields.
There is also a shape that works without any change: keep `otelcol-0.156` and `otelcol-0.157` as
separate packages and move the Selector between them.

So this is not a capability gap. It is three smaller things:

- **It is not one action.** "Put it back" requires the operator to know what "back" was, at the
  moment they are least calm.
- **It is not recorded.** Nothing on the Server says what this package was before, so nobody can
  answer "what changed here?" without external notes.
- **It is asymmetric.** A referenced package rolls back from public information; an uploaded one
  rolls back only if someone kept the file.

The protocol has no objection: a `PackageAvailable` is *"an offer from the Server to the Agent to
install a new package or initiate an **upgrade or downgrade** of a package that the Agent already
has"*. A rollback is not a special mechanism — it is the ordinary offer, naming the older artifact.

## Decision

We will give a **Package a Selector**, exactly as a Configuration has one, and make the **Server**
decide which artifact an Agent is offered — so a rollout is aimed from the REST API and the UI, not
from a file on each host.

1. **A Package carries a Selector.** An empty Selector means the whole fleet, as with
   Configurations. The Server offers an Agent only the packages whose Selector matches its reported
   attributes, and computes `all_packages_hash` over *that* Agent's set, so the Baseline's re-offer
   gate keeps working per Agent.

2. **A Supervisor says only whether it accepts updates, never which one.**
   `accepts_packages = true` means "this Managed Process's binary is updated from the top-level
   package the Server selects for me"; absent, the Supervisor takes no package offers. There is no
   `package = "name"` key: choosing the artifact is the Server's job, and a second way to choose it
   on the host would keep the decision in the place this decision moves it out of. A `client.toml`
   carrying `package = "…"` fails at startup with a message naming the replacement — loudly, as
   ADR-0008 requires, never silently ignored.

   Pinning one host to a specific artifact does not disappear; it lives on the Server, where a
   Selector on that host's `host.name` (or an operator attribute) expresses it — and, unlike a line
   in a file on that machine, is visible in the fleet view.

3. **One top-level package per Agent, resolved by specificity.** The Baseline states
   there is *"normally only one top-level package, which implements the primary functionality of the
   Agent"*, and a Supervisor has one binary to replace. Where several top-level packages match an
   Agent, the **most specific Selector wins** — the one naming the most attributes. That is what
   makes the shape an operator actually reaches for work: a fleet-wide package with an empty
   Selector, plus a narrower one aimed at the hosts a rollout starts on, which overrides it for
   exactly those and leaves everyone else alone.

   Only a *tie* — two equally specific Selectors both matching one Agent — has no defensible
   answer. That Agent is offered nothing, and the fleet view says why on the Agent itself
   (`package_conflict`), because a rollout that silently never starts is worse than one that
   explains itself. Matching addons are offered alongside the chosen top-level package.

   Refusing the ambiguity at the API instead — rejecting a Selector that could overlap another —
   does not work: every package starts with an empty Selector, which overlaps everything, so a store
   holding two packages could never have a Selector set on either. It also forbids the
   fleet-wide-plus-canary shape outright, which is the whole point.

4. **The Selector is set through its own sub-resource**,
   `PUT /api/v1/packages/{name}/selector`, with the same JSON shape a Configuration uses. The
   artifact upload keeps its contract (`PUT /api/v1/packages/{name}?version=…` with the artifact as
   the body); a package without a Selector behaves exactly as one with an empty Selector, so no
   stored package and no generated client breaks.

### One step back

We will have the package store remember **exactly one** previous version per package, and add
`POST /api/v1/packages/{name}/rollback`, which makes it the current one again.

5. **One step, not a catalogue.** Replacing a package's artifact — by upload or by
   source — moves the descriptor it replaces into `previous`. A rollback swaps `current` and
   `previous`, so the thing rolled back *from* becomes the next `previous` and pressing the button
   twice returns to where it started. That is what one button should mean.

6. **What is remembered costs what it costs.** For a referenced package it is a URL, a
   checksum, a version and any headers — nothing measurable. For an uploaded one the previous
   artifact file is kept alongside the current one, so a package occupies at most twice its size.
   Bounded, and the bound is the reason for keeping one rather than many: an agent binary is
   hundreds of megabytes, and a Server that accumulates every version an operator ever pushed
   becomes an artifact registry by accident. A fleet that wants a catalogue should point at one —
   which is exactly what a referenced package does.

7. **A rollback changes only what is offered, never whom.** The Selector belongs to the
   package and is untouched: which Agents a package reaches is a separate decision from which bytes
   they get, and mixing the two would make an undo do more than it says.

8. **The fleet view shows what "back" is.** A rollback button that does not say what it
   will install is a dare, so the package list carries the previous version and, for a referenced
   package, its source. An operator sees `0.157.0 ← 0.156.0` before choosing.

9. **A package with no previous version cannot be rolled back**, and the API says so
   (`409`) rather than silently doing nothing. That is the state of every package at its first
   upload.

## Alternatives considered

- **Leave targeting to `client.toml`.** Rejected. It works — the platform case is solved by naming
  `otelcol-linux-amd64` per host — but it puts the rollout decision on three hundred machines and
  outside the API that goal 5 makes the integration contract. A staged rollout then means editing
  and redistributing host configuration, which is the problem this project exists to remove.
- **Reuse Configurations to carry the package name.** Rejected. It would let an operator aim a
  rollout with the mechanism that already aims, but it conflates two lifecycles: a configuration is
  applied by restarting on new files, a package is verified, swapped, health-gated, and rolled back.
  Tying them means a config change and a binary change cannot be reasoned about — or reverted —
  separately.
- **Keep `package = "name"` alongside the new opt-in, as an explicit pin.** Rejected, after weighing
  it as the compatible option. It would spare existing `client.toml` files a change, and a pin is
  occasionally what an operator wants — a test host that must not follow the fleet. But it leaves two
  ways to decide the same thing, one of them on the machine this decision is trying to stop editing,
  and the pin is expressible on the Server anyway: a Selector matching that host's `host.name`. One
  mechanism, visible in the fleet view, beats two that can disagree.
- **One key with two meanings (`package = true` or `package = "name"`).** Rejected. It is the most
  compact spelling and breaks nothing, but the same key then means "whether" in one form and "which"
  in the other — a distinction a reader has to know rather than see. Config keys are read under
  pressure; this one would be read wrong.

- **Split the upload into metadata and artifact** (`PUT /api/v1/packages/{name}` for JSON including
  the Selector, `PUT /api/v1/packages/{name}/file` for the bytes). Rejected for now: it is the
  tidier REST shape and mirrors Configurations better, but it breaks the published v1 contract for a
  gain that a sub-resource delivers additively. Worth revisiting if the package model grows further.

- **Refuse an overlapping Selector at the API.** Rejected on contact with the code, which is the
  honest reason: with every existing package carrying an empty Selector, the first `PUT` is always
  refused, and there is no order in which two packages can be narrowed — the store cannot leave the
  state it starts in. Resolving at offer time has no such dead end, and it turns the common case
  (a default plus an override) from an error into the mechanism.
- **Target by Instance UID instead of a Selector.** Rejected. Naming Agents individually pins a
  rollout to identities that are reassignable (`AgentIdentification`) and says nothing about *why*
  those hosts were chosen. A Selector over reported attributes survives re-identification and is
  self-documenting — and it is already the project's vocabulary.

- **Re-upload or re-point instead of rolling back.** The honest baseline, and it is why the one-step
  rollback is small. It costs an operator two fields for a referenced package, and for an uploaded
  one it costs whatever it costs to still have the file. Rejected because the moment it matters is
  an incident, and an incident is exactly when "find the old artifact" is worst — but it is a close
  call, and without the rollback nothing is broken.
- **Keep every version, with a retention policy.** The complete answer, and a much larger one: it
  needs a version key on every package, a listing per package, garbage collection, and disk
  accounting nobody asked for. It also turns the Server into a small artifact registry, which the
  specification does not ask it to be. Rejected in favour of the one step that answers the actual
  question; a fleet with a registry already has referenced packages.
- **Model rollback as a Selector move between per-version packages.** Works without code, and is
  genuinely good practice: `otelcol-0.156` and `otelcol-0.157` side by side, the Selector deciding.
  Rejected as *the* answer because it makes every rollout a two-package dance and leaves the fleet's
  state spread across package names — but it stays a perfectly reasonable way to work, and this
  decision does not take it away.
- **Roll back automatically when health degrades after an install.** Tempting, and far more than an
  undo button: it needs a definition of "degrades" that is not "the process exited" (that case is
  already handled by the apply grace), a window over which to judge it, and a way to stop a flapping
  fleet from oscillating between two versions. Rejected as a different decision with its own risks,
  not as a bad idea.

## Sources / Prior art

- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"The PackagesAvailable message describes the packages that are available on the Server for this
  Agent"*, and *"There is normally only one top-level package, which implements the primary
  functionality of the Agent"*: the per-Agent offer (a name mapped to one offered version and hash
  *for this Agent*), which is both the constraint resolution satisfies and the licence for holding
  what is not offered.

- [OpAMP specification § PackageAvailable (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"an offer from the Server to the Agent to install a new package or initiate an upgrade or
  downgrade of a package that the Agent already has"*: a downgrade is the ordinary offer, so nothing
  on the wire changes for a rollback.

- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — checked as the behavioural oracle: its
  example Server only logs `PackagesAvailable`/`ServerProvidedAllPackagesHash` and never offers a
  package, so there is no upstream targeting behaviour to copy. The model is ours to choose, within
  what the protocol already provides.

- [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md) — the Selector semantics
  this reuses verbatim (every pair must equal a reported attribute; an empty Selector matches all),
  so an operator learns one targeting mechanism, not two.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) — the delivery, verification, and
  health-gating this leaves untouched; only *who is offered what* is decided here.

- [ADR-0015](0015-package-delivery-for-managed-processes.md) — the Agent-side rollback the one-step
  rollback is deliberately *not*: that one restores a binary that would not start, within seconds,
  without an operator. This one is a decision a human makes later, about a binary that runs.
- [ADR-0017](0017-package-artifacts.md) — referenced packages, which make the previous
  version free to remember and, for many fleets, already make rolling back a two-field edit.

## Consequences

- Positive: a binary rollout can be staged — a Selector on `role`, `host.name`, or an operator
  attribute reaches five hosts first, and widening it is one API call. This is goal 9 applied to
  software rather than only to configuration.
- Positive: a heterogeneous fleet stops needing per-host wiring. `host.arch` and `os.type` are
  already reported, so one package per platform, each with a Selector, updates every machine from
  the Server — and the UI can show which Agents a package targets, as it already does for
  Configurations.
- Negative / trade-offs: `all_packages_hash` is per-Agent, so the Server cannot keep one
  precomputed aggregate. The cost is small (a hash over the matching set per exchange) but it is a
  real change to the gate that stops re-offering, and getting it wrong means either a re-offer loop
  or a missed update — it needs its own test.
- Negative / trade-offs: **this breaks every `client.toml` that names a package.** `package = "…"`
  is refused at startup and must become `accepts_packages = true`, with the artifact choice moved
  into a Selector on the Server. A one-line edit per host, but a required one, and a Client that is
  updated before its Server has Selectors configured will accept an offer chosen by an empty
  Selector — i.e. the fleet-wide package — which is safe, but worth saying out loud in the release
  note.
- Negative / trade-offs: specificity is a precedence rule, and precedence rules have to be learned.
  "The Selector naming more attributes wins" is simple, but an operator who expects "last write
  wins" or "most recently uploaded wins" will be surprised once.
- Negative / trade-offs: a tie leaves an Agent with no package at all until someone narrows a
  Selector. It is reported on the Agent in the fleet view and in the Server's log, but it is a
  state an operator can reach by widening a Selector that was fine before, and nothing warns at the
  moment of the change — only afterwards, on the Agents it affects.
- Negative / trade-offs: a Selector that stops matching an Agent does **not** uninstall anything —
  the Agent keeps running what it installed. That is deliberate (the protocol has no "revert to the
  previous artifact" and a silent downgrade would be worse), but "remove from the Selector" reading
  as "leave it as it is" will surprise someone.

- Follow-ups: a bulk restart across a selected set of Agents, which the fleet view's unused
  checkboxes already suggest.

- Positive: undoing a rollout is one action that names what it will do, instead of a search for an
  artifact during an incident.
- Positive: the Server can answer "what was here before?", which is the smallest useful form of a
  rollout record.
- Negative / trade-offs: an uploaded package occupies up to twice its size on the Server. For a
  fleet with several large packages that is real disk, and there is no setting to opt out — keeping
  the previous version is the whole feature.
- Negative / trade-offs: "one step" is a promise that will be tested. The first time someone rolls
  back twice expecting to reach a version from last week, they will find themselves back where they
  started. The fleet view showing exactly one previous version is what has to make that obvious.
- Negative / trade-offs: a rollback is a new offer like any other, so it travels at the speed of the
  control loop — Agents pick it up on their next exchange, and one that is offline stays on the new
  version until it returns. Nothing here makes an undo faster than a rollout.
- Follow-ups: a rollout record worth the name — who changed a package, when, and to what — which is
  a broader question than packages and would touch Configurations too. And automatic rollback on
  post-install health, which needs its own definition of failure.
