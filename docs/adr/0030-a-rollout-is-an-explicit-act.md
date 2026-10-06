# ADR-0030: A rollout is an explicit act — saving never distributes, and the operator releases per Agent or for all matching Agents

- **Status:** 🟢 accepted
- **Date:** 2026-08-14
- **Deciders:** Markus Brigl

## Context

A publication gate — a package or Configuration that is a draft until published, with publication
as the moment the fleet changes — answers "may the fleet have it?" **once, for the whole fleet,
forever**. The instant a resource is published, every matching Agent converges to it automatically:
the WebSocket loops wake, the polling path recomputes each offer from published content on every
exchange ([`fleet.rs`](../../crates/server/src/fleet.rs#L1030-L1053)), and an Agent that enrols next
week takes the published state without anyone deciding that it should.

The operator requirement goes further than a publication gate can:

1. Saving a package or Configuration must never distribute anything — not on first save, not on
   a later edit.
2. **The fleet view must show, per Agent, what could be rolled out to it** — which package,
   which Configuration, and that it is waiting.
3. **Distribution happens only when the operator says so** — per Agent, or on the resource
   itself for every Agent its Selector currently matches.
4. One model for packages and Configurations alike — not two mechanisms (a published flag on
   packages, two revisions on Configurations).

Publication cannot express this. It is a property of the *resource*, so it cannot hold for one
Agent and not another; and it is a *standing* state, so it keeps distributing to Agents that
appear or start matching later. A draft/published split also leaves two lifecycles to hold in
mind — and the operator has asked for one: content is simply *saved*, and *rolled out* is a fact
about an Agent, not about the content.

The Baseline permits all of this: `PackagesAvailable` is "the packages that are available on the
Server **for this Agent**", and the offered `remote_config` is the Server's own composition — a
Server that composes them per Agent from an operator's explicit decisions deviates from nothing.

## Decision

We will have no draft/published distinction and make **the rollout itself the explicit act**:
per Agent, the Server persists what the operator has released to it — the **assignment** — and
composes every offer from assignments only. Matching becomes a proposal; the operator's press
makes it an offer.

1. **Saving only saves.** A Configuration holds one revision — the saved one; a package carries no
   published flag. `PUT` on the resource (body, Selector, type, entries) changes what *could* be
   rolled out and never what *is*.

2. **The assignment is per Agent, persisted, and pins a snapshot.** In each Agent's record
   (ADR-0025) the Server stores what has been rolled out to that Agent: for Configurations, the
   revision released to it (referenced by content hash; the store retains every revision an
   assignment still references); for packages, the concrete Set (its ADR-0016 identity). The
   precedent in this codebase is `restart_pending` — the one per-Agent, persisted,
   operator-queued intent the system already has; the precedent outside it is WSUS, Jamf, and
   Bindplane, where an approval always binds a concrete version, never "latest". Editing a
   Configuration after rollout therefore changes nothing on any Agent: the Agents keep their
   pinned revision, and the fleet view shows a newer save is waiting.

3. **Offers are composed from assignments only.** The composed config map, its hash, and the
   package offer are built from the Agent's assignments — not from `desired_for` over saved
   content. The hash gate (goal 3, no redundant reconfiguration) works unchanged, over the
   assigned content. Matching (`fits`, `matches`, `resolve`) survives intact but computes the
   **candidate**: what *would* be assigned if the operator rolled out now.

4. **The fleet view shows the difference, per Agent.** For every Agent, the Server derives
   candidate vs. assignment and reports what is waiting: a Configuration not yet rolled out, a
   saved revision newer than the assigned one, a Set version greater than the assigned Set. This
   is Argo CD's OutOfSync made per Agent: the gap between "could run" and "was released" is
   first-class, displayed, and never acted on by the Server alone.

5. **Two rollout acts, one meaning.** `POST /api/v1/agents/{uid}/rollout` releases a named
   resource (or all waiting ones) to one Agent; `POST` on the resource's own `rollout`
   sub-resource releases it to **every Agent its Selector currently matches** — a bulk write of
   the same per-Agent assignments, WSUS's "approve for All Computers" as the widest form of the
   same act. Both pin the content as of that press. Rolling out an empty Set is refused.

6. **An Agent that appears later waits.** A newly enrolled Agent, or one that starts matching
   after a Selector edit or a label move (ADR-0027), receives nothing until an operator rolls
   out to it; it surfaces in the fleet view with everything it could receive marked waiting.
   Widening a Selector thereby is not a distribution event at all — the failure mode ADR-0016
   recorded is structurally unreachable.

7. **Taking an assignment away is honest about what it does.** Removing a Configuration
   assignment (or deleting the Configuration) shrinks the Agent's composed map — an active
   change, applied on the Agent. Removing a package assignment withdraws the offer and uninstalls
   nothing (ADR-0016's rule). Rolling a package back is not a rollout; the way back is
   [ADR-0035](0035-what-reaches-an-agent.md) point 11's.

8. **Immutability follows the assignment.** The freeze of a Set's entries (ADR-0016 point 10) is
   gated on assignment: a Set assigned to at least one Agent has immutable entries. A Set assigned
   to nobody is freely editable.

9. **A store written before this ADR loads as rolled out.** Every Agent record loads with an
   assignment equal to what the published content matched to it at upgrade time; a
   Configuration's saved revision is its former draft, and an unpublished draft becomes plain
   saved content assigned to nobody. Reading existing stores as unassigned would empty every
   composed map and reconfigure the entire fleet on upgrade.

10. **The bundled UI splits along the same seam.** *Save*/*Upload* never distribute; each Agent
    row shows what is waiting for it with a per-Agent *Roll out*; the resource view keeps the
    reach count ("whom would this reach") and offers *Roll out to all matching* as its one
    releasing action. The press that changes the fleet is one press — the rollout, where the
    operator can see exactly whom it hits.

## Alternatives considered

- **Keep publication and add a per-Agent gate behind it.** Two gates in series: publish, then
  roll out. Rejected: the requirement removes the first gate explicitly, and a publication that
  no longer distributes anything answers no question the assignment does not — it would survive
  only as a third state for operators to hold in mind.
- **A standing assignment — "this Agent follows this resource", rollout as subscription.**
  Saving would then distribute to every already-assigned Agent, which is precisely what the
  requirement forbids ("saving must never distribute"). This is Flux's suspend/resume and
  `kubectl rollout pause` — a gate on *time*, not *content* — and the mature fleet systems
  (WSUS, Jamf, Bindplane, Chef environments) all chose pinning instead. So does this ADR.
- **A standing "roll out to all matching, including future matchers" flag on the resource.**
  Publication under a new name: an Agent enrolling later would take content nobody released to
  it. Rejected; the late Agent surfaces as waiting instead (point 6), and if this proves too
  much friction in practice, an explicit opt-in convergence policy is a follow-up decision, not
  a default.
- **Cohort-level approval only (rings/groups), no per-Agent act.** WSUS and Intune approve per
  group, never per device. Selector rings (ADR-0016, ADR-0027) already give this fleet its
  cohorts, and "all matching" is the widest of them — but the requirement asks for the per-Agent
  view and act, and a cohort cannot express "this one canary host first".
- **Per-Agent display, but rollout still fleet-wide only.** Half the requirement, and the fleet
  view would show a difference the operator cannot act on at the granularity it is shown.

## Sources / Prior art

- **WSUS update approval** — content syncs to the server, nothing reaches clients until an
  explicit approval per computer group; approval binds a concrete update, and "All Computers"
  is the widest form of the same act.
  <https://learn.microsoft.com/en-us/windows-server/administration/windows-server-update-services/deploy/3-approve-and-deploy-updates-in-wsus>
- **Argo CD manual sync** — desired state is stored, drift is *displayed* as OutOfSync with a
  per-resource diff, and nothing is applied until an operator presses Sync; selective sync
  releases a chosen subset. The model for point 4.
  <https://argo-cd.readthedocs.io/en/stable/user-guide/auto_sync/>,
  <https://argo-cd.readthedocs.io/en/stable/user-guide/selective_sync/>
- **Bindplane rollouts** — edits create a new version, collectors keep the old one, deployment
  starts on an explicit *Start Rollout*, and rollback is rolling out a pinned historical version.
  The pinned-version-per-target shape is taken; the batched incremental walk is not taken (the
  fleet-fraction question stays with the Selector, ADR-0016/ADR-0027).
  <https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts>
- **Jamf Pro patch management** — new versions are *reported* against the fleet without any
  distribution; deployment requires an explicit policy binding one specific version to an
  explicit scope. <https://learn.jamf.com/r/en-US/jamf-pro-documentation-current/Patch_Policies>
- **Flux suspend / `kubectl rollout pause`** — the time-gated counter-model this ADR rejects in
  Alternatives: resume applies whatever is latest, so a save *does* eventually distribute
  without a further decision.
  <https://fluxcd.io/flux/components/kustomize/kustomizations/>,
  <https://kubernetes.io/docs/reference/kubectl/generated/kubectl_rollout/kubectl_rollout_pause/>
- **Grafana Fleet Management** — the weak-gate counter-model: an active/inactive toggle per
  pipeline, activation reaching all matching collectors at once, no per-target approval and no
  pinning.
  <https://grafana.com/docs/grafana-cloud/send-data/fleet-management/set-up/configuration-pipelines/>
- **OpAMP specification `v0.19.0`** — `PackagesAvailable` is per-Agent by definition, and the
  offered `remote_config` is the Server's composition; hash comparison is the protocol's own
  anti-re-distribution primitive, which point 3 keeps.
  <https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md>

## Consequences

- Positive: one lifecycle instead of two mechanisms. "Saved" is the only content state;
  "rolled out" is a fact about an Agent the operator can see and made true. Nothing distributes
  by side effect — not a save, not a Selector edit, not a label move, not an enrolment.
- Positive: the canary workflow needs no ring choreography — roll out to one Agent, watch it,
  then press "all matching".
- Positive: per-Agent assignment is the natural place for the audit trail ("when, to whom") —
  still blocked on *by whom* (API identity), but no longer on a data model.
- Negative / trade-offs: **every new Agent needs an operator's press before it runs anything.**
  Enrolling fifty hosts means fifty waiting rows (mitigated by the resource-level "all
  matching" press — but that press must be *repeated* after new Agents appear). If routine
  enrolment makes this a treadmill, an explicit opt-in convergence policy is the follow-up.
- Negative / trade-offs: the Agent record carries a persisted assignment (revisions retained
  while referenced), and the v1 API has rollout routes and no `publication` sub-resources. The
  fleet view, the editor, and the package view all carry the waiting/rolled-out state labels.
- Negative / trade-offs: per-Agent state can diverge across the fleet by design — ten Agents on
  three pinned revisions is a reachable, *intended* state, and the view must keep it legible.
- Follow-ups: an opt-in per-resource convergence policy for Agents that appear later; a
  batched/paused progressive walk over the matching set; the audit trail once the API has an
  identity; a diff view between an Agent's assigned revision and the saved one.
