# ADR-0016: A package is a versioned Set for one Agent type, aimed by a Selector and chosen by the Server — identified by name, Agent type, and version, with one entry per platform

- **Status:** 🟢 accepted
- **Date:** 2026-08-11
- **Deciders:** Markus Brigl

## Context

Package delivery works end to end (ADR-0015): an operator uploads an artifact through the REST API
and every capable Agent downloads, verifies, applies, and health-gates it, reporting progress on the
way. Three questions decide what that delivery is for: *who* gets an artifact, *which kind of Agent*
it is built for, and *what object* the store holds.

**Aim belongs on the Server.** Configurations are targeted by a Selector
([ADR-0012](0012-selector-targeted-configurations-and-rest-api.md)) — matched against the
attributes an Agent reports. Goal 9 states that a change may address *"the whole fleet or a chosen
subset of it, so a configuration can be rolled out to part of the fleet before all of it"*, and the
vision applies that same expectation to software: *"it can update an agent's binary in place"* as
part of the same control loop. A binary is precisely the change an operator wants to try on five
hosts before three hundred. If the host named its package in its own configuration file, the two
things an operator most needs to steer — *who* gets an update and *which* artifact they get — would
be settings in a file on every managed machine, and nothing about them would be reachable from the
REST API or the bundled UI, which is what goal 5 promises an operator. Every Agent already reports
`host.arch` and `os.type` in its description.

The protocol is not in the way. The Baseline describes `PackagesAvailable` as *"the packages that are
available on the Server **for this Agent**"* — the offer is per-Agent by design, and its
`all_packages_hash` gates re-offering per Agent as well. The Server composes a per-Agent remote
configuration this way already.

**The Agent type must not be left to the Selector.** ADR-0021 made platform fit mandatory, because
"a binary that cannot run on the machine it is sent to is not a targeting mistake to be resolved by
precedence — it is not a candidate", and because a Selector *can* express the platform but is
opt-in, "and the failure mode of forgetting it is the worst one this system has". The Agent type is
the other half of "is this artifact meant for this Agent". Expressed only as a Selector pair, it
starts empty and matches everything: an operator who uploads a Promtail artifact for the right
platform and forgets the Selector has it downloaded, verified, unpacked and swapped over the
**Collector's** binary on every consenting host. The health gate catches it (ADR-0015) — the process
will not stay up and is rolled back — but that is the fleet-wide outage window ADR-0021 refused to
accept for the platform. A rule that is only ever right when remembered is not the rule this needs.
[ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) makes the type a fact
the Server can read: `service.name` is the type, resolved from what the Managed Process reports, else
the block's `service_name`, else the program's file name — so every Agent this Client presents
reports a type, always.

**The Client's own binary is protected by its self-update consent.** ADR-0017 makes `[self_update]
package` the protection, because "a package with an empty Selector reaches every consenting Agent,
so without a name to match, the first fleet-wide Collector artifact someone uploads would be
installed over the Client and take the host out of reach". That check lives on the host being
protected and compares a *package name* — an operator who gives a Collector package the Client's
package name defeats it. A Server-side type fit is an independent second guard on the one case where
the blast radius is the Client itself.

**The store needs an object for a release.** If a package were a *name* carrying a Selector and an
Agent type, with per-platform artifacts each holding its own version, three things would follow:

- **"Which version is this package at" would have up to five answers.** `linux-amd64` could hold
  `3.1.0` while `windows-amd64` still holds `3.0.0` — sometimes a rollout in progress, sometimes a
  forgotten upload, and the model could not tell the two apart. A release is one version across five
  artifacts.
- **The type would arrive late.** Stated on a sub-resource after upload, it leaves a window in which
  a package exists untyped, a state whose only meaning is "not finished", plus a rule to make that
  window safe. The window is an artifact of the request choreography, not of the domain.
- **Replacing bytes in place** would let a hash change under an offer, and would force the store to
  remember secretly, one step deep, what each new version destroyed.

So a package defines a **Set**. Each Set is identified by **name, Agent type, and version**, and may
define a Selector. A Set holds **one or more entries**, each identified by **os and arch**, with no
two entries for the same combination. An entry carries **either an uploaded file or a source with
its SHA-256**, and optionally a signature. **Saving a Set does not make it available to any Client**
— releasing it is its own act ([ADR-0030](0030-a-rollout-is-an-explicit-act.md)).

The wire imposes one constraint and one freedom. `PackagesAvailable` maps a package *name* to one
offered version and hash per Agent — so whatever the store holds, offer resolution must reduce
"every Set named `otelcol`" to at most one per Agent. And the Baseline leaves the offered set
entirely to the Server, so holding many versions and offering one is conformant.

## Decision

We will make the **Set** the unit the package store holds, identified by **(name, Agent type,
version)**, give it a **Selector** exactly as a Configuration has one, offer it only to Agents of its
type and platform, and make the **Server** decide which Set an Agent is offered — so a rollout is
aimed from the REST API and the UI, not from a file on each host.

### Aim: the Selector, and the Server's choice

1. **A Set carries a Selector.** An empty Selector means the whole fleet, as with Configurations.
   The Server considers for an Agent only the Sets whose Selector matches its reported attributes —
   and the labels the Server sets on it (ADR-0027 clause 2) — and computes `all_packages_hash` over
   *that* Agent's set, so the Baseline's re-offer gate keeps working per Agent. What is actually
   offered is composed from what has been rolled out to the Agent; matching computes the candidate
   (ADR-0030 point 3).

2. **A Supervisor says only whether it accepts updates, never which one.** How a host consents is
   ADR-0018's path rule (ADR-0018 clause 2, as narrowed by ADR-0018 clause 2). A `package = "name"`
   key in a `[[supervisor]]` block is **removed**: choosing the artifact is the Server's job, and
   leaving a second way to choose it on the host would keep the decision in the place this decision
   moves it out of. A configuration file still carrying `package = "…"` fails at startup with a
   message naming the replacement — loudly, as ADR-0008 requires, never silently ignored.

   Pinning one host to a specific artifact does not disappear; it moves to the Server, where a
   Selector on that host's `host.name` (or an operator attribute) expresses it — and, unlike a line
   in a file on that machine, is visible in the fleet view.

3. **At most one Set per name per Agent: the most specific Selector wins, the greater version breaks
   a tie.** The Baseline states there is *"normally only one top-level package, which implements the
   primary functionality of the Agent"*, and a Supervisor has one binary to replace. Among the
   candidates that fit an Agent (clause 5) and *share a name*, the **most specific Selector wins** —
   the one naming the most attributes. That is what makes the shape an operator actually reaches for
   work: a fleet-wide Set with an empty Selector, plus a narrower one aimed at the hosts a rollout
   starts on, which overrides it for exactly those and leaves everyone else alone. Among equally
   specific candidates the **greater version** wins, compared as ADR-0009 compares versions.

   Only a tie that version comparison cannot break — equal versions, or values that are not versions
   at all — has no defensible answer. That Agent is offered nothing under that name, and the fleet
   view says why on the Agent itself (`package_conflict`), because a rollout that silently never
   starts is worse than one that explains itself. Matching addons are offered alongside the chosen
   top-level package.

   The winner is offered under the Set's name with the Set's version and the fitting entry's hash —
   **nothing changes on the wire or in the Client**. Whether a candidate also moves the Agent
   forward is ADR-0035's version test (ADR-0035 points 2 to 6); what rollback is, is ADR-0035 point
   11.

4. **The Selector is set through its own sub-resource**, `PUT …/selector` beneath the Set's route
   (clause 12), with the same JSON shape a Configuration uses.

### Fit: the Agent type

5. **Fit before aim, in two steps, neither optional.** Candidate resolution drops every Set whose
   Agent type is not the Agent's reported `service.name`, then every Set holding no entry for the
   Agent's platform (ADR-0021 point 3), and only then runs the aiming of clauses 1 and 3 over what is
   left. Type first because it is the cheaper comparison and the coarser cut. **A Set is offered only
   to an Agent of its type.**

   The type is compared **raw**, with no canonicalisation table. ADR-0021 could canonicalise because
   the semantic conventions enumerate operating systems and architectures; there is no canonical set
   of Agent types and inventing one would mean this Server having an opinion about every collector
   distribution that exists. The value an Agent reports is the value to write.

6. **An Agent that reports no `service.name` fits nothing**, the same rule ADR-0021 applies to a
   missing platform, and for the same reason: "unknown type, so anything goes" would put the
   mismatched-binary failure straight back. Every Client this project ships reports a type
   (ADR-0022); a foreign OpAMP client that does not is told so on its fleet row rather than left with
   a rollout that never starts.

7. **The Client's self-update has a second, independent guard.** The Server will not offer a Set
   typed `otelcol-contrib` to an Agent reporting `supervisor` (ADR-0022 clause 6), whatever the Set
   is named, and the ADR-0017 name check still runs on the host. Neither replaces the other: one is
   the Server refusing to send, the other the Client refusing to install.

### The Set

8. **Identity is the triple, stated at creation, never edited.** A Set is created as one document —
   name, Agent type, version, and optionally a Selector — and the triple is its key: creating
   "the same Set again" addresses the same resource, and a new version is a **new Set**, never a
   mutation of an old one. There is no moment where a Set exists untyped, and no inert state to
   explain; the type is stated once per Set, not once per artifact. Name follows the ADR-0010
   grammar; version and Agent type are bounded to a conservative token grammar (printable, no path
   separators, no `@`) so the triple embeds losslessly in file names and URLs.

9. **Entries are a map keyed by Platform, so a duplicate is unrepresentable.** A Set holds one entry
   per canonical `(os, arch)` pair (ADR-0021's vocabulary and alias table, unchanged). Writing an
   entry for a pair the Set already holds *replaces* that entry — while the Set's entries are not
   frozen (clause 10). Each entry is **either** an uploaded artifact (the Server computes and stores
   its SHA-256) **or** a source reference (URL, mandatory SHA-256, optional headers — ADR-0015
   unchanged), and either way an optional Ed25519 signature. One entry suffices for a Set to be
   released; five platforms are five entries under one identity, which is what makes a release one
   object.

10. **Saving a Set reaches nobody, and released bytes do not change.** Saving only saves; what
    reaches an Agent is the rollout act (ADR-0030 points 1 and 5). There is no in-place upgrade: the
    ordinary version bump is a new Set, saved and released like everything else. A Set with no
    entries is never released (`409`, ADR-0030 point 5): a Set *contains one or more entries* by
    definition, and the empty state exists only while an operator is still assembling one. When a
    Set's entries are frozen is ADR-0030 point 8's rule; while they are, writing or deleting an entry
    is refused (`409`): the fleet is installing those bytes, and a hash that changes under an offer
    is the confusion the release act exists to prevent. The **Selector stays editable in every
    state** — aim is not bytes, and moving a Set between rings is how a rollout proceeds (clause 3).

11. **The store holds Sets, one directory each.** The filesystem layout is
    `<packages_dir>/<name>@<version>@<type>/` holding `set.json` (identity, Selector, and every
    entry's metadata) and one `<os>-<arch>.bin` per uploaded entry — the grammar of clause 8 is what
    makes the directory name parse back unambiguously, as ADR-0021's `@` trick did. There is no
    `.previous.bin` and no hidden history: every version the store keeps, it keeps openly, as a Set.

12. **The REST resource is the triple.** The package routes address
    `/api/v1/packages/{name}/{agent_type}/{version}`: `PUT` creates the Set (body: Selector,
    optional), entry routes beneath it write bytes (`PUT …/entries/{os}/{arch}`, body the artifact,
    optional `signature=<hex>`) or a source (`PUT …/entries/{os}/{arch}/source`, body as ADR-0015),
    `PUT …/selector` keeps its shape, `DELETE` removes an entry or the Set. The release routes are
    ADR-0030 point 5's. `GET /api/v1/packages` lists Sets grouped by name, each with its entries and
    reach. There is no `/type` and no `/rollback` route — the type is identity, and rollback is not
    an act on the store (ADR-0035 point 11). The OpenAPI document follows, as it must (ADR-0012).

13. **The bundled UI supports the model as a master–detail view.** The Packages tab is **one table
    of Sets** — one row per Set, showing its identity (name, Agent type, version), the platforms it
    holds, and its reach. Selecting a row makes that Set the *current* record and shows it in a
    **detail form**; while nothing is current, the form is not visible. **Create** opens the form
    empty to define a new Set — the only moment the identity fields are writable (clause 8); on an
    existing Set the form offers exactly what the API offers: the Selector always, the entries while
    they are not frozen — each entry removable in place, and a new one addable in place through its
    own **Add** action, so assembling a five-platform release is five adds on one Set rather than
    five saves. **OK** persists the form's changes through the REST routes (including an entry still
    sitting in the editor) and **Cancel** discards them; both conclude the form — the current record
    is let go and the form leaves the screen, a failed save alone keeping it open beside its message.
    Delete stands alone on the form's left, Cancel and OK conclude it on the right; **Delete** removes
    the current Set, after which nothing is current and the form hides. One thing is deliberately
    *not* in the form at all: releasing the Set. The press that releases a rollout is never the press
    that carries the bytes, and a release folded into "save" would be armed by the same click that
    edits a Selector; the releasing controls are ADR-0030 point 10's. The form enforces no rule of its
    own: what it greys out is what the Server answers `409` to, one rule, rendered.

14. **An existing store is migrated at first open — loudly where it cannot be.** Each stored
    package becomes one Set per distinct variant version, carrying the entries that share that
    version and the package's Selector, type, and publication state (which ADR-0030 point 9 then
    reads as rolled out); a remembered previous version becomes a Set of that version that reaches
    nobody, so nothing an operator could go back to is lost. A stored package with **no Agent type
    cannot become a Set** — the type is identity — and fails startup naming the file, ADR-0021 point
    8's rule: it was inert before and would be unrepresentable now, and inventing a type would aim
    bytes this Server cannot judge.

## Alternatives considered

- **Leave targeting to the host's configuration file.** Rejected. It works — the platform case is
  solved by naming `otelcol-linux-amd64` per host — but it puts the rollout decision on three hundred
  machines and outside the API that goal 5 makes the integration contract. A staged rollout then
  means editing and redistributing host configuration, which is the problem this project exists to
  remove.
- **Reuse Configurations to carry the package name.** Rejected. It would let an operator aim a
  rollout with the mechanism that already aims, but it conflates two lifecycles: a configuration is
  applied by restarting on new files, a package is verified, swapped, health-gated, and rolled back.
  Tying them means a config change and a binary change cannot be reasoned about — or reverted —
  separately.
- **Keep `package = "name"` on the host as an explicit pin.** Rejected, after weighing it as the
  compatible option. It would spare existing host configurations a change, and a pin is occasionally
  what an operator wants — a test host that must not follow the fleet. But it leaves two ways to
  decide the same thing, one of them on the machine this decision stops editing, and the pin is
  expressible on the Server anyway: a Selector matching that host's `host.name`. One mechanism,
  visible in the fleet view, beats two that can disagree.
- **One key with two meanings (`package = true` or `package = "name"`).** Rejected. It is the most
  compact spelling, but the same key then means "whether" in one form and "which" in the other — a
  distinction a reader has to know rather than see. Config keys are read under pressure; this one
  would be read wrong.
- **Refuse an overlapping Selector at the API.** Rejected on contact with the code, which is the
  honest reason: a Selector left empty overlaps everything, so with every Set starting that way the
  first `PUT` is always refused, and there is no order in which two Sets can be narrowed — the store
  cannot leave the state it starts in. It also forbids the fleet-wide-plus-canary shape outright,
  which is the whole point. Resolving at candidate time has no such dead end, and it turns the common
  case (a default plus an override) from an error into the mechanism.
- **Target by Instance UID instead of a Selector.** Rejected. Naming Agents individually pins a
  rollout to identities that are reassignable (`AgentIdentification`) and says nothing about *why*
  those hosts were chosen. A Selector over reported attributes survives re-identification and is
  self-documenting — and it is already the project's vocabulary.
- **Leave the Agent type to the Selector and document it.** `{"service.name": "otelcol-contrib"}`
  does the filtering at zero cost. Rejected on ADR-0021's own words: it is opt-in, and forgetting it
  bricks an agent on every host of every other type. Nothing about the role makes the argument
  weaker than for the platform — if anything the blast radius is larger, since a wrong-platform
  binary fails to exec while a wrong-role binary may start, run, and quietly collect nothing.
- **Default the type to the package name.** A package named `otelcol-contrib` would be for Agents of
  that type unless overridden, which needs no new input for the shape operators already use.
  Rejected: it is right most of the time and silently wrong for `otelcol-canary`,
  `collector-3.1.0-rc`, or any name chosen for the rollout rather than the target — and a mechanism
  whose entire purpose is "not optional" cannot rest on a guess that is usually correct. ADR-0021
  rejected the structurally identical "optional Platform, empty meaning every platform".
- **State the type on each artifact upload**, as the platform is. Rejected: five platform uploads
  under one name would state the type five times and could disagree, and resolving that disagreement
  (last writer wins? refuse a mismatch?) is a rule this project would rather not have. The type is
  the Selector's kind of thing, not the artifact's — stated once per Set (clause 8).
- **Match the type as a prefix or a pattern** (`otelcol*` covering `otelcol` and `otelcol-contrib`).
  Rejected: those are genuinely different binaries with different components, which is the whole
  reason the distinction exists, and a matching language is a decision that outlives its convenience.
  Two packages, two types.
- **Canonicalise types through an alias table**, as ADR-0021 does for platforms. Rejected in clause 5:
  there is no authority to canonicalise against, and a table this project maintained would encode its
  opinion of every distribution in the world.
- **Keep a package as a name with per-platform artifacts and only add staging.** Smallest change, and
  it fixes only one of the three seams: versions stay per-platform, the untyped window stays, and
  rollback stays a hidden single step. Rejected: each seam traces to the same root — the store has no
  object for a release.
- **Version per platform artifact.** It permits a per-platform rollout of different versions under
  one name without multiple Sets. Rejected: multiple Sets express that case explicitly (two Sets, two
  Selectors) — and the implicit form is indistinguishable from a half-finished upload, which is the
  ambiguity operators actually hit.
- **Identity without the Agent type** — `(name, version)`, type as an attribute. One fewer path
  segment, and two Sets differing only by type are odd. Rejected: the type is as constitutive of
  "what is this artifact" as the version (clause 5's whole point), and an attribute is editable —
  retyping released bytes to a different kind of Agent is exactly the mistake immutable identity
  forecloses.
- **Refuse every same-specificity tie instead of greater-version-wins.** Simpler rule, no version
  ordering needed, fully explicit. Rejected, narrowly: it makes the natural end state of every
  rollout — old and new both aimed fleet-wide — a conflict that stops offers entirely, so operators
  must take the old version out of reach at the precise moment they finish a rollout. Greater-
  version-wins keeps the old version harmlessly in place; the tie rule still catches genuinely
  incomparable versions.
- **Mutable entries on released Sets** (in-place replacement). Rejected: it is the hole in the
  release gate, restated; with cheap versioned Sets the ordinary upgrade has a first-class shape, and
  immutability is what lets an operator read a released Set's hash as a fact.
- **A Set may be empty.** Rejected for a released Set: a Set contains one or more entries, and an
  empty one is an offer of nothing that still occupies a name and a version.
- **Automatic pruning of superseded Sets.** Keeping one version bounded disk; Sets unbound it.
  Rejected *here*: retention is the same broad question ADR-0025 deferred for Agents and ADR-0010 for
  version directories, and deciding it as a footnote would repeat the mistake those ADRs declined.
  Deleting a Set is one request; the follow-up names the policy question.
- **Encode the triple as one opaque Set ID** (a hash) instead of three path segments. Robust against
  any character in any field. Rejected: the ID would appear in URLs, file names, and the UI while
  meaning nothing to a human; a bounded token grammar (clause 8) buys the same safety and keeps
  `otelcol@3.1.0@otelcol-contrib` readable in a directory listing.

## Sources / Prior art

- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — *"The PackagesAvailable message describes the packages that are available on the Server for this
  Agent"*, and *"There is normally only one top-level package"*: the per-Agent offer and the
  single-top-level-package expectation this decision builds on. `PackagesAvailable` maps a name to
  one offered version/hash per Agent, which is both the constraint clause 3 satisfies (reduce many
  Sets to one offer) and the licence for holding what is not offered. What a package contains "is
  Agent type-specific and is outside the concerns of the OpAMP protocol": the protocol names the
  concept, declines to model it, and leaves the Server to decide.
- [`opamp-go`](https://github.com/open-telemetry/opamp-go) — checked as the behavioural oracle: its
  example Server only logs `PackagesAvailable`/`ServerProvidedAllPackagesHash` and never offers a
  package, so there is no upstream targeting behaviour to copy. The model is ours to choose, within
  what the protocol already provides.
- The Baseline's `AgentDescription`
  ([`opamp.proto:690`](../../crates/opamp/proto/v0.20.0/opamp/v1/opamp.proto#L690)) — `service.name`
  as "a reverse FQDN that uniquely identifies the Agent type", which is the attribute the type fit
  reads and the reason it is the right one to read.
- **[OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md)** — one
  *reference* (name + tag ≈ name + version) resolves to a manifest list with one entry per required
  `platform` object, each naming its digest — a Set with entries, almost field for field. Registries
  also settle clause 10's rule: a pushed manifest's digest is immutable; moving consumers is done by
  moving references, not by rewriting bytes. It is also the counter-example for the type: an image
  index discriminates on platform alone and leaves "what is this image for" to the name — the design
  this decision declines, because a container name is chosen by whoever pulls it and a package name
  is chosen by whoever uploads it.
- **[Debian repository format](https://wiki.debian.org/DebianRepository/Format)** /
  **[RPM repositories](https://rpm-software-management.github.io/)** — a released package is
  `(name, version, architecture)` with per-file checksums and detached signatures, and a *suite*
  (stable/testing) decides availability separately from presence in the pool: presence is not
  release, which is clause 10 in thirty-year-old production form.
- [Bindplane — Bring Your Own Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the comparable product models an Agent Type as a first-class object that a collector reports via
  `dist.name`, and hangs what an agent may receive off it rather than off free-form labels: evidence
  that type-as-a-property, not type-as-a-tag, is the shape that holds up in a shipping fleet manager.
  **[Bindplane rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts)**
  — staged versions released by an explicit act; the versioned-Set shape extends the same separation
  from configurations to artifacts.
- [ADR-0012](0012-selector-targeted-configurations-and-rest-api.md) — the Selector semantics
  this reuses verbatim (every pair must equal a reported attribute; an empty Selector matches all),
  so an operator learns one targeting mechanism, not two.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) — the delivery, verification, and
  health-gating this leaves untouched; only *who is offered what* changes.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) (file-or-source entries, kept),
  [ADR-0009](0009-version-from-cargo-toml-and-git.md) (the version
  comparison clause 3 ranks by), and [ADR-0021](0021-one-platform-vocabulary.md) (the platform
  vocabulary, its canonicalisation, and the mandatory platform fit; the model for the type fit's
  fit-before-aim step, its "reports nothing fits nothing" rule, and the reasoning that a Selector
  which must be remembered is not a guarantee — diverging on one point, the startup refusal, because
  a Set cannot be untyped).
- [ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) — what makes a type
  matchable at all, and the resolution order (`opampextension`'s `dist.name` → the block's
  `service_name` → the program's file name) that guarantees every Agent this Client presents has one.

## Consequences

- Positive: a binary rollout can be staged — a Selector on `role`, `host.name`, or an operator
  attribute reaches five hosts first, and widening it is one API call. This is goal 9 applied to
  software rather than only to configuration.
- Positive: a heterogeneous fleet stops needing per-host wiring. `host.arch` and `os.type` are
  already reported, so one Set with an entry per platform updates every machine from the Server —
  and the UI can show which Agents a Set targets, as it already does for Configurations.
- Positive: **the worst remaining operator mistake stops being available.** A Set can no longer be
  installed over an Agent of another type, whatever its Selector says or fails to say, so the
  fleet-wide outage window ADR-0021 closed for the platform is closed for the role.
- Positive: **the Client's own binary is protected twice, on both sides of the wire** — the Server
  will not offer it a foreign artifact and the Client will not install one.
- Positive: the Selector is purely about *aim* — rings, environments, canaries — instead of carrying
  the role as a pair that also perturbs the specificity count. A ladder of `{}` / `{rollout: canary}`
  does not have to reserve a pair for the type.
- Positive: **a release is one object.** Five platforms' artifacts under one identity, saved
  together, released together, with one version — the ambiguity "rollout in progress or forgotten
  upload?" stops being representable.
- Positive: nothing an operator saves — new Set, new entry, replaced entry — reaches any Client by
  being saved, and released bytes cannot change underneath the fleet.
- Positive: no untyped state, no late-typing window, and the type can never disagree with itself
  across platforms.
- Positive: the store openly holds every version as a Set, so the fleet view can show the whole
  ladder.
- Positive: the UI's master–detail shape mirrors the store one to one — the table is the Set list,
  the form is one Set, and every constraint the form shows (frozen identity, frozen entries) is the
  Server's own rule, not a second implementation of it.
- Negative / trade-offs: `all_packages_hash` is per-Agent, so the Server cannot keep one precomputed
  aggregate. The cost is small (a hash over the matching set per exchange) but getting it wrong means
  either a re-offer loop or a missed update — it needs its own test.
- Negative / trade-offs: **a host configuration that names a package does not start.**
  `package = "…"` is refused at startup; the artifact choice lives in a Selector on the Server.
- Negative / trade-offs: specificity is a precedence rule, and precedence rules have to be learned.
  "The Selector naming more attributes wins" is simple, but an operator who expects "last write
  wins" or "most recently uploaded wins" will be surprised once.
- Negative / trade-offs: a tie leaves an Agent with no Set under that name at all until someone
  narrows a Selector. It is reported on the Agent in the fleet view and in the Server's log, but it
  is a state an operator can reach by widening a Selector that was fine before, and nothing warns at
  the moment of the change — only afterwards, on the Agents it affects.
- Negative / trade-offs: a Selector that stops matching an Agent does **not** uninstall anything —
  the Agent keeps running what it installed. That is deliberate (the protocol has no "revert to the
  previous artifact" and a silent downgrade would be worse), but "remove from the Selector" reading
  as "leave it as it is" will surprise someone.
- Negative / trade-offs: **a mistyped type is a silent no-op.** ADR-0021 can catch a typo by
  canonicalising and echoing the pair back; there is no equivalent here, so `otelcol-contib` is
  indistinguishable from a type no Agent happens to run yet. The Set view showing it reaches no Agent
  is the mitigation, and it is weaker than the platform's.
- Negative / trade-offs: **an Agent's type can change under a stable configuration.** ADR-0022 notes
  that a Collector gaining the `opampextension` switches its reported type from the program's file
  name to `dist.name`; that also switches which Sets reach it. The change is correct in both cases
  and surprising in both.
- Negative / trade-offs: one more thing to get right before a first rollout works, in a system whose
  reason for existing is that rollouts should be easy. Type, platform, Selector, consent — four
  things, and only the last is derived rather than stated.
- Negative / trade-offs: **the largest package-API break** — every route under `/api/v1/packages`
  has the Set's shape, and every script and the bundled UI must follow it. Pre-1.0, and the
  alternative is carrying two models.
- Negative / trade-offs: **disk is unbounded by design.** Sets accumulate until deleted — an artifact
  registry, accepted knowingly, because the registry role is explicit and manual deletion is one
  request, with retention named as the follow-up.
- Negative / trade-offs: **greater-version-wins puts semantics on the version string.** Two equally
  aimed Sets are ordered by ADR-0009 comparison, so a fleet whose versions are not comparable
  (Foreign Agents numbering freely) falls to the conflict rule and must keep Selectors disjoint. The
  rule is mechanical but it is one more thing the manual must state plainly.
- Negative / trade-offs: entries are frozen while ADR-0030 point 8 says so, so adding a platform to
  such a Set is not one upload. Rare (a release is normally built whole), and the alternative
  (mutable released Sets) reopens the gate; but it is a real cost on a case a single upload would
  otherwise handle.
- Negative / trade-offs: migration turns each stored package into as many Sets as it had distinct
  versions, and an untyped stored package **fails startup** until the file is removed — ADR-0021's
  medicine, with the same operator cost.
- Follow-ups: a retention policy for superseded Sets — by topic, with the Agent-record and
  version-directory retention questions it joins; surfacing the version ladder per name in the
  bundled UI; extending the ADR-0025 storage-port pattern to this store once that ADR's precedent
  stands; and a bulk restart across a selected set of Agents, which the fleet view's unused
  checkboxes already suggest.
