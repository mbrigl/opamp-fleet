# ADR-0095: A Package is what an Agent type runs at a version — the name and the aim leave it

- **Status:** 🟡 proposed
- **Date:** 2026-08-23
- **Deciders:** Markus Brigl

Supersedes [ADR-0052](0052-a-package-is-a-versioned-set.md) on acceptance — points 1, 4, 5, 6, 7
and 8. **Point 2 survives whole and is cited, not restated**: a Set holds one entry per canonical
`(os, arch)` pair, each entry is either an uploaded artifact or a source reference (ADR-0018), and
one entry suffices. That sentence is this ADR's entry model unchanged.

Amends without superseding: [ADR-0031](0031-per-platform-package-variants.md) (the Platform
vocabulary, the alias table, and mandatory platform fit stand),
[ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md) (the type is now *half the
identity*, so its late-typing window and its mutable-attribute shape become unrepresentable rather
than merely retired), and [ADR-0083](0083-what-reaches-an-agent.md) (points 2 to 6 and 9 to 11 are
untouched; only *which key* `claimed_version` reads changes).

This ADR decides **the object**. Its companion, [ADR-0096](0096-a-deployment-aims-packages-at-a-channel.md),
decides **the act** and takes in what this one gives up. Accepting this one without that one leaves
packages that nothing can aim; the dependency runs one way only.

## Context

A Set is identified by the triple *(name, Agent type, version)* and carries, besides its artifacts,
a Selector, an `addon` flag, and one optional signature per entry. Four observations, all from this
repository:

**1. The name is a second identity for a thing that already has one.** The Agent type is compared
raw against the reported `service.name` and decides fit (ADR-0034). The name decides nothing — it
is the map key on the wire and the grouping key for resolution, and in every artifact this project
ships the two are already the same string. `fleet-packages/supervisor@0.4.3@supervisor/` is not a
coincidence: ADR-0082 fixed the Client's own Agent type at `supervisor` and named its Set after it,
and `opamp-package-fetch` uploads to `/api/v1/packages/{service_name}/{service_name}/{version}`
without ever having been told to. The model carries a degree of freedom nobody uses, and the one
place it *could* be used — two Sets of one Agent type under different names — is precisely the
ambiguity `resolve` has to rank its way out of.

**2. The `addon` flag is a Server-side concept with no Client behind it.** The Baseline distinguishes
top-level packages from addons, and this Server stores, offers and ranks both. The Client refuses
every one of them: an offer of addons only is reported as an operator error
([`agent.rs`](../../crates/client/src/supervisor/agent.rs), the `PackageType::Addon` filter). The
flag has therefore never changed what any host installs. It only changes what the store must sort,
and it takes a byte in the offer hash preimage for the trouble.

**3. Aim on the object makes "what is this artifact" and "who gets it" one editable record.** The
Selector is mutable while the bytes are frozen (ADR-0052 point 3), which is correct and also the
sign that two different lifetimes are living in one document. Its consequence is ADR-0017 point 3:
where several Sets match an Agent, the most specific Selector wins. That rule is a ranking an
operator cannot see — the answer to "which artifact does this host get" is not readable off any one
object, it has to be computed across all of them.

**4. Nothing is deployed.** The package store is empty; no fleet holds an installed record written
by this Server. Every compatibility argument that would normally shape a change of this size —
migrate the store, keep the hash preimage stable, alias the old routes — is void. That is not a
licence to be careless; it is a licence to *delete*, and AGENTS.md §1 asks for exactly that.

## Decision

We will make the **Package** the unit the store holds, identified by **(Agent type, version)** and
holding nothing but its entries — no name of its own, no Selector, no kind flag, no signature — and
we will remove the migration machinery that has no store left to migrate.

1. **Identity is the pair, stated at creation, never edited.** A Package is created as
   `(agent_type, version)`; both tokens keep the conservative grammar ADR-0052 point 1 introduced
   (printable, no path separators, no `@`), so the pair embeds losslessly in a file name and a URL.
   A new version is a new Package. The free-form `name` and the ADR-0010 name grammar leave the
   Package entirely; the grammar moves to the Deployment's name, where a human-chosen label belongs.

2. **The name is derived, and there are two of them — they answer different questions.**
   - The **wire name** is the Agent type alone. It is the `PackagesAvailable` map key, the
     `PackageStatuses` key an Agent reports back, and the value `[self_update] package` is compared
     against. It must be *stable across versions*, which is why it cannot carry one.
   - The **display name** is `<agent_type> <version>` — the UI, the log line, the operator's ear.

   This is the sharpest consequence of point 1 and the one most easily got wrong: deriving one name
   and using it in both places would either break the Client's self-update guard on every release or
   make the fleet view unreadable.

3. **A Package holds entries and a derived hash, and nothing else.** The entry model is ADR-0052
   point 2 verbatim. The per-package hash the wire carries keeps its construction and **loses the
   kind byte**: `len(version) || version || content_hash`. The Platform still has no place in it —
   two platforms' artifacts differ by content hash by construction. The hash is surfaced to the
   operator on the entry, because it is the exact value an Agent compares, and an operator who can
   read it can answer "did this host take my bytes" without trusting a status field.

4. **The `addon` flag is removed from the Server.** Every Package is top-level; `PackageType::Addon`
   is never emitted. **The Client's refusal of addons stays exactly where it is** — it is not the
   other half of this flag but a defence against a non-conforming *peer*: the Baseline permits any
   Server to offer addons, this Client speaks to `opamp-go` in the conformance tests, and without
   the filter a foreign Server could write an addon over a Managed Process's binary. Only the
   Server's half goes.

5. **The store holds Packages, one directory each, and forgets how to migrate.** The layout becomes
   `<packages_dir>/<agent_type>@<version>/` holding `package.json` and one `<os>-<arch>.bin` per
   uploaded entry. `migrate_legacy` — the pre-ADR-0052 upgrade — and the `published` /
   `formerly_published` / `formerly_offered` seed that ADR-0061 point 9 needed are **deleted**, not
   carried forward: they exist to serve stores that do not exist. A directory in a shape this
   version does not know **fails startup naming the path**, rather than being skipped; that is the
   one line standing between an empty store and a half-tidied development machine, and it follows
   ADR-0031 point 8's rule that an unreadable store is loud.

6. **The REST resource is the pair.** `/api/v1/packages/{agent_type}/{version}`, with the entry
   routes beneath it unchanged in shape. The create body has **no writable field left** — the
   Selector and the `addon` flag were all it held — so `{}` creates a Package. `PUT …/selector` and
   `POST …/rollout` disappear from the Package: aim and release are not its business
   (ADR-0096). The download route loses the same segment. The OpenAPI document follows (ADR-0012).
   The old routes are removed rather than aliased: an alias would need a `name` the model no longer
   has a field for, and there is no deployed consumer to spare.

7. **What ADR-0083 decides is untouched.** A Package still reaches an Agent only as an upgrade, the
   running `service.version` still decides in both directions, and an unorderable claim still
   refuses. One line changes: the claimed version is read under the Agent type, because that is now
   the key an Agent reports its status under.

## Alternatives considered

- **Keep the name as an optional field defaulting to the Agent type.** Smallest diff, and it leaves
  room for two artifacts of one type under different names. Rejected: the room is the problem. It
  is the degree of freedom that makes `resolve` need a ranking, and an optional identity field is
  one every operator must decide about once and no operator wants to.
- **Identity `(name, version)`, with the type as an attribute** — ADR-0052 already weighed and
  rejected this shape, and the reasons hold with the fields swapped: the type is what decides fit,
  and an attribute is editable. Re-typing published bytes to a different kind of Agent is exactly
  what immutable identity forecloses.
- **Keep the `addon` flag against a future need.** The Baseline has the concept and dropping it
  narrows what this Server can express. Rejected under §1 (YAGNI): no Client this project ships
  installs one, no artifact it builds is one, and the flag's only present effect is a byte in a
  hash and a branch in resolution. It can come back with the Client support that would give it
  meaning — named here as a follow-up topic, not a number.
- **Keep the kind byte in the hash preimage anyway, for stability.** With a deployed fleet this
  would be mandatory: dropping it changes every package hash, and the Client compares exactly that
  hash, so every host would re-download software it already runs. With an empty store it buys
  nothing and leaves an unexplained constant in a hash function for the next reader to fear.
  Rejected *because* the store is empty — and this ADR records the reason so a future reader does
  not restore the byte by cargo cult.
- **Keep `migrate_legacy` and add a second stage.** The conservative reading, and wrong here: a
  migration with no source is untestable in the only way that matters and untrue as documentation.
  Deleting it is a decision, so it is written down rather than done quietly.
- **One opaque Package ID (a hash) instead of two path segments.** ADR-0052 rejected this and the
  reason is unchanged: the ID would appear in URLs, file names and the UI while meaning nothing to
  a human, and a bounded token grammar buys the same safety while keeping `telegraf@1.30.0`
  readable in a directory listing.

## Sources / Prior art

- **[OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md)** —
  ADR-0052's model, and closer still without the extra name: a *reference* is `name:tag`, and the
  name is the software's identity, not a label beside it. One manifest list, one entry per
  `platform` object, each naming its digest.
- **[Debian repository format](https://wiki.debian.org/DebianRepository/Format)** /
  **[RPM](https://rpm-software-management.github.io/)** — a released package is
  `(name, version, architecture)`; the name *is* what the software is, and nothing carries a second
  identity beside it. Thirty years of the shape this ADR converges on.
- **[OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)**
  — `PackagesAvailable` maps a name to one offered version and hash *for this Agent*, and states
  there is "normally only one top-level package, which implements the primary functionality of the
  Agent". Point 4 makes that structural rather than enforced, and point 2's stable wire name is what
  the per-Agent map key has to be.
- This repository: ADR-0010 (the name grammar, now the Deployment's), ADR-0018 (source entries,
  kept), ADR-0031 and ADR-0034 (guarantees kept, structure amended), ADR-0052 (superseded; its
  entry model carried forward verbatim), ADR-0061 (the act, amended by ADR-0096), ADR-0082 (the
  Client's own Agent type is `supervisor`, which is why the derived name needs no exception),
  ADR-0083 (the version test, untouched).

## Consequences

- **Positive:** the answer to "what is this artifact" is readable off one object with two fields.
  Two Packages of one Agent type at one version become unrepresentable rather than a conflict to
  rank. `resolve` loses its grouping pass, its specificity comparison and its addon partition.
  Several hundred lines of migration and publication-seed machinery leave the tree.
- **Negative / trade-offs:** two artifacts of one Agent type at one version — two Collector builds
  at `0.109.0`, say — can no longer coexist; they must differ in type or in version, which is what
  their `service.name` already says about them. The kind flag's removal narrows this Server against
  the Baseline. And an operator who typed the Client's `[self_update] package` as something other
  than an Agent type will see that Client refuse the offer, visibly, on its fleet row.
- **Follow-ups (by topic, never by number):** whether the Client's `[self_update] package` key
  should be renamed now that its value is an Agent type; whether addons return once a Client can
  install one; a retention policy for superseded Packages, which ADR-0052 left open and which stays
  open.

### The wording the specification needs

`docs/SPECIFICATION.md` defines **Package** at line 242. This ADR does not change it unilaterally —
the specification outranks every ADR (AGENTS.md §3.4). The conflict is raised here, with the wording
this decision would need, to be accepted or amended together with the decision:

> - **Package** — a versioned, downloadable software artifact an Agent installs, identified by the
>   **Agent type it is built for and its version**; its display name is derived from the two. It is
>   verified against a content hash, and against a signature where one is configured — the signature
>   travelling with the Deployment that offers it rather than with the artifact record. The Server
>   offers Packages; an Agent reports the status of each. This is how the Server updates an agent's
>   software, not only its configuration.
