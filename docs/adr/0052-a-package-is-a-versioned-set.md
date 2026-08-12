# ADR-0052: A package is a versioned Set — identified by name, Agent type, and version; one entry per platform; saved is not offered

- **Status:** 🟡 proposed
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

Supersedes [ADR-0019](0019-one-step-back.md) (the hidden one-step history — versions are now
first-class, and rollback is a publication move) and the **structural** decisions of
[ADR-0031](0031-per-platform-package-variants.md) point 1 (the `(name, os, arch)` store key, the
per-variant version, the per-variant history) and [ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md)
points 1 and 3 (the type as a mutable attribute set after upload, and the untyped-inert state).
Everything those ADRs *guarantee* survives and is strengthened: the platform vocabulary,
canonicalisation, and mandatory platform fit (ADR-0031 points 3–7), the type fit and its raw
comparison (ADR-0034 points 2, 4, 5), Selector aiming and specificity ([ADR-0017](0017-selector-targeted-packages.md)),
file-or-source entries ([ADR-0018](0018-packages-imported-from-a-url.md)), and the
publication gate ([ADR-0043](0043-a-package-is-published-before-it-is-offered.md)) — whose one
asymmetry this decision removes.

## Context

The package model grew by accretion, and its seams now show. A package is a *name*; the name
carries a Selector, an Agent type, and a publication state; under the name sit per-platform
variants, each with **its own version** and its own one-step history. Four properties arrive over
up to four requests, and three consequences follow:

- **"Which version is this package at" has up to five answers.** The version belongs to the
  variant (ADR-0031), so `linux-amd64` can hold `3.1.0` while `windows-amd64` still holds `3.0.0`
  — sometimes a rollout in progress, sometimes a forgotten upload, and the model cannot tell the
  two apart. A release is one version across five artifacts; the store has no object that *is*
  that release.
- **The type arrives late, and the untyped state exists only to be inert.** ADR-0034 put the type
  on a sub-resource because the *artifact* upload was the wrong place for it — five uploads could
  disagree. The price is a window in which a package exists untyped, a state whose only meaning is
  "not finished", plus a rule (offered to nobody) to make that window safe. The window is an
  artifact of the request choreography, not of the domain.
- **Replacing bytes in place is the one path around the publication gate.** ADR-0043 stages new
  packages but lets an upload *replace* the artifact of a published package and distribute
  immediately — a deliberate asymmetry ("the ordinary in-place upgrade"), with the workaround
  "retract first" for anyone who wants staging. In-place replacement is also what forces ADR-0019
  to exist: because a new version *overwrites* the old one, the store must secretly remember what
  it destroyed, one step deep, per variant.

The requirement this ADR answers reorganises the model around the object that was missing — the
release. A package defines a **Set**. Each Set is identified by **name, Agent type, and version**,
and may define a Selector. A Set holds **one or more entries**, each identified by **os and arch**,
with no two entries for the same combination. An entry carries **either an uploaded file or a
source with its SHA-256**, and optionally a signature. **Saving a Set does not make it available
to any Client** — publication stays its own act.

The wire imposes one constraint and one freedom. `PackagesAvailable` maps a package *name* to one
offered version and hash per Agent — so whatever the store holds, offer resolution must reduce
"every Set named `otelcol`" to at most one per Agent. And the Baseline leaves the offered set
entirely to the Server ("the packages that are available on the Server **for this Agent**"), so
holding many versions and offering one is conformant, exactly as holding drafts is (ADR-0043).

## Decision

We will make the **Set** the unit the package store holds, identified by **(name, Agent type,
version)**, immutable in its bytes while published, and offered to nobody until published.

1. **Identity is the triple, stated at creation, never edited.** A Set is created as one document —
   name, Agent type, version, and optionally a Selector — and the triple is its key: creating
   "the same Set again" addresses the same resource, and a new version is a **new Set**, never a
   mutation of an old one. This dissolves ADR-0034's late-typing window (there is no moment where
   a Set exists untyped, and no inert state to explain) and ADR-0034's objection to stating the
   type at upload (it was per-artifact five times; here it is per-Set once). Name follows the
   ADR-0010 grammar as today; version and Agent type are bounded to a conservative token grammar
   (printable, no path separators, no `@`) so the triple embeds losslessly in file names and URLs.
   The type is still compared **raw** against the reported `service.name` (ADR-0034 point 2).

2. **Entries are a map keyed by Platform, so a duplicate is unrepresentable.** A Set holds one
   entry per canonical `(os, arch)` pair (ADR-0031's vocabulary and alias table, unchanged).
   Writing an entry for a pair the Set already holds *replaces* that entry — while the Set is a
   draft. Each entry is **either** an uploaded artifact (the Server computes and stores its
   SHA-256) **or** a source reference (URL, mandatory SHA-256, optional headers — ADR-0018
   unchanged), and either way an optional Ed25519 signature. One entry suffices for a Set to be
   publishable; five platforms are five entries under one identity, which is what makes a release
   one object.

3. **A saved Set reaches nobody; publication is per Set; published bytes are immutable.** Every
   Set is created a draft, and — closing ADR-0043's asymmetry — there is no in-place upgrade to
   exempt: the "ordinary version bump" is a new Set, staged like everything else. Publishing a Set
   with no entries is refused (`409`): a Set *contains one or more entries* by definition, and the
   empty state exists only while an operator is still assembling one. While a Set is published its
   entries cannot be written or deleted (`409`; retract first): the fleet is installing those
   bytes, and a hash that changes under an offer is the confusion the publication gate exists to
   prevent. The **Selector stays editable in every state** — aim is not bytes, and moving a
   published Set between rings is precisely how a rollout proceeds (ADR-0017).

4. **Offer resolution: fit, aim, then version — at most one Set per name per Agent.**
   For one Agent, the candidates are the published Sets whose Agent type equals the reported
   `service.name`, holding an entry for the reported platform, whose Selector matches
   (fit-before-aim, ADR-0034/0031/0017, all unchanged). Among candidates *sharing a name*, the
   most specific Selector wins (ADR-0017's rule, now ranking Sets instead of packages); among
   equally specific candidates the **greater version** wins, compared as ADR-0029 compares
   versions; a tie that version comparison cannot break (equal, or not versions at all) is a
   conflict — offer nothing under that name, and surface it (`package_conflict`), as ambiguous
   targeting is surfaced today. The winner is offered under the Set's name with the Set's version
   and the fitting entry's hash — **nothing changes on the wire or in the Client**.

   Greater-version-wins is what makes rollouts and rollbacks publication moves: publish `3.1.0`
   beside a fleet-wide `3.0.0` with a canary Selector and the ring gets `3.1.0` while everyone
   else keeps `3.0.0`; widen the Selector to finish the rollout; retract `3.1.0` and the fleet
   falls back to `3.0.0` — ADR-0019's "one step back", except the step is any published version
   and the record of what "back" is, is the store itself.

5. **The store holds Sets, one directory each.** The filesystem layout becomes
   `<packages_dir>/<name>@<version>@<type>/` holding `set.json` (identity, Selector, publication
   state, and every entry's metadata) and one `<os>-<arch>.bin` per uploaded entry — the grammar
   of point 1 is what makes the directory name parse back unambiguously, as ADR-0031's `@` trick
   did for variants. There is no `.previous.bin` and no hidden history: what ADR-0019 kept
   secretly, this store keeps openly, as Sets.

6. **The REST resource is the triple.** The package routes address
   `/api/v1/packages/{name}/{agent_type}/{version}`: `PUT` creates the Set (body: Selector,
   optional), entry routes beneath it write bytes (`PUT …/entries/{os}/{arch}`, body the artifact,
   optional `signature=<hex>`) or a source (`PUT …/entries/{os}/{arch}/source`, body as ADR-0018),
   `PUT …/selector` and `PUT …/publication` keep their shapes, `DELETE` removes an entry or the
   Set. `GET /api/v1/packages` lists Sets grouped by name, each with its entries, reach, and
   publication state. `/type` and `/rollback` disappear — the first is identity, the second is a
   publication move. The OpenAPI document follows, as it must (ADR-0012).

7. **The bundled UI supports the model from the start, as a master–detail view.** The Packages tab
   is **one table of Sets** — one row per Set, showing its identity (name, Agent type, version),
   publication state, the platforms it holds, and its reach. Selecting a row makes that Set the
   *current* record and shows it in a **detail form**; while nothing is current, the form is not
   visible. **Create** opens the form empty to define a new Set — the only moment the identity
   fields are writable (point 1); on an existing Set the form offers exactly what the API offers:
   the Selector always, the entries while the Set is a draft — each entry removable in place, and
   a new one addable in place through its own **Add** action, so assembling a five-platform
   release is five adds on one draft rather than five saves. **OK** persists the form's changes
   through the REST routes (including an entry still sitting in the editor) and **Cancel**
   discards them; both conclude the form — the current record is let go and the form leaves the
   screen, a failed save alone keeping it open beside its message. Delete stands alone on the
   form's left, Cancel and OK conclude it on the right; **Delete** removes the current Set, after
   which nothing is current and the form hides. One thing is deliberately *not* in the form at all: the publication state.
   Publishing and retracting are their own control, a per-row button in the table beside the
   Set's state, because ADR-0043 point 7's seam — the press that releases a rollout is never the
   press that carries the bytes — is the reason the gate works, and a publication flag folded
   into "save" would be armed by the same click that edits a Selector.
   The form enforces no rule of its own: what it greys out is what the Server answers `409` to,
   one rule, rendered.

8. **An existing store is migrated at first open — loudly where it cannot be.** Each stored
   package becomes one Set per distinct variant version, carrying the entries that share that
   version and the package's Selector, type, and publication state; a remembered previous version
   (ADR-0019) becomes an **unpublished** Set of that version, so nothing an operator could roll
   back to is lost. A stored package with **no Agent type cannot become a Set** — the type is
   identity — and fails startup naming the file, ADR-0031 point 8's rule: it was inert before and
   would be unrepresentable now, and inventing a type would aim bytes this Server cannot judge.

## Alternatives considered

- **Keep the accreted model and only add the missing staging** (make in-place replacement stage
  too — ADR-0043's rejected stricter reading). Smallest change, and it fixes only one of the three
  seams: versions stay per-variant, the untyped window stays, and rollback stays a hidden single
  step. Rejected: the requirement is the reorganisation, and each seam traces to the same root —
  the store has no object for a release.
- **Version per variant, as today** (ADR-0031's shape). It permits a per-platform rollout of
  different versions under one name without multiple Sets. Rejected: multiple Sets express that
  case explicitly (two Sets, two Selectors) — and the implicit form is indistinguishable from a
  half-finished upload, which is the ambiguity operators actually hit.
- **Identity without the Agent type** — `(name, version)`, type as an attribute. One fewer path
  segment, and two Sets differing only by type are odd. Rejected: the requirement names the triple;
  the type is as constitutive of "what is this artifact" as the version (ADR-0034's whole point),
  and an attribute is editable — retyping published bytes to a different kind of Agent is exactly
  the mistake immutable identity forecloses.
- **Refuse every same-specificity tie instead of greater-version-wins.** Simpler rule, no version
  ordering needed, fully explicit. Rejected, narrowly: it makes the natural end state of every
  rollout — old and new both published fleet-wide — a conflict that stops offers entirely, so
  operators must retract the old version at the precise moment they finish a rollout, and the
  rollback target vanishes with it. Greater-version-wins keeps the old version harmlessly
  published as the standing fallback; the tie rule still catches genuinely incomparable versions.
- **Mutable entries on published Sets** (today's in-place replacement, kept). Rejected: it is the
  hole in ADR-0043's gate, restated; with cheap versioned Sets the ordinary upgrade has a
  first-class shape, and immutability is what lets an operator read a published Set's hash as a
  fact.
- **A Set may be published empty.** Rejected: the requirement says one or more entries, and an
  empty published Set is an offer of nothing that still occupies a name and a version.
- **Automatic pruning of superseded Sets.** ADR-0019 bounded disk by keeping one version; Sets
  unbounded it. Rejected *here*: retention is the same broad question ADR-0039 deferred for Agents
  and ADR-0010 for version directories, and deciding it as a footnote would repeat the mistake
  those ADRs declined. Deleting a Set is one request; the follow-up names the policy question.
- **Encode the triple as one opaque Set ID** (a hash) instead of three path segments. Robust
  against any character in any field. Rejected: the ID would appear in URLs, file names, and the
  UI while meaning nothing to a human; a bounded token grammar (point 1) buys the same safety and
  keeps `otelcol@3.1.0@otelcol-contrib` readable in a directory listing.

## Sources / Prior art

- **[OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md)** —
  already ADR-0031's model, and an even closer fit now: one *reference* (name + tag ≈ name +
  version) resolves to a manifest list with one entry per required `platform` object, each naming
  its digest — a Set with entries, almost field for field. Registries also settle point 3's rule:
  a pushed manifest's digest is immutable; moving consumers is done by moving references
  (tags/publication), not by rewriting bytes.
- **[Debian repository format](https://wiki.debian.org/DebianRepository/Format)** /
  **[RPM repositories](https://rpm-software-management.github.io/)** — a released package is
  `(name, version, architecture)` with per-file checksums and detached signatures, and a *suite*
  (stable/testing) decides availability separately from presence in the pool: presence is not
  publication, which is ADR-0043's gate and this ADR's point 3 in thirty-year-old production form.
- **[BindPlane rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts)**
  — staged versions released by an explicit act, already ADR-0043's source; the versioned-Set
  shape extends the same separation from configurations to artifacts.
- **[OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)**
  — `PackagesAvailable` maps a name to one offered version/hash per Agent ("the packages that are
  available on the Server **for this Agent**"), which is both the constraint point 4 satisfies
  (reduce many Sets to one offer) and the licence for holding what is not offered.
- This repository: [ADR-0017](0017-selector-targeted-packages.md) (specificity, kept and extended
  across versions), [ADR-0018](0018-packages-imported-from-a-url.md) (file-or-source entries,
  kept), [ADR-0019](0019-one-step-back.md) (superseded — its own "keep every version" alternative,
  adopted with the registry role it warned about handed to explicit Sets and a named retention
  follow-up), [ADR-0029](0029-a-version-is-compared-and-shown-without-its-build-metadata.md) (the
  version comparison point 4 ranks by), [ADR-0031](0031-per-platform-package-variants.md) and
  [ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md) (structure superseded,
  guarantees kept), [ADR-0043](0043-a-package-is-published-before-it-is-offered.md) (the gate,
  kept and closed).

## Consequences

- Positive: **a release is one object.** Five platforms' artifacts under one identity, staged
  together, published together, with one version — the ambiguity "rollout in progress or forgotten
  upload?" stops being representable.
- Positive: **the publication gate has no exception left.** Nothing an operator saves — new Set,
  new entry, replaced draft entry — reaches any Client until the publication act, and published
  bytes cannot change underneath the fleet.
- Positive: **rollback becomes visible and multi-step.** The store openly holds what ADR-0019 hid;
  falling back is retracting a Set, the target is any version still published, and the fleet view
  can show the whole ladder.
- Positive: no untyped state, no late-typing window, and the type can never disagree with itself
  across platforms.
- Positive: the UI ships the model in the same change, and its master–detail shape mirrors the
  store one to one — the table is the Set list, the form is one Set, and every constraint the form
  shows (frozen identity, entries locked while published) is the Server's own rule, not a second
  implementation of it.
- Negative / trade-offs: **the largest package-API break yet** — every route under
  `/api/v1/packages` changes shape, `/type` and `/rollback` disappear, and every script and the
  bundled UI must follow. Pre-1.0, and the alternative is carrying two models; but ADR-0031 called
  its four route changes the largest break since ADR-0012, and this is larger.
- Negative / trade-offs: **disk is unbounded by design.** ADR-0019 capped a package at twice its
  size; Sets accumulate until deleted, which is the "artifact registry by accident" that ADR
  warned about — accepted knowingly, because the registry role is now explicit and manual deletion
  is one request, with retention named as the follow-up.
- Negative / trade-offs: **greater-version-wins puts semantics on the version string.** Two
  equally aimed Sets are ordered by ADR-0029 comparison, so a fleet whose versions are not
  comparable (Foreign Agents numbering freely) falls to the conflict rule and must keep Selectors
  disjoint or publication exclusive. The rule is mechanical but it is one more thing the manual
  must state plainly.
- Negative / trade-offs: adding a platform to a published Set means retract → add → republish — a
  window in which the Set reaches nobody. Rare (a release is normally built whole), and the
  alternative (mutable published Sets) reopens the gate; but it is a real two-minute cost on a
  case the old model handled in one upload.
- Negative / trade-offs: migration turns each package into as many Sets as it had distinct
  variant versions, and an untyped stored package **fails startup** until the file is removed —
  ADR-0031's medicine, applied once more, with the same operator cost.
- Follow-ups: a retention policy for superseded Sets (the question ADR-0019 answered with "keep
  one" and this ADR reopens — by topic, with the Agent-record and version-directory retention
  questions it joins); surfacing the version ladder per name in the bundled UI; and extending the
  ADR-0051 storage-port pattern to this store once that ADR's precedent stands.
