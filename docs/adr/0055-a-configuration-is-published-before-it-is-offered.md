# ADR-0055: A Configuration is published before it is offered — saving stages a draft, releasing it is its own act

- **Status:** ⚪ superseded by [ADR-0061](0061-a-rollout-is-an-explicit-act.md)
- **Date:** 2026-08-12
- **Deciders:** Markus Brigl

Extends [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md): the resource,
its Selector, and the composed config map survive unchanged; this gates *when* a saved
Configuration enters composition. It is
[ADR-0043](0043-a-package-is-published-before-it-is-offered.md)'s decision — saved is not offered
— carried from packages to Configurations, with one structural difference argued below.

## Context

Saving a Configuration is distributing it. `PUT /api/v1/configurations/{name}` persists the file
and inserts it into the in-memory map the control loop reads
([`configs.rs`](../../crates/server/src/configs.rs#L185-L201)); the next resolution composes it
into every matching Agent's offer, and over WebSocket that offer is pushed within seconds. There
is no moment between "the operator pressed save" and "the fleet is applying it" — every keystroke
committed in the bundled UI is a rollout, restarting every matching Managed Process.

ADR-0043 named the two decisions this collapses — *is this correct and described?* and *may the
fleet have it?* — and split them for packages; ADR-0052 kept the split ("saved is not offered")
when packages became versioned Sets. Configurations, the resource an operator edits far more
often than a package, still collapse them. The operator requirement that triggered this ADR is
exactly ADR-0043's: saving must only save.

**Why the package mechanism does not transfer verbatim.** A package's gate is one flag, with
"retract first" as the staging path for changes, because withdrawing a package's offer uninstalls
nothing (ADR-0043 point 5) — the fleet keeps running what it installed. A Configuration does not
have that luxury: it is an *entry in a composed `AgentConfigMap`* (ADR-0012), and removing an
entry changes the map, its hash, and therefore what the fleet runs — the Agent applies the map
without the entry, the file is removed, the process restarts. A retract-to-edit rule would route
every staged edit through an active fleet change, which is the opposite of staging. The gate for
Configurations must let the released revision keep being offered *while* the next one is
prepared beside it.

The Baseline permits all of this: what `remote_config` a Server offers is the Server's own
composition, and nothing obliges it to offer what it merely stores — the latitude ADR-0043
already claimed for `PackagesAvailable`.

## Decision

We will give every Configuration **two revisions — a draft and a published one** — compose offers
from published revisions only, and make publication its own act.

1. **`PUT /api/v1/configurations/{name}` writes the draft revision** — the whole writable spec
   (`selector`, `body`, `role`, and ADR-0054's `service_name` if accepted), on create and on
   every later edit. A draft is never composed and never offered, whatever it says. Saving only
   saves.

2. **Composition reads published revisions only.** Matching, the composed entries, and the hash
   gate (ADR-0012, goal 3) work exactly as today, over the published revision of each
   Configuration. A Configuration that has never been published reaches nobody.

3. **Publication is a sub-resource**, `PUT /api/v1/configurations/{name}/publication` with
   `{"published": true}` — the ADR-0043 shape. It promotes the draft to published as one snapshot
   of the whole spec; body and aim release together, atomically, never a half-edited mixture.

4. **Retraction is honest about not being inert.** `{"published": false}` removes the published
   revision; the entry leaves every composed map. Unlike a package, that *is* an active change:
   an Agent still matching other Configurations applies the shrunken map — entry file removed,
   process restarted — and only an Agent left matching nothing keeps running what it runs
   (goal 9, "no match, no offer"). The UI states this on the button; deletion of the whole
   Configuration behaves the same way and always has.

5. **A store written before this ADR loads as published** (draft equal to published). ADR-0043
   point 4's argument, stronger here: reading existing files as drafts would not merely stop
   rollouts silently — it would empty every composed map and actively reconfigure the entire
   fleet on upgrade.

6. **The API and UI show both revisions and their difference.** `GET` answers the draft (what
   editing operates on), the published state, and whether they differ — the "pending changes"
   marker, so a fleet that is not changing after a save is explainable at a glance. The editor
   shows the reach a draft would have before it is released (ADR-0043 point 6's purpose); the
   fleet view's "matching Configurations" per Agent keeps answering for what is in force — the
   published revisions.

7. **The bundled UI splits along the seam**: *Save* writes the draft and releases nothing;
   *Publish* releases the snapshot; a published Configuration's second action reads *Retract*
   with point 4's warning. The press that changes the fleet is one press, and it is never the
   one that carries the text.

## Alternatives considered

- **One flag with retract-to-edit** — ADR-0043/ADR-0052 verbatim: published Configurations
  immutable, retract to change. Rejected above: retraction actively un-applies an entry, so the
  staging path would itself be a fleet change. The two-revision shape exists precisely because
  Configurations compose where packages do not.
- **Editing a published Configuration distributes directly**, gating only the first release —
  ADR-0043 point 2's asymmetry (replacing a published artifact still distributes). Rejected: for
  packages the in-place replacement is the routine act and staging the exception; for
  Configurations the *edit* is the very act the requirement gates, and edits are frequent,
  incremental, and made in a text area — the case with the most to gain from "save, review reach,
  then release".
- **Versioned Configurations** — a new version is a new Configuration, publication moves between
  versions (ADR-0052's answer for packages). Rejected as YAGNI: the name is the config-map key
  and a file name on every host (ADR-0012), so versions would either move the key — restarting
  every process to deliver a rename — or need an identity layer above the name. Staging needs
  two revisions; history and audit remain the separate follow-up ADR-0012 already lists.
- **Incremental release** — batches with automatic pause, as BindPlane rolls out. Out of scope
  for the same reason ADR-0043 gave: this decision is the gate, not the walk, and the
  fleet-fraction question is what Selector rings (ADR-0017, ADR-0042) answer.
- **A fleet-wide "apply all" transaction** over every pending draft. Rejected: release is
  per-Configuration exactly as aim is, and a cross-resource transaction is a consistency model
  with no present need — publishing two Configurations is two presses, each individually
  reversible.

## Sources / Prior art

- [ADR-0043](0043-a-package-is-published-before-it-is-offered.md) and
  [ADR-0052](0052-a-package-is-a-versioned-set.md) — the in-repo precedent: saved is not
  offered, publication as a sub-resource, existing stores load as published. This ADR diverges
  on the mechanism (two revisions instead of one flag) and states why.
- [Bindplane — Rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts)
  (checked 2026-08-12) — the comparable product's configuration model is exactly staged-then-
  released: "the currently deployed version … is editable. Edits are not instantly applied to
  collectors", with an explicit *Start Rollout* action releasing them. The staged/live split is
  taken; the batched incremental walk is not (see Alternatives).
- [Grafana Fleet Management — architecture](https://grafana.com/docs/grafana-cloud/send-data/fleet-management/introduction/architecture/)
  (checked 2026-08-12) — the counter-model this decision moves away from: "the server
  configuration changes immediately", delivery lagging only by the poll interval. Notably that
  lag is the accidental safety margin a pushing WebSocket Server (ADR-0007) does not have —
  which is why the gate matters more here than there.
- [OpAMP specification `v0.19.0`](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — the offered `remote_config` is the Server's composition to make; withholding a stored
  Configuration deviates from nothing.

## Consequences

- Positive: saving becomes safe. A body can be drafted, its reach inspected, and released as one
  deliberate, named act — reversible per Configuration — instead of every save being an
  immediate fleet-wide restart of whatever matches.
- Positive: the sharpest edge of ADR-0012's empty-Selector degenerate case — one save reaching
  the whole fleet *instantly* — is gone; with ADR-0054 the two together let an operator state
  whom a Configuration is for and when it takes effect.
- Negative / trade-offs: every change now takes two presses, deliberately and always — the
  asymmetry ADR-0043 kept for routine package bumps is consciously not kept, per the operator
  requirement. The mitigation for "press fatigue" is that *Save* is frequent and *Publish* is
  the rare, meaningful press.
- Negative / trade-offs: this changes the semantics of an existing v1 route — `PUT` no longer
  distributes — which ADR-0043 managed to avoid for packages. Scripts that save and expect
  delivery need one more call; existing *stored* Configurations stay in force (point 5), so the
  break is in workflows, not in running fleets.
- Negative / trade-offs: one more state to hold in mind, and a new confusion made possible —
  "I saved but nothing changed" — answered by the pending-changes marker (point 6), which the
  UI must make impossible to miss. The persisted file grows a second revision.
- Follow-ups: an audit trail (when and by whom published) — still blocked on an API identity, as
  ADR-0043 recorded; a diff view between draft and published revision; incremental release.
