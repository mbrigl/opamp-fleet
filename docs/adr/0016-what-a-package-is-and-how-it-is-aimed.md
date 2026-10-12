# ADR-0016: What a package is, and how the Server — not the host — aims it at Agents

- **Status:** 🟢 accepted
- **Date:** 2026-08-05
- **Deciders:** Markus Brigl

This decision amends without superseding: [ADR-0017](0017-package-artifacts.md) (the
Platform vocabulary, the alias table, and mandatory platform fit stand),
[ADR-0017](0017-package-artifacts.md) (the type is *half the
identity*, so a late-typing window and a mutable-attribute shape are unrepresentable),
[ADR-0034](0034-what-reaches-an-agent.md) (points 2 to 6 and 9 to 11 are untouched; only *which
key* `claimed_version` reads changes, and point 1's tie-break clause goes while fit by type and
platform stands), [ADR-0028](0028-a-rollout-is-an-explicit-act.md) (points 1, 3, 4 and 6 stand; 2,
5 and 8 change shape; 9 becomes moot), [ADR-0015](0015-package-delivery-for-managed-processes.md)
(where the signature an Agent verifies comes from), and
[ADR-0017](0017-package-artifacts.md) (a source entry keeps its URL, its mandatory
SHA-256 and its headers, and loses only its per-entry signature).

It cites [ADR-0023](0023-what-describes-an-agent.md) **unchanged** and makes it load-bearing: a label is
how an Agent enters a Deployment.

It decides two things: **the object** (the Package) and **the act** (the Deployment, which takes in
the name, the aim and the signature the Package gives up). A Package without a Deployment is
something nothing can aim; the dependency runs one way only.

## Context

### The object

Consider a package identified by the triple *(name, Agent type, version)* that carries, besides its
artifacts, a Selector, an `addon` flag, and one optional signature per entry. Four observations, all
from this repository:

**1. The name is a second identity for a thing that already has one.** The Agent type is compared
raw against the reported `service.name` and decides fit (ADR-0017). The name decides nothing — it
is the map key on the wire and the grouping key for resolution, and in every artifact this project
ships the two are already the same string. `fleet-packages/supervisor@0.4.3@supervisor/` is not a
coincidence: ADR-0022 fixed the Client's own Agent type at `supervisor` and named its package after
it, and `opamp-package-fetch` uploads under its `service_name` as both name and type without ever
having been told to. The model carries a degree of freedom nobody uses, and the one place it
*could* be used — two packages of one Agent type under different names — is precisely the ambiguity
`resolve` has to rank its way out of.

**2. The `addon` flag is a Server-side concept with no Client behind it.** The Baseline distinguishes
top-level packages from addons. The Client refuses every addon: an offer of addons only is reported
as an operator error ([`agent.rs`](../../crates/client/src/supervisor/agent.rs), the
`PackageType::Addon` filter). The flag therefore never changes what any host installs. It only
changes what the store must sort, and it takes a byte in the offer hash preimage for the trouble.

**3. Aim on the object makes "what is this artifact" and "who gets it" one editable record.** A
Selector that is mutable while the bytes are frozen is correct, and it is also the sign that two
different lifetimes are living in one document. Its consequence is a ranking: where several
packages match an Agent, the most specific Selector wins. That ranking is one an operator cannot
see — the answer to "which artifact does this host get" is not readable off any one object, it has
to be computed across all of them.

**4. Nothing is deployed.** The package store is empty; no fleet holds an installed record written
by this Server. Every compatibility argument that would normally shape a change of this size —
migrate the store, keep the hash preimage stable, alias the old routes — is void. That is not a
licence to be careless; it is a licence to *delete*, and AGENTS.md §1 asks for exactly that.

### The act

With the Package stripped to its bytes, three things have to land somewhere: the aim, the
signature, and the act that releases it. Putting them on a new object is not bookkeeping — it
answers a question a package-carried aim cannot.

**Which artifact does this host get?** With aim on the package, the answer is computed, not read:
gather the packages whose type and platform fit, keep those whose Selector matches, rank by
Selector specificity, break ties by version, and refuse an unbreakable tie. Each step is
defensible; the sum is a rule no operator can evaluate by looking at anything. Its failure mode is
`package_conflict`, and the shape it exists for — a fleet-wide package plus a narrower canary — is
exactly the shape that makes the ranking necessary in the first place.

**Where does the signature belong?** On the artifact record, supplied as a query parameter on the
upload, signing is a property of the *bytes*, which sounds right and is not: what an operator signs
off on is a release to a set of machines. It would also put a field back on the object that was just
emptied.

**What does a rollout name?** ADR-0028 gets the hard part right — nothing distributes without an
explicit act, and an act pins content per Agent. If the thing the act names is a package, releasing
"the software this channel runs" is *n* acts over *n* packages that nothing holds together.

**And the Selector cannot say "not".** Every Selector is equality over the effective description
([`configs::matches`](../../crates/server/src/configs.rs)); there is no negation and no set
difference. "Everyone except the canary hosts" is not a writable Selector. Any model that requires
disjoint targets must get disjointness from *membership*, not from exclusion — which is what
ADR-0023's labels already are.

## Decision

### The Package: what an Agent type runs at a version

We will make the **Package** the unit the store holds, identified by **(Agent type, version)** and
holding nothing but its entries — no name of its own, no Selector, no kind flag, no signature — and
the store carries no migration machinery, because there is no store left to migrate.

1. **Identity is the pair, stated at creation, never edited.** A Package is created as
   `(agent_type, version)`; both tokens follow a conservative token grammar (printable, no path
   separators, no `@`), so the pair embeds losslessly in a file name and a URL. A new version is a
   new Package. A Package has no free-form `name`; the ADR-0010 name grammar belongs to the
   Deployment's name (point 9), where a human-chosen label belongs.

2. **The name is derived, and there are two of them — they answer different questions.**
   - The **wire name** is the Agent type alone. It is the `PackagesAvailable` map key, the
     `PackageStatuses` key an Agent reports back, and the value `[self_update] package` is compared
     against. It must be *stable across versions*, which is why it cannot carry one.
   - The **display name** is `<agent_type> <version>` — the UI, the log line, the operator's ear.

   This is the sharpest consequence of point 1 and the one most easily got wrong:
   deriving one name and using it in both places would either break the Client's self-update guard
   on every release or make the fleet view unreadable.

3. **A Package holds entries and a derived hash, and nothing else.** A Package holds one
   entry per canonical `(os, arch)` pair (ADR-0017's vocabulary and alias table), so a duplicate is
   unrepresentable; each entry is **either** an uploaded artifact (the Server computes and stores
   its SHA-256) **or** a source reference (URL, mandatory SHA-256, optional headers — ADR-0017), and
   one entry suffices. The per-package hash the wire carries is `len(version) || version ||
   content_hash`, with **no kind byte**. The Platform has no place in it — two platforms' artifacts
   differ by content hash by construction. The hash is surfaced to the operator on the entry,
   because it is the exact value an Agent compares, and an operator who can read it can answer "did
   this host take my bytes" without trusting a status field.

4. **The Server has no `addon` flag.** Every Package is top-level;
   `PackageType::Addon` is never emitted. **The Client's refusal of addons stays exactly where it
   is** — it is not the other half of a flag but a defence against a non-conforming *peer*: the
   Baseline permits any Server to offer addons, this Client speaks to `opamp-go` in the conformance
   tests, and without the filter a foreign Server could write an addon over a Managed Process's
   binary. Only the Server's half is absent.

5. **The store holds Packages, one directory each, and knows no migration.** The layout
   is `<packages_dir>/<agent_type>@<version>/` holding `package.json` and one `<os>-<arch>.bin` per
   uploaded entry. There is no `migrate_legacy` and no `published` / `formerly_published` /
   `formerly_offered` seed for ADR-0028 point 9: they would serve stores that do not exist. A
   directory in a shape this version does not know **fails startup naming the path**, rather than
   being skipped; that is the one line standing between an empty store and a half-tidied
   development machine, and it follows ADR-0017 point 13's rule that an unreadable store is loud.

6. **The REST resource is the pair.** `/api/v1/packages/{agent_type}/{version}`, with
   the entry routes beneath it (`…/entries/{os}/{arch}` for bytes, `…/entries/{os}/{arch}/source`
   for a source). The create body has **no writable field** — so `{}` creates a Package. There is
   no `PUT …/selector` and no `POST …/rollout` on a Package: aim and release are not its business
   (points 9 and 16). The download route carries no name segment either.
   The OpenAPI document follows (ADR-0012). No route takes a package name, not even as an alias: an
   alias would need a `name` the model has no field for, and there is no deployed consumer to
   spare.

7. **What ADR-0034 decides is untouched.** A Package reaches an Agent only as an
   upgrade, the running `service.version` decides in both directions, and an unorderable claim
   refuses. The claimed version is read under the Agent type, because that is the key an Agent
   reports its status under.

### The Deployment: aim, signature, and rollout

We will have the **Deployment** — a named set of Packages, aimed by a Selector, carrying the
signature of each artifact — be the only thing that is rolled out, and rule that **an Agent belongs
to at most one**.

8. **A Supervisor says only whether it accepts updates, never which one.** Operators
   depend on this rule. `accepts_packages = true` means "this Managed Process's binary is updated
   from the Package the Server selects for me"; absent, the Supervisor takes no package offers. A
   `package = "…"` key in the Client's configuration fails at startup with a message naming its
   replacement — loudly, as ADR-0008 requires, never silently ignored. Choosing the artifact is the
   Server's job.

9. **A Deployment is name, Selector, packages, signatures.** The name follows the ADR-0010
   grammar (it is the one human-chosen label in the model). The Selector is ADR-0012's semantics
   unchanged — equality pairs over the effective description, so an operator learns one targeting
   mechanism, not two. It holds **one Package per Agent type**, and one signature per
   `(Package, Platform)`.

10. **At most one Package per Agent type, refused at the API.** A second Package of a type
   already present answers `409`. Two of one type would collide on the wire map key *and* fit the
   same Agent; refusing at write time turns a resolution-time mystery into an error at the moment
   of the mistake. Refusing ambiguity at the API fails when the collision is on the *aim*: every
   Selector starts empty, so the *first* write would always be refused and the store could never
   leave the state it started in. That dead end does not exist here, because the collision is on
   the **type**, which is identity, not on the aim.

11. **The Selector must not be empty.** An empty Selector is the channel that collides
   with every other, and a forgotten field would silently become the base for the whole fleet — the
   class of accident ADR-0028 exists to prevent. `400`, at creation and at every edit — the status
   this API already uses for a body it will not accept; it holds no other validation code.

12. **Channels are a label partition, and there is no "everyone" shortcut.** Because a
   Selector cannot express exclusion (Context), disjoint channels come from a key every Agent
   carries. **The Server prescribes none** — there is no reserved word and no special handling —
   and which one an operator picks says what the partition *means*: `channel` (`stable`, `beta`)
   for release risk, where the host subscribes once and the channel is handed one version after
   another; `region` for where it runs, where a release follows the sun or stays pinned to a
   jurisdiction; `tenant` for whose it is, where one customer moves on their own schedule. They
   compose — `{tenant, channel}` is two equality pairs and needs nothing new. What they share is
   why they work: each names a property of the **host**. A key naming the Deployment instead would
   put one fact in two places, so re-aiming a channel would mean editing every host in it. The
   value arrives from provisioning, through the `[attributes]` table the Client's configuration
   already documents, or as a Server label (ADR-0023) — which is how a host moves between channels
   without the machine being touched. An Agent carrying no channel belongs to no Deployment and
   waits; that is ADR-0028 point 6's rule, reached one step earlier, and the fleet view
   distinguishes it from "waiting for a rollout" because the operator's next move differs.

13. **An Agent belongs to at most one Deployment; any overlap is a conflict.** Not the
   most specific, not the newest — none. The Agent is reported under the existing
   `package_conflict` surface, naming both Deployments. No specificity ranking applies anywhere:
   Configurations never used one, and Deployments do not either.

14. **A conflict takes the candidate away, never a standing assignment.** An Agent
   already rolled out to keeps its offer — ADR-0028's rule that nothing distributes *or
   un-distributes* by itself binds in both directions, and creating an overlapping Deployment must
   not withdraw software from a running host. An Agent never rolled out to is offered nothing and
   the view says why. This is the sentence most easily got wrong in one line of code, and it is
   the one this decision most depends on.

15. **The signature lives here, per `(Package, Platform)`, and the wire is unchanged.**
   What an Agent receives in `DownloadableFile.signature` is the signature its Deployment holds for
   the artifact it is offered. The Client's verification policy is untouched (ADR-0015): with a
   verification key configured a signature is mandatory, without one a signed artifact is refused.
   The Server cannot refuse an unsigned Package — an unsigned fleet is a legitimate policy — so it
   **reports** the gap rather than hiding it, and the same Package in two Deployments must be
   signed in each.

16. **Both rollout acts name a Deployment.** `POST /api/v1/deployments/{name}/rollout`
   releases to every Agent it fits and aims at; `POST /api/v1/agents/{uid}/rollout` releases to
   one. Both pin content as of that press (ADR-0028 point 2), and an Agent's assignment is at most
   one pair — the Deployment it came from and the Package that was pinned. Naming a bare Package in
   the per-Agent act is deliberately **not** offered: it would bypass the Deployment that supplies
   the signature.

17. **The per-Agent act refuses to pick a side.** Under a conflict it answers `409` even
   though the operator named a Deployment. Otherwise the conflict is sidestepped for good instead
   of fixed, and the canary path becomes the way into a state the bulk path forbids.

18. **Immutability follows the assignment, and it freezes exactly what a standing offer
    travels with — no more.** A Package assigned to at least one Agent has immutable entries
    (ADR-0028 point 8). On a Deployment, two things join it: the **signature** of a Package that
    channel released, and the channel's **hold** on that Package. Both for one reason: what gates
    re-offering is the package hash, which covers the version and the content and **not** the
    signature. A signature changed under a standing offer would therefore never reach the Agent
    installing against the old one, and removing the Package — which takes its signatures with it —
    would silently turn a signed rollout unsigned for any Agent that has not finished, so a Client
    with a verification key would refuse an artifact it was already downloading for a reason
    nothing said out loud.

    **Everything else stays editable, and that is not a concession.** The Selector always — aim is
    not bytes. Adding a Package for an Agent type the channel does not hold: it changes no existing
    offer and surfaces as waiting. And, decisively, **swapping the version a channel holds**: the
    Agents already released keep their pinned Package and their offer, the new version shows as
    waiting, and the next press moves them. That *is* how a rollout proceeds.

19. **The bundled UI has a Deployments tab, and Packages has no aim.** The Packages tab
    is a master–detail view — one table of Packages, a detail form for the current record that
    hides while nothing is current, OK and Cancel concluding it on the right and Delete alone on
    its left — and shows no Selector, no kind and no reach; reach is a property of aim. The
    Deployments tab is the same shape with the **per-row rollout button** beside it: ADR-0028's
    seam, as ADR-0028 keeps it — the press that changes the fleet is never the press that saves. An
    Agent with no Deployment is shown calmly; it is the normal state of a freshly enrolled host, not
    a fault.

20. **Point 9 of ADR-0028 is moot and is not carried forward.** It read a pre-ADR store
    as rolled out so an upgrade would not empty the fleet's offers. There is no such store: the
    package store is empty and no Agent record holds an assignment written by this Server. The seed
    and its marker do not exist, and this is recorded so their absence does not read as an
    oversight.

## Alternatives considered

### The Package

- **Keep the name as an optional field defaulting to the Agent type.** Smallest diff, and it leaves
  room for two artifacts of one type under different names. Rejected: the room is the problem. It
  is the degree of freedom that makes `resolve` need a ranking, and an optional identity field is
  one every operator must decide about once and no operator wants to.
- **Identity `(name, version)`, with the type as an attribute.** Rejected: the type is what decides
  fit, and an attribute is editable. Re-typing published bytes to a different kind of Agent is
  exactly what immutable identity forecloses.
- **Identity `(name, Agent type, version)`, with a Selector on the package.** It keeps a second
  identity beside the type and puts aim on the object; both are what the Context above shows to
  produce a ranking no operator can read.
- **Keep the `addon` flag against a future need.** The Baseline has the concept and dropping it
  narrows what this Server can express. Rejected under §1 (YAGNI): no Client this project ships
  installs one, no artifact it builds is one, and the flag's only effect would be a byte in a hash
  and a branch in resolution. It can come back with the Client support that would give it meaning —
  named here as a follow-up topic, not a number.
- **Keep a kind byte in the hash preimage anyway, for stability.** With a deployed fleet this would
  be mandatory: dropping it changes every package hash, and the Client compares exactly that hash,
  so every host would re-download software it already runs. With an empty store it buys nothing and
  leaves an unexplained constant in a hash function for the next reader to fear. Rejected *because*
  the store is empty — and the reason is recorded so a future reader does not restore the byte by
  cargo cult.
- **Keep `migrate_legacy` and add a second stage.** The conservative reading, and wrong here: a
  migration with no source is untestable in the only way that matters and untrue as documentation.
  Deleting it is a decision, so it is written down rather than done quietly.
- **One opaque Package ID (a hash) instead of two path segments.** The ID would appear in URLs, file
  names and the UI while meaning nothing to a human, and a bounded token grammar buys the same
  safety while keeping `telegraf@1.30.0` readable in a directory listing.

### The Deployment

- **The empty Selector as a catch-all Deployment that loses to every other.** Restores the
  base-plus-canary shape with one bounded rule instead of general specificity, and guarantees no
  host is ever orphaned. Rejected on two counts. It is a ranking — one level deep, but the very
  specificity ranking this decision withdraws — and it makes *forgetting* a field the way to target
  the whole fleet, which runs against every gate this project has built. It also makes overlap
  structural: the base overlaps everything by construction, so a write-time check could never
  exist, which is precisely the dead end point 10 describes.
- **Inequality in Selectors (`key != value`).** The most expressive fix, and not far-fetched —
  Kubernetes label selectors have `NotIn`/`DoesNotExist`. Rejected under §1: the operator must keep
  both Selectors in sync by hand (a third channel means editing the base Deployment's exclusions),
  which is the maintenance burden membership models exist to remove; overlap stops being visible by
  eye; and it changes a Specification concept for Configurations too, for a need the partition
  already covers. Named as a follow-up topic if the partition proves too rigid.
- **Keep specificity, on the Deployment.** The smallest change, and it preserves the canonical
  fleet-wide-plus-canary shape. Rejected: it preserves the thing that makes the model unreadable.
  "Which Deployment does this host belong to" would still be a computation over all Deployments
  rather than a fact about one, and the requirement is a partition.
- **Naming a Deployment in the per-Agent act *is* the disambiguation** (point 17
  inverted). Tempting, and the operator has after all said which one they mean. Rejected: it makes
  the conflict permanently liveable, and a rule that can be waived per Agent is not a partition.
- **Freeze a released channel whole ("immutable membership").** Rejected: it forbids updating a
  fleet at all, because swapping the version a channel holds is how a rollout proceeds. The tests
  of point 18 catch exactly this.
- **A separate ADR for the one-Deployment-per-Agent rule.** Rejected: that rule *is* what makes a
  Deployment a partition rather than a label, and split out, this decision could not state what it
  decided.
- **Deployments carry Configurations too.** The tidier end state — one Selector, one act, one object
  per channel — and where this is likely to go. Deferred deliberately: it is a second lifecycle with
  its own revision model (ADR-0028/0061), and folding it in here would double a change that is
  already large. Named as a follow-up topic.
- **Signatures stay on the artifact; the Deployment only names the required key.** Smallest
  signature change and no re-signing when a Package moves. Rejected: it leaves an attribute on the
  Package, which holds only entries (point 3), and it puts the release decision back on
  the bytes.

## Sources / Prior art

- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"The PackagesAvailable message describes the packages that are available on the Server for this
  Agent"*, and *"There is normally only one top-level package, which implements the primary
  functionality of the Agent"*: the per-Agent offer (a name mapped to one offered version and hash
  *for this Agent*), which is both the constraint resolution satisfies and the licence for holding
  what is not offered.

  It also carries `signature` per downloadable file, so the Deployment changes only where the
  Server reads that field from. The stable wire name of point 2 is what the per-Agent
  map key has to be, and point 4 makes the single top-level package structural rather
  than enforced.

- **[OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md)** — a
  *reference* is `name:tag`, and the name is the software's identity, not a label beside it. One
  manifest list, one entry per `platform` object, each naming its digest.
- **[Debian repository format](https://wiki.debian.org/DebianRepository/Format)** /
  **[RPM](https://rpm-software-management.github.io/)** — a released package is
  `(name, version, architecture)`; the name *is* what the software is, and nothing carries a second
  identity beside it. Thirty years of the shape the Package converges on.
- **[WSUS update approval](https://learn.microsoft.com/en-us/windows-server/administration/windows-server-update-services/deploy/3-approve-and-deploy-updates-in-wsus)**
  — approval binds a concrete update to a **computer group**; membership, never exclusion. ADR-0028
  took the approval act from here; the Deployment takes the group.
- **[Jamf Pro patch policies](https://learn.jamf.com/r/en-US/jamf-pro-documentation-current/Patch_Policies)**
  — deployment requires an explicit policy binding one specific version to an explicit **scope**,
  and a scope is built from groups. Again membership.
- **[Bindplane rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts)**
  — the source ADR-0028 already reads: edits create a version, deployment starts on an
  explicit act, rollback is rolling out a pinned historical version. Point 16 keeps that
  shape with a Deployment as the thing named.
- **[Argo CD manual sync](https://argo-cd.readthedocs.io/en/stable/user-guide/auto_sync/)** — desired
  state stored, drift *displayed*, nothing applied until an operator presses Sync. The model for the
  waiting view, which points 12 and 14 extend to "belongs to no Deployment".
- **[Kubernetes label selectors](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/)**
  — the counter-model weighed in Alternatives: set-based selectors *do* have `NotIn` and
  `DoesNotExist`, and Kubernetes still expects membership labels for grouping. Read as evidence that
  negation is possible and that the ecosystem does not lean on it for cohorts.

- This repository: ADR-0010 (the name grammar, the Deployment's), ADR-0012 (Selector semantics,
  reused verbatim), ADR-0015 (delivery and verification, amended in where the signature comes
  from), ADR-0017 (source entries, kept, without a per-entry signature; the platform and type
  guarantees kept, structure amended), ADR-0023 (labels, unchanged and now load-bearing), ADR-0028
  (the act, amended), ADR-0022 (the Client's own Agent type is `supervisor`, which is why the
  derived name needs no exception), ADR-0034 (the version test, point 1's tie-break withdrawn).

## Consequences

- **Positive:** the answer to "what is this artifact" is readable off one object with two fields.
  Two Packages of one Agent type at one version are unrepresentable rather than a conflict to
  rank. `resolve` has no grouping pass, no specificity comparison and no addon partition, and the
  tree carries no migration and publication-seed machinery.
- **Positive:** there is no ranking — no specificity comparison, no version tie-break and no
  unbreakable-tie conflict. A channel is a thing an operator can name, look at, sign, and release
  in one act, and a Deployment's reach is a fact about the Deployment rather than a number computed
  against every other one. **Stated precisely, because it is easy to overclaim:** what this buys is
  not a structural guarantee that one Deployment matches — two Deployments may still both name
  `channel = "stable"` alongside a differing second key and collide. It is that such a collision is
  **refused and named** instead of silently resolved by a rule nobody can see. Loud beats ranked;
  the partition is a discipline on top, not a proof.
- **Negative / trade-offs:** two artifacts of one Agent type at one version — two Collector builds
  at `0.109.0`, say — cannot coexist; they must differ in type or in version, which is what their
  `service.name` already says about them. The absence of the kind flag narrows this Server against
  the Baseline. And an operator who typed the Client's `[self_update] package` as something other
  than an Agent type will see that Client refuse the offer, visibly, on its fleet row.
- **Negative / trade-offs:** **there is no "roll out to everyone".** A fleet-wide delivery requires
  every Agent to carry the same channel value, and a freshly enrolled host belongs to no Deployment
  until it is labelled. That is a real discipline; it belongs to provisioning rather than to the
  Server, and it is the price of a partition. The same Package in two Deployments must be signed in
  each, even though Ed25519 over the same bytes with the same key yields the same signature —
  duplication that buys a Package holding nothing but its entries.
- **Follow-ups (by topic, never by number):** whether the Client's `[self_update] package` key
  should be renamed now that its value is an Agent type; whether addons return once a Client can
  install one; a retention policy for superseded Packages, which stays open; whether a
  Configuration should be aimed by the Deployment that already aims its Agent's Package, instead of
  carrying a Selector of its own — the two-meanings-of-Selector question this change defers;
  inequality in Selectors, if the partition proves too rigid; refusing an overlapping Selector at
  write time rather than at resolution, once a fleet is large enough that a conflict is expensive
  to notice.

### The wording the specification needs

`docs/SPECIFICATION.md` defines **Package** at line 242, has no term for the Deployment, and defines
**Selector** (line 239) as serving a configuration alone. This decision does not change it
unilaterally — the specification outranks every ADR (AGENTS.md §3.4). The conflict is raised here,
with the wording this decision needs, to be accepted or amended together with the decision:

> - **Package** — a versioned, downloadable software artifact an Agent installs, identified by the
>   **Agent type it is built for and its version**; its display name is derived from the two. It is
>   verified against a content hash, and against a signature where one is configured — the signature
>   travelling with the Deployment that offers it rather than with the artifact record. The Server
>   offers Packages; an Agent reports the status of each. This is how the Server updates an agent's
>   software, not only its configuration.
>
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
