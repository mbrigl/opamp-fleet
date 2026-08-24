# ADR-0096: A Deployment aims Packages at a channel, signs them, and is the only thing rolled out — and an Agent belongs to at most one

- **Status:** 🟡 proposed
- **Date:** 2026-08-23
- **Deciders:** Markus Brigl

Supersedes [ADR-0017](0017-selector-targeted-packages.md) on acceptance. Point 1 (a Package carries
a Selector) moves to the Deployment; point 3 (most-specific-Selector-wins, and the ranking that
picks one top-level package out of several) is **withdrawn with no successor**; point 4 (the
Selector sub-resource on the package) is removed. **Point 2 stands untouched and is restated here
because operators depend on it**: a Supervisor says only *whether* it accepts updates, never which
one, and a `package = "…"` key in the Client's configuration fails at startup naming its
replacement. Choosing the artifact was, is, and remains the Server's job.

Amends without superseding: [ADR-0061](0061-a-rollout-is-an-explicit-act.md) (points 1, 3, 4 and 6
stand; 2, 5 and 8 change shape; 9 becomes moot),
[ADR-0015](0015-package-delivery-for-managed-processes.md) (where the signature an Agent verifies
comes from), [ADR-0018](0018-packages-imported-from-a-url.md) (a source entry keeps its URL, its
mandatory SHA-256 and its headers, and loses only its per-entry signature), and
[ADR-0083](0083-what-reaches-an-agent.md) point 1 (the tie-break clause goes; fit by type and
platform stands).

Cites [ADR-0042](0042-server-set-labels.md) **unchanged** and makes it load-bearing: a label is how
an Agent enters a Deployment. Cites [ADR-0043](0043-a-package-is-published-before-it-is-offered.md)
as history only — it is already superseded by ADR-0061 and is not re-retired here.

Takes in what [ADR-0095](0095-a-package-is-what-an-agent-type-runs-at-a-version.md) gives up.

## Context

ADR-0095 strips the Package to its bytes. Three things it carried have to land somewhere: the aim,
the signature, and the act that releases it. Putting them on a new object is not bookkeeping — it
answers a question the old model could not.

**Which artifact does this host get?** Today the answer is computed, not read: gather the Sets whose
type and platform fit, keep those whose Selector matches, rank by Selector specificity, break ties
by version, and refuse an unbreakable tie (ADR-0017 point 3, ADR-0052 point 4). Each step is
defensible; the sum is a rule no operator can evaluate by looking at anything. The failure mode is
recorded in this repository as `package_conflict`, and the shape it exists for — a fleet-wide Set
plus a narrower canary — is exactly the shape that made the ranking necessary in the first place.

**Where does the signature belong?** On the artifact record, today, supplied as a query parameter on
the upload. That makes signing a property of the *bytes*, which sounds right and is not: what an
operator signs off on is a release to a set of machines. It also puts a field back on the object
ADR-0095 just emptied.

**What does a rollout name?** ADR-0061 got the hard part right — nothing distributes without an
explicit act, and an act pins content per Agent — but the thing the act names is a Set, so
releasing "the software this channel runs" is *n* acts over *n* Sets that nothing holds together.

**And the Selector cannot say "not".** Every Selector is equality over the effective description
([`configs::matches`](../../crates/server/src/configs.rs)); there is no negation and no set
difference. "Everyone except the canary hosts" is not a writable Selector. Any model that requires
disjoint targets must get disjointness from *membership*, not from exclusion — which is what
ADR-0042's labels already are.

## Decision

We will introduce the **Deployment** — a named set of Packages, aimed by a Selector, carrying the
signature of each artifact — make it the only thing that is rolled out, and rule that **an Agent
belongs to at most one**.

1. **A Deployment is name, Selector, packages, signatures.** The name follows the ADR-0010 grammar
   (it is the one human-chosen label in the model, and ADR-0095 freed the grammar for it). The
   Selector is ADR-0012's semantics unchanged — equality pairs over the effective description, so
   an operator learns one targeting mechanism, not two. It holds **one Package per Agent type**, and
   one signature per `(Package, Platform)`.

2. **At most one Package per Agent type, refused at the API.** A second Package of a type already
   present answers `409`. Two of one type would collide on the wire map key *and* fit the same
   Agent; refusing at write time turns a resolution-time mystery into an error at the moment of the
   mistake. ADR-0017 rejected exactly this shape — refuse the ambiguity at the API — and was right
   to: every package started with an empty Selector, so the *first* write would always have been
   refused and the store could never leave the state it started in. That dead end does not exist
   here, because the collision is on the **type**, which is identity, not on the aim.

3. **The Selector must not be empty.** An empty Selector is the channel that collides with every other,
   and a forgotten field would silently become the base for the whole fleet — the class of accident
   ADR-0061 exists to prevent. `400`, at creation and at every edit — the status this API already
   uses for a body it will not accept; it holds no other validation code.

4. **Channels are a label partition, and there is no "everyone" shortcut.** Because a Selector cannot
   express exclusion (Context), disjoint channels come from a key every Agent carries. **The Server
   prescribes none** — there is no reserved word and no special handling — and which one an operator
   picks says what the partition *means*: `channel` (`stable`, `beta`) for release risk, where the
   host subscribes once and the channel is handed one version after another; `region` for where it
   runs, where a release follows the sun or stays pinned to a jurisdiction; `tenant` for whose it
   is, where one customer moves on their own schedule. They compose — `{tenant, channel}` is two
   equality pairs and needs nothing new. What they share is why they work: each names a property of
   the **host**. A key naming the Deployment instead would put one fact in two places, so re-aiming
   a channel would mean editing every host in it. The value arrives from provisioning, through the
   `[attributes]` table the Client's configuration already documents, or as a Server label
   (ADR-0042) — which is how a host moves between channels without the machine being touched. An Agent
   carrying no channel belongs to no Deployment and waits; that is ADR-0061 point 6's rule, reached one
   step earlier, and the fleet view distinguishes it from "waiting for a rollout" because the
   operator's next move differs.

5. **An Agent belongs to at most one Deployment; any overlap is a conflict.** Not the most specific,
   not the newest — none. The Agent is reported under the existing `package_conflict` surface,
   naming both Deployments. Specificity does not survive anywhere: Configurations never used it, so
   it was a package-only rule and it dies with the ranking it existed to perform.

6. **A conflict takes the candidate away, never a standing assignment.** An Agent already rolled out
   to keeps its offer — ADR-0061's rule that nothing distributes *or un-distributes* by itself binds
   in both directions, and creating an overlapping Deployment must not withdraw software from a
   running host. An Agent never rolled out to is offered nothing and the view says why. This is the
   sentence most easily got wrong in one line of code, and it is the one this decision most depends
   on.

7. **The signature lives here, per `(Package, Platform)`, and the wire is unchanged.** What an Agent
   receives in `DownloadableFile.signature` is the signature its Deployment holds for the artifact
   it is offered. The Client's verification policy is untouched (ADR-0015): with a verification key
   configured a signature is mandatory, without one a signed artifact is refused. The Server cannot
   refuse an unsigned Package — an unsigned fleet is a legitimate policy — so it **reports** the gap
   rather than hiding it, and the same Package in two Deployments must be signed in each.

8. **Both rollout acts survive and name a Deployment.** `POST /api/v1/deployments/{name}/rollout`
   releases to every Agent it fits and aims at; `POST /api/v1/agents/{uid}/rollout` releases to one.
   Both pin content as of that press (ADR-0061 point 2), and an Agent's assignment is now at most
   one pair — the Deployment it came from and the Package that was pinned. Naming a bare Package in
   the per-Agent act is deliberately **not** offered: it would bypass the Deployment that supplies
   the signature.

9. **The per-Agent act refuses to pick a side.** Under a conflict it answers `409` even though the
   operator named a Deployment. Otherwise the conflict is sidestepped for good instead of fixed, and
   the canary path becomes the way into a state the bulk path forbids.

10. **Immutability follows the assignment, and it freezes exactly what a standing offer travels
    with — no more.** A Package assigned to at least one Agent has immutable entries (ADR-0061
    point 8). On a Deployment, two things join it: the **signature** of a Package that channel
    released, and the channel's **hold** on that Package. Both for one reason: what gates re-offering
    is the package hash, which covers the version and the content and **not** the signature. A
    signature changed under a standing offer would therefore never reach the Agent installing
    against the old one, and removing the Package — which takes its signatures with it — would
    silently turn a signed rollout unsigned for any Agent that has not finished, so a Client with a
    verification key would refuse an artifact it was already downloading for a reason nothing said
    out loud.

    **Everything else stays editable, and that is not a concession.** The Selector always —
    ADR-0052 point 3's reasoning survives its own ADR: aim is not bytes. Adding a Package for an
    Agent type the channel does not hold: it changes no existing offer and surfaces as waiting. And,
    decisively, **swapping the version a channel holds**: the Agents already released keep their
    pinned Package and their offer, the new version shows as waiting, and the next press moves
    them. That *is* how a rollout proceeds. A rule that froze the channel whole would forbid updating
    a fleet at all — the first draft of this point said "immutable membership" and did exactly
    that, which its own tests caught.

11. **The bundled UI gets a Deployments tab, and Packages loses its aim.** The Packages tab keeps
    ADR-0052 point 7's master–detail shape and drops the Selector, the kind and the reach — reach is
    a property of aim. The Deployments tab is the same shape with the **per-row rollout button**
    beside it: ADR-0043's seam, kept verbatim through ADR-0061 and kept again here — the press that
    changes the fleet is never the press that saves. An Agent with no Deployment is shown calmly; it
    is the normal state of a freshly enrolled host, not a fault.

12. **Point 9 of ADR-0061 becomes moot and is not carried forward.** It read a pre-ADR store as
    rolled out so an upgrade would not empty the fleet's offers. There is no such store: the package
    store is empty and no Agent record holds an assignment written by this Server. The seed and its
    marker are deleted rather than translated, and this is recorded so the deletion does not read as
    an oversight.

## Alternatives considered

- **The empty Selector as a catch-all Deployment that loses to every other.** Restores the
  base-plus-canary shape with one bounded rule instead of general specificity, and guarantees no
  host is ever orphaned. Rejected on two counts. It is a ranking — one level deep, but the very part
  of ADR-0017 this ADR withdraws — and it makes *forgetting* a field the way to target the whole
  fleet, which runs against every gate this project has built. It also makes overlap structural: the
  base overlaps everything by construction, so a write-time check could never exist, which is
  precisely the dead end ADR-0017 recorded.
- **Inequality in Selectors (`key != value`).** The most expressive fix, and not far-fetched —
  Kubernetes label selectors have `NotIn`/`DoesNotExist`. Rejected under §1: the operator must keep
  both Selectors in sync by hand (a third channel means editing the base Deployment's exclusions),
  which is the maintenance burden membership models exist to remove; overlap stops being visible by
  eye; and it changes a Specification concept for Configurations too, for a need the partition
  already covers. Named as a follow-up topic if the partition proves too rigid.
- **Keep specificity, just move it to the Deployment.** The smallest change, and it preserves the
  canonical fleet-wide-plus-canary shape. Rejected: it preserves the thing that made the model
  unreadable. "Which Deployment does this host belong to" would still be a computation over all
  Deployments rather than a fact about one, and the requirement is a partition.
- **Naming a Deployment in the per-Agent act *is* the disambiguation** (point 9 inverted). Tempting,
  and the operator has after all said which one they mean. Rejected: it makes the conflict
  permanently liveable, and a rule that can be waived per Agent is not a partition.
- **A separate ADR for the one-Deployment-per-Agent rule.** Rejected: that rule *is* what makes a
  Deployment a partition rather than a label, and split out, this ADR could not state what it
  decided.
- **Deployments carry Configurations too.** The tidier end state — one Selector, one act, one object
  per channel — and where this is likely to go. Deferred deliberately: it is a second lifecycle with
  its own revision model (ADR-0055/0061), and folding it in here would double a change that is
  already large. Named as a follow-up topic.
- **Signatures stay on the artifact; the Deployment only names the required key.** Smallest
  signature change and no re-signing when a Package moves. Rejected: it leaves an attribute on the
  object ADR-0095 empties, and it puts the release decision back on the bytes.

## Sources / Prior art

- **[WSUS update approval](https://learn.microsoft.com/en-us/windows-server/administration/windows-server-update-services/deploy/3-approve-and-deploy-updates-in-wsus)**
  — approval binds a concrete update to a **computer group**; membership, never exclusion. ADR-0061
  took the approval act from here; this ADR takes the group.
- **[Jamf Pro patch policies](https://learn.jamf.com/r/en-US/jamf-pro-documentation-current/Patch_Policies)**
  — deployment requires an explicit policy binding one specific version to an explicit **scope**,
  and a scope is built from groups. Again membership.
- **[Bindplane rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts)**
  — the source ADR-0043 and ADR-0061 already read: edits create a version, deployment starts on an
  explicit act, rollback is rolling out a pinned historical version. Point 8 keeps that shape with a
  Deployment as the thing named.
- **[Argo CD manual sync](https://argo-cd.readthedocs.io/en/stable/user-guide/auto_sync/)** — desired
  state stored, drift *displayed*, nothing applied until an operator presses Sync. The model for the
  waiting view, which points 4 and 6 extend to "belongs to no Deployment".
- **[Kubernetes label selectors](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/)**
  — the counter-model weighed in Alternatives: set-based selectors *do* have `NotIn` and
  `DoesNotExist`, and Kubernetes still expects membership labels for grouping. Read as evidence that
  negation is possible and that the ecosystem does not lean on it for cohorts.
- **[OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)**
  — `PackagesAvailable` is per-Agent by definition and carries `signature` per downloadable file, so
  point 7 changes only where the Server reads that field from.
- This repository: ADR-0012 (Selector semantics, reused verbatim), ADR-0015 (delivery and
  verification, amended in one respect), ADR-0017 (superseded; its point 2 restated as surviving),
  ADR-0018 (source entries, amended), ADR-0042 (labels, unchanged and now load-bearing), ADR-0061
  (the act, amended), ADR-0083 (the version test, point 1's tie-break withdrawn), ADR-0095 (the
  object this one aims).

## Consequences

- **Positive:** the ranking disappears — with it the specificity comparison, the version tie-break
  and the unbreakable-tie conflict. A channel is a thing an operator can name, look at, sign, and
  release in one act, and a Deployment's reach is a fact about the Deployment rather than a number
  computed against every other one. **Stated precisely, because it is easy to overclaim:** what this
  buys is not a structural guarantee that one Deployment matches — two Deployments may still both
  name `channel = "stable"` alongside a differing second key and collide. It is that such a collision is
  **refused and named** instead of silently resolved by a rule nobody can see. Loud beats ranked; the
  partition is a discipline on top, not a proof.
- **Negative / trade-offs:** **there is no "roll out to everyone" any more.** A fleet-wide delivery
  requires every Agent to carry the same channel value, and a freshly enrolled host belongs to no
  Deployment until it is labelled. That is a real discipline nobody needed before; it belongs to
  provisioning rather than to the Server, and it is the price of a partition. The same Package in
  two Deployments must be signed in each, even though Ed25519 over the same bytes with the same key
  yields the same signature — duplication that buys the empty Package of ADR-0095.
- **Follow-ups (by topic, never by number):** whether a Configuration should be aimed by the
  Deployment that already aims its Agent's Package, instead of carrying a Selector of its own —
  the two-meanings-of-Selector question this change defers; inequality in Selectors, if the
  partition proves too rigid; refusing an overlapping Selector at write time rather than at
  resolution, once a fleet is large enough that a conflict is expensive to notice.

### The wording the specification needs

`docs/SPECIFICATION.md` has no term for this object and defines **Selector** (line 239) as serving a
configuration alone. As in ADR-0095, the conflict is raised rather than resolved by hand:

> - **Selector** — the rule by which the Server addresses a **subset** of the fleet for a
>   Configuration or a Deployment, so a change reaches the matching Agents and leaves the rest
>   running what they already run. One mechanism with two subjects, not two mechanisms.
>
> - **Deployment** — a named set of Packages, aimed at a subset of the Fleet by a Selector and
>   carrying the signature of each Package's artifact. It is the only thing that is rolled out. An
>   Agent belongs to **at most one**: two Deployments matching one Agent is a conflict, and that
>   Agent is offered nothing new until it is resolved.

**Fleet** (line 159) is deliberately left as it stands — all Agents managed by the Server. That the
word was already taken is why this object is called a Deployment.
