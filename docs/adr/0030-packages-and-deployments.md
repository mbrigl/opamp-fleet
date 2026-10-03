# ADR-0030: A Package is what an Agent type runs at a version, and a Deployment aims Packages at a channel, signs them, and is the only thing rolled out — an Agent belongs to at most one

- **Status:** 🟢 accepted
- **Date:** 2026-10-01
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-server/src/packages.rs`, `crates/fleet-server/src/deployments.rs`, the package and deployment routes of `crates/fleet-server/src/api.rs`, the package assignment in `crates/fleet-server/src/fleet.rs` and `crates/fleet-server/src/agent_store.rs`, the Packages and Deployments tabs of the bundled UI, `docs/SPECIFICATION.md`

## Context

[ADR-0020](0020-the-package-store.md) decides what the package store holds — one release of one
Agent type at one version, one entry per Platform, mandatory type and platform fit — and
[ADR-0027](0027-rollout-and-what-reaches-an-agent.md) decides when content reaches an Agent and the
version test it must pass. This record completes both: it states **what identifies a Package**,
its name on the wire, its hash, its layout and routes (completing ADR-0020 clauses 2, 3, 10 and
13), and **how a Package is aimed, signed and released** — the aim ADR-0027 clause 9 requires, the
object its resource-level act (clause 5) names, and the counts clause 19 asks for.

Four observations shape it:

- **A name beside the Agent type is a second identity for one thing.** The type decides fit; a
  free-form name would decide nothing but would group, and the only thing it could add — two
  artifacts of one type under different names — is an ambiguity that resolution would have to rank
  its way out of. In every artifact this project ships the two are the same string already
  (`supervisor`).
- **The Baseline's addon kind has no Client behind it.** The Client refuses every addon offer, as a
  defence against a foreign Server writing one over a Managed Process's binary; a Server-side kind
  flag would never change what a host installs.
- **Aim on the artifact makes "what is this" and "who gets it" one record**, and where several aims
  match one Agent, "which artifact does this host get" becomes a computation across all of them —
  a specificity ranking with ties to refuse — which no operator can read off any one object.
- **A Selector cannot say "not".** Every Selector is equality over the effective description
  ([ADR-0016](0016-configurations-and-the-rest-api.md)); "everyone except the canary hosts" is not
  writable. Disjoint targets must come from membership — which is what Server-set labels
  ([ADR-0026](0026-the-fleet-record.md)) and provisioned attributes already are.

What an operator signs off on is a release to a set of machines, not a pile of bytes; and a rollout
should name "what this channel runs", not *n* artifacts nothing holds together.

The package store and the Agent records are read only in the shapes this Server writes.

## Decision

We will identify a **Package** by its Agent type and version alone, holding nothing but its entries,
and introduce the **Deployment** — a named channel aimed by a non-empty Selector, holding one
Package per Agent type and the signature of each artifact — as the only thing that is rolled out,
with an Agent belonging to at most one.

1. **A Package is identified by `(agent_type, version)` and nothing else.** Both are tokens of 1–64
   characters from letters, digits, `.`, `_`, `+` and `-` — no `@`, no path separator — so the pair
   embeds losslessly in a directory name and a URL. A Package has no name of its own, no Selector,
   no kind and no signature. Creating one that exists is the same request arriving twice.

2. **Two names are derived, and they answer different questions.** The **wire name** is the Agent
   type alone: the `PackagesAvailable` map key, the key an Agent reports its `PackageStatuses`
   under, and the value the Client's `[self_update] package` is compared against
   ([ADR-0021](0021-the-client-updates-itself.md)). It must be stable across versions. The
   **display name** is `<agent_type> <version>` — the UI and the log. Using one name in both places
   would break the Client's self-update guard on every release or make the fleet view unreadable.

3. **The hashes are fixed constructions, and both are shown.** The per-package hash is
   SHA-256 over the version's length (u64, little-endian), the version, and the entry's content
   hash; the Platform has no place in it. `all_packages_hash` is SHA-256 over the Agent type's
   length, the Agent type, and that per-package hash. Each entry in the API carries its
   `content_hash` — the exact value an Agent verifies against — and its `package_hash`, the value an
   Agent echoes once in sync.

4. **Every offered Package is top-level.** `PackageType::Addon` is never emitted. The Client's
   refusal of an addon offer stays where it is ([ADR-0019](0019-package-delivery-on-the-agent.md)):
   it defends against a foreign Server, not against this one.

5. **The store's layout is fixed, and no other is read.** A Package is
   `<packages_dir>/<agent_type>@<version>/` holding `package.json` (identity and every entry's
   metadata) and one `<os>-<arch>.bin` per uploaded entry. The Deployments live in
   `<packages_dir>/deployments/`, armed by the same key. A loose file at the top level, or a
   directory without `package.json`, fails startup naming the path — skipping it would open a store
   that merely looks empty.

6. **The Package routes are the pair.** `GET|PUT|DELETE /api/v1/packages/{agent_type}/{version}`
   (`PUT` takes no body); `PUT|DELETE …/entries/{os}/{arch}` with the artifact as the body;
   `PUT …/entries/{os}/{arch}/source` with `{url, sha256, headers}`. `GET /api/v1/packages` lists
   each Package with its entries and the Deployments holding it. A `signature` on an entry upload or
   in a source body is refused (`400`) naming the Deployment route instead. The download is
   `GET /api/v1/packages/{agent_type}/{version}/file?os=…&arch=…` on the Agent plane. A Package has
   no Selector route and no rollout route; the OpenAPI document follows.

7. **The version test reads the claim under the Agent type**, because that is the key an Agent
   reports its status under; [ADR-0027](0027-rollout-and-what-reaches-an-agent.md) clauses 10–13
   are otherwise unchanged.

8. **A Deployment is a name, a Selector, Packages and signatures.** The name — the one human-chosen
   label in the model — follows the grammar a Configuration's name follows: 1–32 lowercase letters,
   digits and `-`, not starting or ending with `-`, no Windows device name. The Selector is
   [ADR-0016](0016-configurations-and-the-rest-api.md)'s equality pairs over the effective
   description, labels included. A Deployment is persisted as
   `<packages_dir>/deployments/<name>.json`, owner-only; a file that does not parse or whose name
   disagrees with its content fails startup naming it. `PUT /api/v1/deployments/{name}` creates it
   with `{selector}`, `PUT …/selector` re-aims it, `DELETE` removes it; saving distributes nothing.

9. **At most one Package per Agent type.** `PUT /api/v1/deployments/{name}/packages/{type}/{version}`
   adds a Package; a second one of a type already held is refused (`409`) naming the one held,
   unless the request says `?replace=true`, which swaps the version the channel holds. Writing the
   held one again succeeds. A Package the store does not hold is `404`. Two of one type would
   collide on the wire key *and* fit the same Agent; refusing at the write turns a resolution-time
   mystery into an error at the moment of the mistake — and, the collision being on the type, which
   is identity, there is no dead end where every write is refused.

10. **A Deployment's Selector is never empty, and no pair has a blank half** — `400` at creation and
    at every edit, with a message naming what to write. An empty Selector is the channel that
    collides with every other, and a forgotten field would silently become the base for the whole
    fleet.

11. **Channels are a partition by a host property, and there is no "everyone".** The Server
    prescribes no key and reserves no word. The key an operator picks says what the partition
    means — `channel` (`stable`, `beta`) for release risk, `region` for where it runs, `tenant` for
    whose it is — and keys compose as further equality pairs. Each names a property of the **host**,
    arriving from provisioning through the Client's `[attributes]` table or as a Server label, which
    moves a host between channels without touching it. An Agent no Deployment claims waits; it is
    the ordinary state of a fresh enrolment, not an error. The fleet view tells apart an Agent in no
    Deployment, one whose Deployment holds nothing it can take (no Package for its type or its
    Platform), one with something waiting, and one in conflict, because the next move differs.

12. **An Agent belongs to at most one Deployment; any overlap is a conflict.** Not the most
    specific, not the newest — none. The Agent's `package_conflict` names every Deployment that
    claims it, and it is offered nothing new until an operator narrows a Selector. There is no
    specificity rule anywhere.

13. **A conflict takes the candidate away, never a standing assignment.** An Agent already rolled
    out to keeps its offer: nothing distributes or un-distributes by itself, and creating an
    overlapping Deployment must not withdraw software from a running host.

14. **The signature lives on the Deployment, per `(Package, Platform)`, and the wire is unchanged.**
    `PUT|DELETE /api/v1/deployments/{name}/signatures/{type}/{version}/{os}/{arch}` with the hex
    Ed25519 signature (as `opamp-package-sign` prints it). A signature for a Package the Deployment
    does not hold is `404`, an empty one `400`; removing a Package from a Deployment takes its
    signatures with it. What an Agent receives in `DownloadableFile.signature` is the signature held
    by the Deployment its assignment was released through, not whichever claims it now. The Server
    does not refuse an unsigned artifact — an unsigned fleet is a legitimate policy — it reports, per
    Package, the platforms the Deployment holds a signature for. The same Package in two Deployments
    is signed in each.

15. **Rollout acts name a Deployment.** `POST /api/v1/deployments/{name}/rollout` releases to every
    Agent it claims and would move (ADR-0027 clauses 9–13), skipping Agents another Deployment also
    claims, and answers `assigned_agents`; a Deployment holding no Package is refused (`409`). The
    per-Agent act takes `{"deployment": "<name>"}`. Both pin as of the press, and an Agent's package
    assignment is one pair — the Deployment it was released through and the Package pinned. A bare
    Package cannot be named in an act: that would bypass the Deployment that supplies the signature.

16. **The per-Agent act refuses to pick a side.** Naming a Deployment while a second one also claims
    the Agent is `409`, as is naming one that does not claim it. Otherwise the conflict is sidestepped
    for good, and the per-Agent path becomes the way into a state the bulk act forbids.

17. **A Deployment freezes exactly what a standing offer travels with.** Once a Deployment has
    released a Package to at least one Agent, the signatures it holds for that Package and its hold
    on that Package (removing it) are refused (`409`). The re-offer gate is the package hash, which
    does not cover the signature: a changed signature would never reach an Agent installing against
    the old one, and a removed one would silently turn a signed rollout unsigned for any Agent that
    has not finished. For the same reason **the Deployment itself cannot be deleted while an
    Agent's assignment names it** (`409`): the offer would stand without its signatures. Rolling
    those Agents out through another Deployment, or deleting the Package, ends the offer first.
    Everything else stays editable: the Selector always; adding a Package for a
    type the channel does not hold; and **swapping the version a channel holds** — the Agents
    already released keep their pinned Package, the new version shows as waiting, and the next press
    moves them. That is how a rollout proceeds.

18. **A Deployment reports three counts.** `claiming_agents` — Agents it claims and no other does;
    zero is the aim mistake worth hunting. `targeted_agents` — of those, the Agents a rollout would
    move. `conflicting_agents` — Agents it matches that another Deployment matches too. The UI shows
    "⚠ n in conflict", the "Roll out (n)" press, "n up to date", or "aims at nobody" accordingly.

19. **The bundled UI has a Deployments tab, and the Packages tab has no aim.** The Deployments tab is
    a master table and a detail form, with the per-row rollout press beside the counts — never in
    the form. The Packages tab carries no Selector, kind or reach; a Package held by no Deployment
    reads "in no deployment". An Agent in no Deployment is shown calmly.

20. **An absent assignment means nothing was rolled out.** An Agent record carrying no assignment
    fields loads as assigned nothing; the Server never invents a rollout at startup. Records are
    written as envelope version 2, and an envelope of another version stops the Server with a
    message saying to clear the agents directory.

**Out of scope:** Deployments carrying Configurations; inequality in Selectors; refusing an
overlapping Selector at write time; whether `[self_update] package` should be renamed, since its
value is an Agent type; addons, until a Client can install one; a retention policy for superseded
Packages.

### The wording the specification needs

[`docs/SPECIFICATION.md`](../SPECIFICATION.md) outranks every ADR
([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules)), so the wording this decision needs is raised here,
to be accepted or amended together with it:

> - **Package** — a versioned, downloadable software artifact an Agent installs, identified by the
>   **Agent type it is built for and its version**; its display name is derived from the two. It is
>   verified against a content hash, and against a signature where one is configured — the
>   signature travelling with the Deployment that offers it. The Server offers Packages; an Agent
>   reports the status of each.
> - **Selector** — the rule by which the Server addresses a **subset** of the fleet for a
>   Configuration or a Deployment. One mechanism with two subjects.
> - **Deployment** — a named set of Packages, aimed at a subset of the Fleet by a Selector and
>   carrying the signature of each Package's artifact. It is the only thing that is rolled out. An
>   Agent belongs to **at most one**: two Deployments matching one Agent is a conflict, and that
>   Agent is offered nothing new until it is resolved.

**Fleet** stays as it stands — all Agents managed by the Server; that the word was taken is why
this object is called a Deployment.

## Alternatives considered

- **Keep a name as an optional field defaulting to the Agent type.** Smallest change; the room it
  leaves is the problem — the degree of freedom that needed a ranking.
- **Identity `(name, version)` with the type as an attribute.** The type decides fit, and an
  attribute is editable; retyping stored bytes to another kind of Agent is what immutable identity
  forecloses.
- **Keep the addon kind, and its byte in the hash, against a future need.** No Client installs an
  addon and no artifact here is one; with nothing deployed the byte buys no stability and leaves an
  unexplained constant in a hash function.
- **Keep specificity and move it to the Deployment.** Preserves the fleet-wide-plus-canary shape and
  the unreadable computation with it; the requirement is a partition.
- **An empty Selector as a catch-all that loses to every other.** A ranking one level deep, makes
  forgetting a field the way to target the whole fleet, and overlaps everything by construction.
- **Inequality in Selectors (`key != value`).** Most expressive; the operator keeps exclusions in
  sync by hand, overlap stops being visible by eye, and it changes the Selector for Configurations
  too. Named as a follow-up if the partition proves too rigid.
- **Naming a Deployment in the per-Agent act resolves the conflict.** Makes the conflict permanently
  liveable; a rule that can be waived per Agent is not a partition.
- **Deployments carry Configurations too.** The tidier end state, and a second lifecycle with its own
  revision model; deferred.
- **Signatures stay on the artifact, and the Deployment names only the required key.** No
  re-signing when a Package moves, but it puts an attribute back on the emptied object and the
  release decision back on the bytes.
- **Keep a reader for older store and record shapes.** A migration with no source is untestable in
  the only way that matters and untrue as documentation.

## Sources / Prior art

- [WSUS update approval](https://learn.microsoft.com/en-us/windows-server/administration/windows-server-update-services/deploy/3-approve-and-deploy-updates-in-wsus)
  — approval binds a concrete update to a **computer group**: membership, never exclusion.
- [Jamf Pro patch policies](https://learn.jamf.com/r/en-US/jamf-pro-documentation-current/Patch_Policies)
  — one version bound to a scope built from groups.
- [Bindplane rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts)
  — deployment starts on an explicit act and names a pinned version.
- [Argo CD manual sync](https://argo-cd.readthedocs.io/en/stable/user-guide/auto_sync/) — drift
  displayed, nothing applied until Sync; the waiting view extended to "in no Deployment".
- [Kubernetes label selectors](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/)
  — set-based selectors have `NotIn` and `DoesNotExist`, and grouping still leans on membership
  labels.
- [OCI Image Index](https://github.com/opencontainers/image-spec/blob/main/image-index.md),
  [Debian repository format](https://wiki.debian.org/DebianRepository/Format) and
  [RPM](https://rpm-software-management.github.io/) — the name *is* what the software is; nothing
  carries a second identity beside it.
- [OpAMP specification § Packages (`v0.19.0`)](https://github.com/open-telemetry/opamp-spec/blob/v0.19.0/specification.md)
  — `PackagesAvailable` maps a name to one version and hash *for this Agent*, carries `signature`
  per downloadable file, and expects "normally only one top-level package".

## Consequences

- Positive: "what is this artifact" is readable off one object with two fields, and "which artifact
  does this host get" off one Deployment. The ranking disappears — specificity, the version
  tie-break and the unbreakable tie. A collision between two Deployments is **refused and named**
  rather than silently resolved; that is a discipline, not a proof that one Deployment matches.
- Positive: a channel is something an operator can name, sign and release in one act.
- Negative / trade-offs: **there is no "roll out to everyone".** A fleet-wide delivery needs every
  Agent to carry the same channel value, and a fresh host belongs to no Deployment until labelled or
  provisioned with one — a discipline for provisioning.
- Negative / trade-offs: two builds of one Agent type at one version cannot coexist; they must differ
  in type or version. The same Package in two Deployments is signed in each, although the signature
  over the same bytes with the same key is identical. An operator who set `[self_update] package` to
  something other than an Agent type sees that Client refuse the offer on its fleet row.
- Follow-ups: whether a Configuration should be aimed by the Deployment that already aims its
  Agent's Package; inequality in Selectors; refusing an overlapping Selector at write time once a
  fleet is large enough that a conflict is expensive to notice.

## Enforcement

- `crates/fleet-server/src/deployments.rs`:
  `two_deployments_matching_one_agent_are_a_conflict_that_names_them`,
  `a_narrower_selector_does_not_win_over_a_wider_one`, `an_agent_no_ring_claims_is_not_a_conflict`,
  `a_deployment_must_name_the_ring_it_aims_at`, `a_deployment_holds_one_package_per_agent_type`,
  `a_signature_needs_its_package_and_leaves_with_it`, `a_deployment_survives_a_reopen`,
  `the_selector_stays_editable_and_keeps_what_the_ring_holds`,
  `an_unreadable_file_fails_the_open_and_names_it`, `the_store_and_its_files_are_owner_only`.
- `crates/fleet-server/src/packages.rs`: `only_the_package_its_ring_holds_is_a_candidate`,
  `a_store_in_an_older_layout_refuses_to_open_and_names_what_is_in_the_way`,
  `identity_tokens_are_bounded`.
- `crates/fleet-server/tests/packages.rs`: `a_selector_aims_a_rollout_at_part_of_the_fleet`,
  `a_canary_ring_is_a_selector_aim_and_two_acts`,
  `an_agent_two_rings_claim_is_offered_nothing_and_the_view_says_why`,
  `a_label_aims_a_set_at_part_of_the_fleet`, `a_deployment_without_a_selector_is_refused`,
  `a_deployment_holds_one_uploaded_package_per_agent_type`,
  `a_deployment_carries_the_signature_and_says_what_is_unsigned`,
  `a_deployments_aim_is_editable_and_deleting_it_is_its_own_act`,
  `the_signature_an_agent_is_offered_comes_from_its_deployment`,
  `a_signature_on_the_artifact_upload_is_refused_by_name`,
  `a_conflict_takes_the_candidate_away_and_leaves_the_assignment_standing`,
  `the_per_agent_act_refuses_to_pick_a_side`, `a_ring_freezes_what_it_has_released`,
  `a_deployment_that_released_a_package_refuses_to_be_deleted`,
  `a_packages_entry_shows_the_hash_an_agent_verifies_against`,
  `the_fleet_view_tells_no_ring_apart_from_a_ring_with_nothing_for_this_agent`,
  `a_set_says_how_many_agents_it_reaches`.
- `crates/fleet-server/tests/rest_api.rs`: `the_openapi_document_describes_the_contract` (the Deployment
  routes are in the contract, and no Selector or rollout route on a Package is).
- `crates/fleet-server/src/fleet.rs`: `a_record_without_assignments_loads_assigned_to_nothing`;
  `crates/fleet-server/src/agent_store.rs`: `a_pre_adr_0027_record_restores_with_no_assignments`.
- `crates/fleet-agent/src/supervisor/agent.rs`: `an_addon_package_is_refused_instead_of_overwriting_the_binary`.
