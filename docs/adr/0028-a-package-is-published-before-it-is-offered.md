# ADR-0028: Packages and Configurations are published before they are offered — uploading or saving stages them, releasing them is its own act

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

For Configurations, this extends
[ADR-0012](0012-selector-targeted-configurations-and-rest-api.md): the resource, its
Selector, and the composed config map survive unchanged; points 8–14 gate *when* a saved
Configuration enters composition. The decision is the same for packages and Configurations —
saved is not offered — with one structural difference argued below.

## Context

Uploading an artifact is currently the whole of a rollout. `PUT /api/v1/packages/{name}` stores the
bytes, and from that moment the package is a candidate in every offer resolution: the next Agent
that reports, whose type matches (ADR-0017), whose platform has an artifact (ADR-0017) and whom the
Selector aims at (ADR-0016), is offered it and installs it. There is no moment between "the bytes
are on the Server" and "the fleet is taking them".

That collapses two decisions an operator makes separately:

1. **Is this artifact here, correct, and described?** Its version, its platform, its signature, the
   type it is built for, the Selector that aims it — five fields that arrive over up to four
   requests (ADR-0017 keeps bytes and aim apart on purpose), each of them a chance to be wrong.
2. **May the fleet have it?**

Between the first upload and the last field, the package is live and incompletely described. A
Selector left at its default from a previous package aims a Collector build at the whole fleet the
instant its bytes land; ADR-0016 already records this failure mode ("a state an operator can reach
by widening a Selector that was fine before, and nothing warns at the moment of the change — only
afterwards, on the Agents it affects"). ADR-0017 narrowed the window — an untyped package reaches
nobody, so a fresh name is inert until its type is set — but it closed it only for *new* names.
Replacing the artifact of a package that is already typed and aimed distributes on upload, which is
exactly the case a staged release wants to control.

The other half is that this Server has no way to *prepare* a rollout. Everything else about a
package can be set up front; only its distribution cannot be held back. An operator who wants to
upload five platforms' artifacts and release them together has to upload them one at a time into a
live rollout and let the fleet take them as they arrive.

The Baseline gives the Server full latitude here: `PackagesAvailable` is "the packages that are
available on the Server **for this Agent**", and which packages those are is the Server's decision.
Withholding one is not a protocol deviation; ADR-0017 already withholds untyped ones.

Saving a Configuration is distributing it. `PUT /api/v1/configurations/{name}` persists the file
and inserts it into the in-memory map the control loop reads
([`configs.rs`](../../crates/server/src/configs.rs#L185-L201)); the next resolution composes it
into every matching Agent's offer, and over WebSocket that offer is pushed within seconds. There
is no moment between "the operator pressed save" and "the fleet is applying it" — every keystroke
committed in the bundled UI is a rollout, restarting every matching Managed Process.

Configurations collapse the same two decisions — *is this correct and described?* and *may the
fleet have it?* — and they are the resource an operator edits far more often than a package.
ADR-0016 keeps the split for packages ("saved is not offered") now that packages are versioned
Sets. The operator requirement is the same for both resources: saving must only save.

**Why the package mechanism does not transfer verbatim.** A package's gate is one flag, with
"retract first" as the staging path for changes, because withdrawing a package's offer uninstalls
nothing (point 5) — the fleet keeps running what it installed. A Configuration does not
have that luxury: it is an *entry in a composed `AgentConfigMap`* (ADR-0012), and removing an
entry changes the map, its hash, and therefore what the fleet runs — the Agent applies the map
without the entry, the file is removed, the process restarts. A retract-to-edit rule would route
every staged edit through an active fleet change, which is the opposite of staging. The gate for
Configurations must let the released revision keep being offered *while* the next one is
prepared beside it.

The Baseline permits this as well: what `remote_config` a Server offers is the Server's own
composition, and nothing obliges it to offer what it merely stores — the same latitude as for
`PackagesAvailable`.

## Decision

We will give a package a **publication state** — `draft` or `published` — and offer a package only
while it is published (points 1–7). We will give every Configuration **two revisions — a draft and
a published one** — compose offers from published revisions only, and make publication its own act
(points 8–14).

### Packages

1. **The state belongs to the name**, beside the Selector and the Agent type, in `<name>.json`. It
   is one property of the rollout, not of the bytes: a package publishes as a whole, with every
   platform's artifact it holds, exactly as ADR-0017's type and ADR-0016's aim apply to all of them.

   It is set through its own sub-resource, `PUT /api/v1/packages/{name}/publication` with
   `{"published": true|false}`, beside `/type` and `/selector`.

2. **A package created here is a draft.** Uploading bytes (or referencing them, ADR-0017) to a name
   the store does not hold creates a draft; uploading to a name it does holds leaves the state as
   it is. So a first upload stages, and *replacing* the artifact of a published package still
   distributes — that is the ordinary in-place upgrade, and making it re-publish every time would
   train an operator to press the button without reading it.

   To stage a replacement instead, retract the package first. This is the one asymmetry in the
   decision and it is deliberate: the state answers "may the fleet have this package", not "may the
   fleet have these particular bytes".

3. **Fit gains a step, and it runs first.** Offer resolution drops every package that is not
   published, before the type comparison and before the platform filter. A draft is not a candidate
   for anyone, whatever its type, its Selector, or its artifacts say — the same shape ADR-0017 gave
   the type, for the same reason: a state whose whole purpose is "not yet" cannot be one that some
   other field can override.

4. **A store written before this ADR loads as published.** Absent in the file means published, not
   draft. This is the opposite of ADR-0017's choice for the type, and the difference is what the
   unset state *was*: there, a package with no type could reach an Agent it was not built for, so
   the safe reading was inert and the migration cost was worth an outage. Here, a package that an
   operator uploaded under the old rule is not unsafe — it is in flight. Reading it as a draft
   would stop every rollout in the fleet at once, silently, on an upgrade whose changelog entry
   nobody had to read yet.

5. **Retracting withdraws the offer and uninstalls nothing.** ADR-0016 settled this for the
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

### Configurations

8. **`PUT /api/v1/configurations/{name}` writes the draft revision** — the whole writable spec
   (`selector`, `body`, `role`, and ADR-0012's `service_name` if accepted), on create and on
   every later edit. A draft is never composed and never offered, whatever it says. Saving only
   saves.

9. **Composition reads published revisions only.** Matching, the composed entries, and the hash
   gate (ADR-0012, goal 3) work exactly as today, over the published revision of each
   Configuration. A Configuration that has never been published reaches nobody.

10. **Publication is a sub-resource**, `PUT /api/v1/configurations/{name}/publication` with
    `{"published": true}` — the shape of point 1. It promotes the draft to published as one
    snapshot of the whole spec; body and aim release together, atomically, never a half-edited
    mixture.

11. **Retraction is honest about not being inert.** `{"published": false}` removes the published
    revision; the entry leaves every composed map. Unlike a package, that *is* an active change:
    an Agent still matching other Configurations applies the shrunken map — entry file removed,
    process restarted — and only an Agent left matching nothing keeps running what it runs
    (goal 9, "no match, no offer"). The UI states this on the button; deletion of the whole
    Configuration behaves the same way and always has.

12. **A store written before this ADR loads as published** (draft equal to published). Point 4's
    argument, stronger here: reading existing files as drafts would not merely stop rollouts
    silently — it would empty every composed map and actively reconfigure the entire fleet on
    upgrade.

13. **The API and UI show both revisions and their difference.** `GET` answers the draft (what
    editing operates on), the published state, and whether they differ — the "pending changes"
    marker, so a fleet that is not changing after a save is explainable at a glance. The editor
    shows the reach a draft would have before it is released (point 6's purpose); the fleet
    view's "matching Configurations" per Agent keeps answering for what is in force — the
    published revisions.

14. **The bundled UI splits along the seam**: *Save* writes the draft and releases nothing;
    *Publish* releases the snapshot; a published Configuration's second action reads *Retract*
    with point 11's warning. The press that changes the fleet is one press, and it is never the
    one that carries the text.

## Alternatives considered

- **Leave it to the Selector: upload with a Selector nobody matches, then widen it.** Costs nothing
  to build, and an operator can do it today. Rejected: it encodes "not yet" in the field that means
  "for whom", so the two cannot be read apart afterwards — a package aimed at `rollout = staging`
  is indistinguishable from one held back, and widening is a rollout's start *and* its aim in one
  edit. ADR-0017 rejected the structurally identical "leave the Agent type to the Selector".
- **A state per artifact rather than per package.** Publishing one platform at a time is a finer
  rollout, and ADR-0017 does keep artifacts apart per platform. Rejected: the aim and the type are
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
- **One flag with retract-to-edit for Configurations** — the package mechanism of points 1–7 and
  ADR-0016 applied verbatim: published Configurations immutable, retract to change. Rejected
  above: retraction actively un-applies an entry, so the staging path would itself be a fleet
  change. The two-revision shape exists precisely because Configurations compose where packages
  do not.
- **Editing a published Configuration distributes directly**, gating only the first release —
  point 2's asymmetry (replacing a published artifact still distributes). Rejected: for
  packages the in-place replacement is the routine act and staging the exception; for
  Configurations the *edit* is the very act the requirement gates, and edits are frequent,
  incremental, and made in a text area — the case with the most to gain from "save, review reach,
  then release".
- **Versioned Configurations** — a new version is a new Configuration, publication moves between
  versions (ADR-0016's answer for packages). Rejected as YAGNI: the name is the config-map key
  and a file name on every host (ADR-0012), so versions would either move the key — restarting
  every process to deliver a rename — or need an identity layer above the name. Staging needs
  two revisions; history and audit remain the separate follow-up ADR-0012 already lists.
- **Incremental release** — batches with automatic pause, as BindPlane rolls out. Out of scope
  for Configurations for the same reason as for packages: this decision is the gate, not the
  walk, and the fleet-fraction question is what Selector rings (ADR-0016, ADR-0023) answer.
- **A fleet-wide "apply all" transaction** over every pending draft. Rejected: release is
  per-Configuration exactly as aim is, and a cross-resource transaction is a consistency model
  with no present need — publishing two Configurations is two presses, each individually
  reversible.

## Sources / Prior art

- **BindPlane's rollouts** — the same product ADR-0023 measured against. A configuration change is
  *staged* and compared against the version in force, and a separate **Start Rollout** action
  releases it, incrementally, with collectors moving through `pending` → `configuring` → healthy.
  The split this ADR makes — prepare, then release — is theirs; the incremental release is not
  taken, because this decision is about a gate and the fleet-fraction question is what ADR-0016's
  Selector already answers. For Configurations the model is exactly staged-then-released (checked
  2026-08-12): "the currently deployed version … is editable. Edits are not instantly applied to
  collectors", with an explicit *Start Rollout* action releasing them. The staged/live split is
  taken; the batched incremental walk is not (see Alternatives).
  <https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts>
- **Artifact registries' promotion workflows** (JFrog Artifactory, Harness, Google Artifact
  Registry): an identical binary moves development → staging → production, and only vetted
  artifacts advance into the repository that production consumes. The lesson taken is the gate
  between "stored" and "consumable"; the lesson not taken is the repository-per-stage topology,
  which for one Server and one fleet would be three stores to keep in sync.
  <https://www.harness.io/harness-devops-academy/artifact-lifecycle-management-strategies>
- [ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md) — the in-repo precedent for versioned
  packages: saved is not offered, publication as a sub-resource, existing stores load as
  published. The Configuration gate diverges from the package gate on the mechanism (two
  revisions instead of one flag) and states why.
- [Grafana Fleet Management — architecture](https://grafana.com/docs/grafana-cloud/send-data/fleet-management/introduction/architecture/)
  (checked 2026-08-12) — the counter-model the Configuration gate moves away from: "the server
  configuration changes immediately", delivery lagging only by the poll interval. Notably that
  lag is the accidental safety margin a pushing WebSocket Server (ADR-0007) does not have —
  which is why the gate matters more here than there.
- **OpAMP Specification, `PackagesAvailable`** — "the packages that are available on the Server for
  this Agent". The set is the Server's to compose, which is what makes a withheld package
  conformant rather than a deviation (docs/CONFORMANCE.md records this for `OffersPackages`).
  Likewise the offered `remote_config` is the Server's composition to make
  ([OpAMP specification `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md));
  withholding a stored Configuration deviates from nothing.

## Consequences

- Positive: the fleet cannot take an artifact that is uploaded but not yet described. Five
  platforms' artifacts can be uploaded and released together. The moment a rollout starts is a
  single, named, logged act rather than the side effect of a file transfer — and it is reversible
  in one request, which "widen the Selector" never was.
- Positive: the failure ADR-0016 records — a Selector left too wide from the previous package —
  stops being reachable by uploading, because the upload no longer distributes.
- Positive: saving a Configuration becomes safe. A body can be drafted, its reach inspected, and
  released as one deliberate, named act — reversible per Configuration — instead of every save
  being an immediate fleet-wide restart of whatever matches.
- Positive: the sharpest edge of ADR-0012's empty-Selector degenerate case — one save reaching
  the whole fleet *instantly* — is gone; with ADR-0012 the two together let an operator state
  whom a Configuration is for and when it takes effect.
- Negative / trade-offs: one more state for an operator to hold in mind, and one more way for a
  package to reach nobody — beside "no agent type" and "a Selector that matches no one" there is
  now "still a draft". Every one of those already has to be visible in the package view, so this
  adds a third label rather than a new kind of confusion. Replacing a published package's artifact
  still distributes on upload (point 2), which is the one case where an operator might expect the
  gate to hold and it does not.
- Negative / trade-offs: the REST API grows a fifth sub-resource under a package, and scripts that
  upload a *new* package now need a second call to release it. Existing scripts against existing
  packages are unaffected (point 4).
- Negative / trade-offs: every Configuration change now takes two presses, deliberately and
  always — the asymmetry point 2 keeps for routine package bumps is consciously not kept for
  Configurations, per the operator requirement. The mitigation for "press fatigue" is that
  *Save* is frequent and *Publish* is the rare, meaningful press.
- Negative / trade-offs: this changes the semantics of an existing v1 route — `PUT` on a
  Configuration no longer distributes — which the package gate avoids. Scripts that save and
  expect delivery need one more call; existing *stored* Configurations stay in force
  (point 12), so the break is in workflows, not in running fleets.
- Negative / trade-offs: for Configurations, one more state to hold in mind, and a new
  confusion made possible — "I saved but nothing changed" — answered by the pending-changes
  marker (point 13), which the UI must make impossible to miss. The persisted file grows a
  second revision.
- Follow-ups: whether publication should be recorded with *when* and *by whom* — an audit trail
  over the REST plane — is a separate decision that needs an identity the API does not have today,
  for packages and Configurations alike. So is an incremental release that walks a fraction of the
  fleet by itself, rather than through a Selector an operator edits. A diff view between a
  Configuration's draft and published revision is a further follow-up.
