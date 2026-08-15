# ADR-0019: One step back — the package store remembers the version it replaced

- **Status:** ⚪ superseded by [ADR-0052](0052-a-package-is-a-versioned-set.md)
- **Date:** 2026-08-06
- **Deciders:** Markus Brigl

## Context

A rollout can now be aimed, watched, and — when the new binary will not stay up — undone by the
Agent itself: the Supervisor restores the binary it replaced and reports `InstallFailed`
(ADR-0015). What has no answer is the other kind of undo, the one an operator asks for an hour
later: *the new version starts fine and behaves badly, put the fleet back.*

Today that means producing the old artifact again. The store keeps **one** package per name — a new
upload or a new source replaces what was there, and nothing remembers what it was. So an operator
either re-uploads the file they hope they still have, or re-points the source at the previous
release. Neither is hard, and for a **referenced** package (ADR-0018) it is genuinely cheap: the
previous URL and its checksum are both on the release page, so a rollback is two fields. There is
also a shape that already works today without any change: keep `otelcol-0.156` and `otelcol-0.157`
as separate packages and move the Selector between them.

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

We will have the package store remember **exactly one** previous version per package, and add
`POST /api/v1/packages/{name}/rollback`, which makes it the current one again.

1. **One step, not a catalogue.** Replacing a package's artifact — by upload or by source — moves
   the descriptor it replaces into `previous`. A rollback swaps `current` and `previous`, so the
   thing rolled back *from* becomes the next `previous` and pressing the button twice returns to
   where it started. That is what one button should mean.

2. **What is remembered costs what it costs.** For a referenced package it is a URL, a checksum, a
   version and any headers — nothing measurable. For an uploaded one the previous artifact file is
   kept alongside the current one, so a package occupies at most twice its size. Bounded, and the
   bound is the reason for keeping one rather than many: an agent binary is hundreds of megabytes,
   and a Server that accumulates every version an operator ever pushed becomes an artifact registry
   by accident. A fleet that wants a catalogue should point at one — which is exactly what a
   referenced package does.

3. **A rollback changes only what is offered, never whom.** The Selector belongs to the package and
   is untouched: which Agents a package reaches is a separate decision from which bytes they get,
   and mixing the two would make an undo do more than it says.

4. **The fleet view shows what "back" is.** A rollback button that does not say what it will install
   is a dare, so the package list carries the previous version and, for a referenced package, its
   source. An operator sees `0.157.0 ← 0.156.0` before choosing.

5. **A package with no previous version cannot be rolled back**, and the API says so (`409`) rather
   than silently doing nothing. That is the state of every package at its first upload.

## Alternatives considered

- **Keep the status quo: re-upload or re-point.** The honest baseline, and it is why this ADR is
  small. It costs an operator two fields for a referenced package, and for an uploaded one it costs
  whatever it costs to still have the file. Rejected because the moment it matters is an incident,
  and an incident is exactly when "find the old artifact" is worst — but it is a close call, and if
  this ADR is rejected nothing is broken.
- **Keep every version, with a retention policy.** The complete answer, and a much larger one: it
  needs a version key on every package, a listing per package, garbage collection, and disk
  accounting nobody asked for. It also turns the Server into a small artifact registry, which the
  specification does not ask it to be. Rejected in favour of the one step that answers the actual
  question; a fleet with a registry already has referenced packages.
- **Model rollback as a Selector move between per-version packages.** Works today, needs no code,
  and is genuinely good practice: `otelcol-0.156` and `otelcol-0.157` side by side, the Selector
  deciding. Rejected as *the* answer because it makes every rollout a two-package dance and leaves
  the fleet's state spread across package names — but it stays a perfectly reasonable way to work,
  and this decision does not take it away.
- **Roll back automatically when health degrades after an install.** Tempting, and far more than an
  undo button: it needs a definition of "degrades" that is not "the process exited" (that case is
  already handled by the apply grace), a window over which to judge it, and a way to stop a flapping
  fleet from oscillating between two versions. Rejected as a different decision with its own risks,
  not as a bad idea.

## Sources / Prior art

- [OpAMP specification § PackageAvailable (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"an offer from the Server to the Agent to install a new package or initiate an upgrade or
  downgrade of a package that the Agent already has"*: a downgrade is the ordinary offer, so nothing
  on the wire changes for this.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) — the Agent-side rollback this is
  deliberately *not*: that one restores a binary that would not start, within seconds, without an
  operator. This one is a decision a human makes later, about a binary that runs.
- [ADR-0018](0018-packages-imported-from-a-url.md) — referenced packages, which make the previous
  version free to remember and, for many fleets, already make rolling back a two-field edit.
- [ADR-0017](0017-selector-targeted-packages.md) — the Selector this decision keeps its hands off.

## Consequences

- Positive: undoing a rollout becomes one action that names what it will do, instead of a search for
  an artifact during an incident.
- Positive: the Server can answer "what was here before?", which is the smallest useful form of a
  rollout record.
- Negative / trade-offs: an uploaded package now occupies up to twice its size on the Server. For a
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
