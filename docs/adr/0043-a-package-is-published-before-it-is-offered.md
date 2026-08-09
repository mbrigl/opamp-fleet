# ADR-0043: A package is published before it is offered — uploading stages it, releasing it is its own act

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

Uploading an artifact is currently the whole of a rollout. `PUT /api/v1/packages/{name}` stores the
bytes, and from that moment the package is a candidate in every offer resolution: the next Agent
that reports, whose type matches (ADR-0034), whose platform has an artifact (ADR-0031) and whom the
Selector aims at (ADR-0017), is offered it and installs it. There is no moment between "the bytes
are on the Server" and "the fleet is taking them".

That collapses two decisions an operator makes separately:

1. **Is this artifact here, correct, and described?** Its version, its platform, its signature, the
   type it is built for, the Selector that aims it — five fields that arrive over up to four
   requests (ADR-0031 keeps bytes and aim apart on purpose), each of them a chance to be wrong.
2. **May the fleet have it?**

Between the first upload and the last field, the package is live and incompletely described. A
Selector left at its default from a previous package aims a Collector build at the whole fleet the
instant its bytes land; ADR-0017 already records this failure mode ("a state an operator can reach
by widening a Selector that was fine before, and nothing warns at the moment of the change — only
afterwards, on the Agents it affects"). ADR-0034 narrowed the window — an untyped package reaches
nobody, so a fresh name is inert until its type is set — but it closed it only for *new* names.
Replacing the artifact of a package that is already typed and aimed distributes on upload, which is
exactly the case a staged release wants to control.

The other half is that this Server has no way to *prepare* a rollout. Everything else about a
package can be set up front; only its distribution cannot be held back. An operator who wants to
upload five platforms' artifacts and release them together has to upload them one at a time into a
live rollout and let the fleet take them as they arrive.

The Baseline gives the Server full latitude here: `PackagesAvailable` is "the packages that are
available on the Server **for this Agent**", and which packages those are is the Server's decision.
Withholding one is not a protocol deviation; ADR-0034 already withholds untyped ones.

## Decision

We will give a package a **publication state** — `draft` or `published` — and offer a package only
while it is published.

1. **The state belongs to the name**, beside the Selector and the Agent type, in `<name>.json`. It
   is one property of the rollout, not of the bytes: a package publishes as a whole, with every
   platform's artifact it holds, exactly as ADR-0034's type and ADR-0017's aim apply to all of them.

   It is set through its own sub-resource, `PUT /api/v1/packages/{name}/publication` with
   `{"published": true|false}`, beside `/type` and `/selector`.

2. **A package created here is a draft.** Uploading bytes (or referencing them, ADR-0018) to a name
   the store does not hold creates a draft; uploading to a name it does holds leaves the state as
   it is. So a first upload stages, and *replacing* the artifact of a published package still
   distributes — that is the ordinary in-place upgrade, and making it re-publish every time would
   train an operator to press the button without reading it.

   To stage a replacement instead, retract the package first. This is the one asymmetry in the
   decision and it is deliberate: the state answers "may the fleet have this package", not "may the
   fleet have these particular bytes".

3. **Fit gains a step, and it runs first.** Offer resolution drops every package that is not
   published, before the type comparison and before the platform filter. A draft is not a candidate
   for anyone, whatever its type, its Selector, or its artifacts say — the same shape ADR-0034 gave
   the type, for the same reason: a state whose whole purpose is "not yet" cannot be one that some
   other field can override.

4. **A store written before this ADR loads as published.** Absent in the file means published, not
   draft. This is the opposite of ADR-0034's choice for the type, and the difference is what the
   unset state *was*: there, a package with no type could reach an Agent it was not built for, so
   the safe reading was inert and the migration cost was worth an outage. Here, a package that an
   operator uploaded under the old rule is not unsafe — it is in flight. Reading it as a draft
   would stop every rollout in the fleet at once, silently, on an upgrade whose changelog entry
   nobody had to read yet.

5. **Retracting withdraws the offer and uninstalls nothing.** ADR-0017 settled this for the
   Selector — "a Selector that stops matching an Agent does not uninstall anything; the Agent keeps
   running what it installed" — and the protocol has no revert. Retraction stops the package
   reaching Agents that have not taken it yet; it is not a recall, and the fleet view says so.

6. **A draft still reports the reach it would have.** `targeted_agents` (the count the package view
   shows) keeps answering "how many Agents this would reach", and the view marks the package a
   draft beside it. Counting zero for every draft would make the number useless in precisely the
   situation it was added for — checking the aim of a rollout *before* it starts.

7. **The bundled UI splits along the same seam.** *Upload* and *Update* write the package and leave
   its state alone; *Offer* applies the targeting and publishes; a published package's action reads
   *Retract*. So the button that releases a rollout is the one an operator presses last, and it is
   never the same press that carries the bytes.

## Alternatives considered

- **Leave it to the Selector: upload with a Selector nobody matches, then widen it.** Costs nothing
  to build, and an operator can do it today. Rejected: it encodes "not yet" in the field that means
  "for whom", so the two cannot be read apart afterwards — a package aimed at `rollout = staging`
  is indistinguishable from one held back, and widening is a rollout's start *and* its aim in one
  edit. ADR-0034 rejected the structurally identical "leave the Agent type to the Selector".
- **A state per artifact rather than per package.** Publishing one platform at a time is a finer
  rollout, and ADR-0031 does keep artifacts apart per platform. Rejected: the aim and the type are
  already per name, so a per-artifact state would be the only one of the three that is not, and
  "published on linux, draft on darwin" is a distinction the offer resolution would have to carry
  into every answer. A rollout that starts on part of the fleet is what the Selector is for.
- **More states — `draft`, `staged`, `published`, `deprecated`.** The promotion pipelines of
  artifact registries (development → staging → production) work this way. Rejected as speculative
  here (YAGNI, AGENTS.md §1): this Server has one store and one fleet, and every state beyond the
  two must answer "what does the offer resolution do differently", which none of the extras can
  today. Adding a third later is a new value in one field.
- **An approval queue — publication requires a second operator.** The obvious next step for a
  control this shape. Rejected for now: the REST plane has no notion of *who* is acting (ADR-0013
  authenticates the OpAMP endpoint, not the API), so "a second operator" is not expressible, and
  inventing identities for it is a much larger decision than this one.
- **Make replacing the artifact of a published package stage it too** (strict reading of point 2).
  Safer in the abstract. Rejected: every ordinary version bump would then need two presses, and a
  confirmation that is required every time stops being read — the ADR-0014 lesson about warnings
  that arrive on every offer. An operator who wants that behaviour retracts first, in one press.

## Sources / Prior art

- **BindPlane's rollouts** — the same product ADR-0042 measured against. A configuration change is
  *staged* and compared against the version in force, and a separate **Start Rollout** action
  releases it, incrementally, with collectors moving through `pending` → `configuring` → healthy.
  The split this ADR makes — prepare, then release — is theirs; the incremental release is not
  taken, because this decision is about a gate and the fleet-fraction question is what ADR-0017's
  Selector already answers.
  <https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts>
- **Artifact registries' promotion workflows** (JFrog Artifactory, Harness, Google Artifact
  Registry): an identical binary moves development → staging → production, and only vetted
  artifacts advance into the repository that production consumes. The lesson taken is the gate
  between "stored" and "consumable"; the lesson not taken is the repository-per-stage topology,
  which for one Server and one fleet would be three stores to keep in sync.
  <https://www.harness.io/harness-devops-academy/artifact-lifecycle-management-strategies>
- **OpAMP Specification, `PackagesAvailable`** — "the packages that are available on the Server for
  this Agent". The set is the Server's to compose, which is what makes a withheld package
  conformant rather than a deviation (docs/CONFORMANCE.md records this for `OffersPackages`).

## Consequences

- Positive: the fleet cannot take an artifact that is uploaded but not yet described. Five
  platforms' artifacts can be uploaded and released together. The moment a rollout starts is a
  single, named, logged act rather than the side effect of a file transfer — and it is reversible
  in one request, which "widen the Selector" never was.
- Positive: the failure ADR-0017 records — a Selector left too wide from the previous package —
  stops being reachable by uploading, because the upload no longer distributes.
- Negative / trade-offs: one more state for an operator to hold in mind, and one more way for a
  package to reach nobody — beside "no agent type" and "a Selector that matches no one" there is
  now "still a draft". Every one of those already has to be visible in the package view, so this
  adds a third label rather than a new kind of confusion. Replacing a published package's artifact
  still distributes on upload (point 2), which is the one case where an operator might expect the
  gate to hold and it does not.
- Negative / trade-offs: the REST API grows a fifth sub-resource under a package, and scripts that
  upload a *new* package now need a second call to release it. Existing scripts against existing
  packages are unaffected (point 4).
- Follow-ups: whether publication should be recorded with *when* and *by whom* — an audit trail
  over the REST plane — is a separate decision that needs an identity the API does not have today.
  So is an incremental release that walks a fraction of the fleet by itself, rather than through a
  Selector an operator edits.
