# ADR-0016: What a package is, and how the Server — not the host — aims it at Agents

- **Status:** 🟡 proposed
- **Date:** 2026-08-05
- **Deciders:** Markus Brigl

This decision supersedes the **structural** decisions of
[ADR-0017](0017-package-artifacts.md) point 6 (the `(name, os, arch)` store key, the
per-variant version, the per-variant history) and [ADR-0017](0017-package-artifacts.md)
points 14 and 16 (the type as a mutable attribute set after upload, and the untyped-inert state).
Everything those ADRs *guarantee* survives and is strengthened: the platform vocabulary,
canonicalisation, and mandatory platform fit (ADR-0017 points 8–12), the type fit and its raw
comparison (ADR-0017 points 15, 17, 18), file-or-source entries
([ADR-0017](0017-package-artifacts.md)), and the publication gate
([ADR-0028](0028-a-package-is-published-before-it-is-offered.md)) — whose one asymmetry this
decision removes.

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

### The missing object: the release

A package model grown by accretion shows its seams. A package as a *name* carries a Selector, an
Agent type, and a publication state; under the name sit per-platform variants, each with **its own
version** and its own one-step history. Four properties arrive over up to four requests, and three
consequences follow:

- **"Which version is this package at" has up to five answers.** With the version on the variant
  (ADR-0017), `linux-amd64` can hold `3.1.0` while `windows-amd64` still holds `3.0.0` — sometimes a
  rollout in progress, sometimes a forgotten upload, and the model cannot tell the two apart. A
  release is one version across five artifacts; such a store has no object that *is* that release.
- **A late type creates an untyped state that exists only to be inert.** ADR-0017 put the type on a
  sub-resource because the *artifact* upload was the wrong place for it — five uploads could
  disagree. The price is a window in which a package exists untyped, a state whose only meaning is
  "not finished", plus a rule (offered to nobody) to make that window safe. The window is an
  artifact of the request choreography, not of the domain.
- **Replacing bytes in place is the one path around the publication gate.** ADR-0028 stages new
  packages but lets an upload *replace* the artifact of a published package and distribute
  immediately — a deliberate asymmetry ("the ordinary in-place upgrade"), with the workaround
  "retract first" for anyone who wants staging. In-place replacement is also what forces a hidden
  history: because a new version *overwrites* the old one, the store must secretly remember what it
  destroyed, one step deep, per variant.

The requirement reorganises the model around the object that was missing — the release. A package
defines a **Set**. Each Set is identified by **name, Agent type, and version**, and may define a
Selector. A Set holds **one or more entries**, each identified by **os and arch**, with no two
entries for the same combination. An entry carries **either an uploaded file or a source with its
SHA-256**, and optionally a signature. **Saving a Set does not make it available to any Client** —
publication stays its own act.

The wire imposes one constraint and one freedom. `PackagesAvailable` maps a package *name* to one
offered version and hash per Agent — so whatever the store holds, offer resolution must reduce
"every Set named `otelcol`" to at most one per Agent. And the Baseline leaves the offered set
entirely to the Server ("the packages that are available on the Server **for this Agent**"), so
holding many versions and offering one is conformant, exactly as holding drafts is (ADR-0028).

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

4. **The Selector is set through its own sub-resource**, `PUT …/selector` beneath the
   Set's resource (point 10), with the same JSON shape a Configuration uses. A Set
   without a Selector behaves exactly as one with an empty Selector.

### A package is a versioned Set

We will make the **Set** the unit the package store holds, identified by **(name, Agent type,
version)**, immutable in its bytes while published, and offered to nobody until published.

5. **Identity is the triple, stated at creation, never edited.** A Set is created as one
   document — name, Agent type, version, and optionally a Selector — and the triple is its key:
   creating "the same Set again" addresses the same resource, and a new version is a **new Set**,
   never a mutation of an old one. There is no moment where a Set exists untyped, and no inert
   state to explain; ADR-0017's objection to stating the type at upload (it was per-artifact five
   times) does not apply, because here it is per-Set once. Name follows the ADR-0010 grammar;
   version and Agent type are bounded to a conservative token grammar (printable, no path
   separators, no `@`) so the triple embeds losslessly in file names and URLs. The type is compared
   **raw** against the reported `service.name` (ADR-0017 point 15).

6. **Entries are a map keyed by Platform, so a duplicate is unrepresentable.** A Set
   holds one entry per canonical `(os, arch)` pair (ADR-0017's vocabulary and alias table,
   unchanged). Writing an entry for a pair the Set already holds *replaces* that entry — while the
   Set is a draft. Each entry is **either** an uploaded artifact (the Server computes and stores its
   SHA-256) **or** a source reference (URL, mandatory SHA-256, optional headers — ADR-0017
   unchanged), and either way an optional Ed25519 signature. One entry suffices for a Set to be
   publishable; five platforms are five entries under one identity, which is what makes a release
   one object.

7. **A saved Set reaches nobody; publication is per Set; published bytes are
   immutable.** Every Set is created a draft, and — closing ADR-0028's asymmetry — there is no
   in-place upgrade to exempt: the "ordinary version bump" is a new Set, staged like everything
   else. Publishing a Set with no entries is refused (`409`): a Set *contains one or more entries*
   by definition, and the empty state exists only while an operator is still assembling one. While
   a Set is published its entries cannot be written or deleted (`409`; retract first): the fleet is
   installing those bytes, and a hash that changes under an offer is the confusion the publication
   gate exists to prevent. The **Selector stays editable in every state** — aim is not bytes, and
   moving a published Set between rings is precisely how a rollout proceeds (point
   3).

8. **Offer resolution: fit, aim, then version — at most one Set per name per Agent.**
   For one Agent, the candidates are the published Sets whose Agent type equals the reported
   `service.name`, holding an entry for the reported platform, whose Selector matches
   (fit-before-aim, ADR-0017/0031 and point 1, all unchanged). Among candidates
   *sharing a name*, the most specific Selector wins (point 3's rule, ranking Sets);
   among equally specific candidates the **greater version** wins, compared as ADR-0009 compares
   versions; a tie that version comparison cannot break (equal, or not versions at all) is a
   conflict — offer nothing under that name, and surface it (`package_conflict`), as ambiguous
   targeting is surfaced. The winner is offered under the Set's name with the Set's version and the
   fitting entry's hash — **nothing changes on the wire or in the Client**.

   Greater-version-wins is what makes rollouts and rollbacks publication moves: publish `3.1.0`
   beside a fleet-wide `3.0.0` with a canary Selector and the ring gets `3.1.0` while everyone
   else keeps `3.0.0`; widen the Selector to finish the rollout; retract `3.1.0` and the fleet
   falls back to `3.0.0` — one step back, except the step is any published version and the record
   of what "back" is, is the store itself.

9. **The store holds Sets, one directory each.** The filesystem layout is
   `<packages_dir>/<name>@<version>@<type>/` holding `set.json` (identity, Selector, publication
   state, and every entry's metadata) and one `<os>-<arch>.bin` per uploaded entry — the grammar
   of point 5 is what makes the directory name parse back unambiguously, as ADR-0017's
   `@` trick did for variants. There is no `.previous.bin` and no hidden history: earlier versions
   are kept openly, as Sets.

10. **The REST resource is the triple.** The package routes address
   `/api/v1/packages/{name}/{agent_type}/{version}`: `PUT` creates the Set (body: Selector,
   optional), entry routes beneath it write bytes (`PUT …/entries/{os}/{arch}`, body the artifact,
   optional `signature=<hex>`) or a source (`PUT …/entries/{os}/{arch}/source`, body as ADR-0017),
   `PUT …/selector` and `PUT …/publication` keep their shapes, `DELETE` removes an entry or the
   Set. `GET /api/v1/packages` lists Sets grouped by name, each with its entries, reach, and
   publication state. There is no `/type` and no `/rollback` route — the first is identity, the
   second is a publication move. The OpenAPI document follows, as it must (ADR-0012).

11. **The bundled UI supports the model from the start, as a master–detail view.** The
   Packages tab is **one table of Sets** — one row per Set, showing its identity (name, Agent type,
   version), publication state, the platforms it holds, and its reach. Selecting a row makes that
   Set the *current* record and shows it in a **detail form**; while nothing is current, the form
   is not visible. **Create** opens the form empty to define a new Set — the only moment the
   identity fields are writable (point 5); on an existing Set the form offers exactly
   what the API offers: the Selector always, the entries while the Set is a draft — each entry
   removable in place, and a new one addable in place through its own **Add** action, so
   assembling a five-platform release is five adds on one draft rather than five saves. **OK**
   persists the form's changes through the REST routes (including an entry still sitting in the
   editor) and **Cancel** discards them; both conclude the form — the current record is let go and
   the form leaves the screen, a failed save alone keeping it open beside its message. Delete
   stands alone on the form's left, Cancel and OK conclude it on the right; **Delete** removes the
   current Set, after which nothing is current and the form hides. One thing is deliberately *not*
   in the form at all: the publication state. Publishing and retracting are their own control, a
   per-row button in the table beside the Set's state, because the publication gate's seam — the press
   that releases a rollout is never the press that carries the bytes — is the reason the gate
   works, and a publication flag folded into "save" would be armed by the same click that edits a
   Selector. The form enforces no rule of its own: what it greys out is what the Server answers
   `409` to, one rule, rendered.

12. **An existing store is migrated at first open — loudly where it cannot be.** Each
   stored package becomes one Set per distinct variant version, carrying the entries that share
   that version and the package's Selector, type, and publication state; a remembered previous
   version (the one-step `previous` slot) becomes an **unpublished** Set of that version, so
   nothing an operator could roll back to is lost. A stored package with **no Agent type cannot
   become a Set** — the type is identity — and fails startup naming the file, ADR-0017 point 13's
   rule: it was inert before and would be unrepresentable now, and inventing a type would aim bytes
   this Server cannot judge.

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

- **Refuse an overlapping Selector at the API.** Rejected on contact with the code, which is the
  honest reason: with every existing package carrying an empty Selector, the first `PUT` is always
  refused, and there is no order in which two packages can be narrowed — the store cannot leave the
  state it starts in. Resolving at offer time has no such dead end, and it turns the common case
  (a default plus an override) from an error into the mechanism.
- **Target by Instance UID instead of a Selector.** Rejected. Naming Agents individually pins a
  rollout to identities that are reassignable (`AgentIdentification`) and says nothing about *why*
  those hosts were chosen. A Selector over reported attributes survives re-identification and is
  self-documenting — and it is already the project's vocabulary.

- **Keep the accreted model and only add the missing staging** (make in-place replacement stage
  too — ADR-0028's rejected stricter reading). Smallest change, and it fixes only one of the three
  seams: versions stay per-variant, the untyped window stays, and rollback stays a hidden single
  step. Rejected: the requirement is the reorganisation, and each seam traces to the same root —
  the store has no object for a release.
- **Version per variant** (ADR-0017's shape). It permits a per-platform rollout of different
  versions under one name without multiple Sets. Rejected: multiple Sets express that case
  explicitly (two Sets, two Selectors) — and the implicit form is indistinguishable from a
  half-finished upload, which is the ambiguity operators actually hit.
- **Identity without the Agent type** — `(name, version)`, type as an attribute. One fewer path
  segment, and two Sets differing only by type are odd. Rejected: the requirement names the triple;
  the type is as constitutive of "what is this artifact" as the version (ADR-0017's whole point),
  and an attribute is editable — retyping published bytes to a different kind of Agent is exactly
  the mistake immutable identity forecloses.
- **Refuse every same-specificity tie instead of greater-version-wins.** Simpler rule, no version
  ordering needed, fully explicit. Rejected, narrowly: it makes the natural end state of every
  rollout — old and new both published fleet-wide — a conflict that stops offers entirely, so
  operators must retract the old version at the precise moment they finish a rollout, and the
  rollback target vanishes with it. Greater-version-wins keeps the old version harmlessly
  published as the standing fallback; the tie rule still catches genuinely incomparable versions.
- **Mutable entries on published Sets** (in-place replacement). Rejected: it is the hole in
  ADR-0028's gate, restated; with cheap versioned Sets the ordinary upgrade has a first-class shape,
  and immutability is what lets an operator read a published Set's hash as a fact.
- **A Set may be published empty.** Rejected: the requirement says one or more entries, and an
  empty published Set is an offer of nothing that still occupies a name and a version.
- **Keep exactly one previous version per package** (a hidden `previous` slot and a `/rollback`
  route). It bounds disk at twice a package's size. Rejected: versions are first-class Sets, and a
  rollback is a publication move over any published version, not a hidden single step.
- **Automatic pruning of superseded Sets.** Keeping one version bounds disk; Sets do not. Rejected
  *here*: retention is the same broad question ADR-0026 deferred for Agents and ADR-0010 for version
  directories, and deciding it as a footnote would repeat the mistake those ADRs declined. Deleting a
  Set is one request; the follow-up names the policy question.
- **Encode the triple as one opaque Set ID** (a hash) instead of three path segments. Robust
  against any character in any field. Rejected: the ID would appear in URLs, file names, and the
  UI while meaning nothing to a human; a bounded token grammar (point 5) buys the same
  safety and keeps `otelcol@3.1.0@otelcol-contrib` readable in a directory listing.

## Sources / Prior art

- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"The PackagesAvailable message describes the packages that are available on the Server for this
  Agent"*, and *"There is normally only one top-level package, which implements the primary
  functionality of the Agent"*: the per-Agent offer (a name mapped to one offered version and hash
  *for this Agent*), which is both the constraint resolution satisfies and the licence for holding
  what is not offered.

- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — checked as the behavioural oracle: its
  example Server only logs `PackagesAvailable`/`ServerProvidedAllPackagesHash` and never offers a
  package, so there is no upstream targeting behaviour to copy. The model is ours to choose, within
  what the protocol already provides.

- **[OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md)** —
  already ADR-0017's model, and an even closer fit for Sets: one *reference* (name + tag ≈ name +
  version) resolves to a manifest list with one entry per required `platform` object, each naming
  its digest — a Set with entries, almost field for field. Registries also settle point
  7's rule: a pushed manifest's digest is immutable; moving consumers is done by moving
  references (tags/publication), not by rewriting bytes.
- **[Debian repository format](https://wiki.debian.org/DebianRepository/Format)** /
  **[RPM repositories](https://rpm-software-management.github.io/)** — a released package is
  `(name, version, architecture)` with per-file checksums and detached signatures, and a *suite*
  (stable/testing) decides availability separately from presence in the pool: presence is not
  publication, which is ADR-0028's gate and point 7 in thirty-year-old production form.
- **[BindPlane rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts)**
  — staged versions released by an explicit act, already ADR-0028's source; the versioned-Set
  shape extends the same separation from configurations to artifacts.

- [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md) — the Selector semantics
  this reuses verbatim (every pair must equal a reported attribute; an empty Selector matches all),
  so an operator learns one targeting mechanism, not two.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) — the delivery, verification, and
  health-gating this leaves untouched; only *who is offered what* is decided here.

- This repository: [ADR-0017](0017-package-artifacts.md) (file-or-source entries, kept),
  [ADR-0009](0009-version-baking-comparison-and-display.md) (the version
  comparison point 8 ranks by), [ADR-0017](0017-package-artifacts.md) (structure superseded,
  guarantees kept), [ADR-0028](0028-a-package-is-published-before-it-is-offered.md) (the gate,
  kept and closed).

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

- Positive: **a release is one object.** Five platforms' artifacts under one identity, staged
  together, published together, with one version — the ambiguity "rollout in progress or forgotten
  upload?" stops being representable.
- Positive: **the publication gate has no exception left.** Nothing an operator saves — new Set,
  new entry, replaced draft entry — reaches any Client until the publication act, and published
  bytes cannot change underneath the fleet.
- Positive: **rollback is visible and multi-step.** The store openly holds every version; falling
  back is retracting a Set, the target is any version still published, and the fleet view can show
  the whole ladder.
- Positive: no untyped state, no late-typing window, and the type can never disagree with itself
  across platforms.
- Positive: the UI ships the model in the same change, and its master–detail shape mirrors the
  store one to one — the table is the Set list, the form is one Set, and every constraint the form
  shows (frozen identity, entries locked while published) is the Server's own rule, not a second
  implementation of it.
- Negative / trade-offs: **the largest package-API break yet** — every route under
  `/api/v1/packages` changes shape, `/type` and `/rollback` disappear, and every script and the
  bundled UI must follow. Pre-1.0, and the alternative is carrying two models; but ADR-0017 called
  its four route changes the largest break since ADR-0012, and this is larger.
- Negative / trade-offs: **disk is unbounded by design.** One remembered version would cap a
  package at twice its size; Sets accumulate until deleted, which is the "artifact registry by
  accident" that cap was meant to prevent — accepted knowingly, because the registry role is now
  explicit and manual deletion is one request, with retention named as the follow-up.
- Negative / trade-offs: **greater-version-wins puts semantics on the version string.** Two
  equally aimed Sets are ordered by ADR-0009 comparison, so a fleet whose versions are not
  comparable (Foreign Agents numbering freely) falls to the conflict rule and must keep Selectors
  disjoint or publication exclusive. The rule is mechanical but it is one more thing the manual
  must state plainly.
- Negative / trade-offs: adding a platform to a published Set means retract → add → republish — a
  window in which the Set reaches nobody. Rare (a release is normally built whole), and the
  alternative (mutable published Sets) reopens the gate; but it is a real two-minute cost on a
  case in-place upload handles in one request.
- Negative / trade-offs: migration turns each package into as many Sets as it had distinct
  variant versions, and an untyped stored package **fails startup** until the file is removed —
  ADR-0017's medicine, applied once more, with the same operator cost.
- Follow-ups: a retention policy for superseded Sets (by topic, with the Agent-record and
  version-directory retention questions it joins); surfacing the version ladder per name in the
  bundled UI; and extending the ADR-0026 storage-port pattern to this store once that ADR's
  precedent stands.
