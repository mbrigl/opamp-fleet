# ADR-0026: A Deployment is the one thing aimed and rolled out, and a rollout pins what it releases

- **Status:** 🟢 accepted
- **Date:** 2026-10-17
- **Deciders:** Markus Brigl
- **Applies to:** `crates/fleet-server/src/configs.rs`, `crates/fleet-server/src/deployments.rs`, the assignments, rollout acts and deletions in `crates/fleet-server/src/fleet.rs`, the matching and offer functions of `crates/fleet-server/src/packages.rs`, the persisted assignment in `crates/fleet-server/src/agent_store.rs` and its adapters, the Configuration, Deployment, Package-deletion and rollout routes of `crates/fleet-server/src/api.rs` and the OpenAPI document, the Configurations, Deployments and Agents tabs of the bundled UI in `crates/fleet-server/static/index.html`, the Client's check of an offer for its own package, and every document that tells an operator how to aim or roll out a Configuration or a Package

## Context

Two questions decide what an Agent runs: **when** a saved Configuration or Package reaches it,
and **which** Package can reach it at all.

*When.* A Server that offers whatever is stored — or whatever was once marked published —
converges every matching Agent automatically: the WebSocket loops wake, polling recomputes each
offer on every exchange, and an Agent that enrols next week takes the content without anyone
deciding that it should. The operator requirement is stricter than any standing state can
express: saving must never distribute, not on first save and not on a later edit; the fleet view
must show per Agent what could be rolled out to it; distribution happens only when the operator
says so — per Agent, or for every Agent a Deployment currently claims; and Packages and
Configurations follow one model. A standing property of a resource cannot hold for one Agent and
not another, and keeps distributing to Agents that appear later.

*Where.* The specification makes the Deployment the one thing that is aimed and rolled out: a
named release of Packages, at most one per Agent type, and Configurations, aimed at a subset of
the Fleet by a Selector (G-9). A Configuration is content only. An Agent belongs to at most one
Deployment, and an Agent in no Deployment is offered nothing. The human settled these points:

- **No fleet-wide base Configuration.** A need for one is solved differently.
- **One rollout act per Deployment, with no option to release only its Configurations or only
  its Packages.** Only what changed reaches an Agent anyway: the Configuration revision hash and
  the Package pin are compared per part, and the wire keeps `AgentRemoteConfig` and
  `PackagesAvailable` apart, each behind its own hash gate (G-3).
- **A Configuration keeps its Agent type and its role,** so one Deployment serves several Agent
  types and top-level and supplementary content.
- **A Configuration may sit in several Deployments,** as a Package may.
- **One deletion rule:** a resource that an assignment pins cannot be deleted.

*Which.* A Package fits an Agent by type and Platform and is aimed at it by a Deployment
([ADR-0027](0027-packages-signatures-and-delivery.md)). Fit and aim alone would count Agents that
already run the version, propose it to Agents that need nothing, and let the act aim backwards
under the same label as a forward one — the shape in which a compromised Server pushes a
known-vulnerable build, and one the Client's own install already refuses, because an Ed25519
signature carries no version ordering and an old release stays validly signed forever.

An Agent reports two versions, and they can contradict each other.
`PackageStatus.agent_has_version` is derived from what an install once wrote — for the Client
`<state_dir>/installed-package.json` — and that record outlives the binary it describes: a staged
update that did not take, a host reinstalled from an older artifact, a state directory restored
beside a downgraded binary. The Baseline defines the field as *"the version of the package that
the Agent **has**"*, which *"MUST be empty if the Agent does not have this package"* — a
statement about the present. `service.version` is the running program's own statement about
itself. Clients that do not report their own package version can state only the latter.

Prior art draws the same line. In Elastic Fleet an Agent is enrolled in exactly one Agent policy,
and the policy's revisions carry its configuration and a target version. Bindplane aims
configurations by labels and rolls a new version out explicitly.

The decision serves G-1 (the loop closes), G-3 (no redundant reconfiguration), G-9 (a subset via a
Deployment's Selector), G-10 (software as well as configuration) and Q-6 (no downgrade).

## Decision

We will make the Deployment the one object that is aimed and released — a Selector, at most one
Package per Agent type, and any number of Configurations — make its rollout the one act that
distributes and pins per Agent what it released, let a Package become a candidate for an Agent
only when it moves the Agent forward from the version it runs, and refuse to delete any
Configuration, Package or Deployment that an assignment pins.

1. **Saving only saves.** A Configuration holds one revision, the saved one; there is no draft
   and no published state for anything. A `PUT` on a resource — a Configuration, a Package, an
   entry — changes what *could* be rolled out and never what *is*. A Selector edit, a label move
   and an enrolment likewise change only what is proposed; no WebSocket loop wakes for any of
   them.

2. **A Configuration is `{name, service_name, role, body}`.**
   - The body is the Managed Process's own format, and the Server never interprets it. A body
     that is empty or only whitespace is refused. Line endings are normalised to `\n`, and a
     missing final newline is added.
   - `service_name` is required: a Configuration is written for one Agent type, and one without
     it is refused (`400`), and a stored one fails startup, naming the file. `role` is optional;
     unset, it is absent from the stored JSON and from responses.
   - A Configuration has no Selector.

3. **A Selector is equality, AND, over the effective description, and it aims a Deployment.**
   - It is a string-to-string map. An Agent matches when every pair equals a string-valued entry
     of its effective description, whose content
     [ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md) clause 18 states.
   - An array-valued attribute (`host.ip`) is displayed but never matched.
   - There are no set-based operators. They would extend the same field compatibly.
   - A Deployment's Selector is never empty
     ([ADR-0027](0027-packages-signatures-and-delivery.md)), so no Selector reaches every Agent
     by default.

4. **The type is fitted inside the Deployment.**
   - A Configuration reaches only the Agents of its Deployment whose reported `service.name`
     equals its `service_name`, compared raw.
   - Content for several Agent types, a certificate bundle for instance, is one Configuration per
     type.
   - An Agent that reports no `service.name` receives no Configuration.
   - Packages are fitted by type and Platform as
     [ADR-0027](0027-packages-signatures-and-delivery.md) says.

5. **A Deployment is a name, a Selector, Packages and Configurations.**
   - The name follows the grammar a Configuration's name follows. The Selector is clause 3's.
   - The Packages are at most one per Agent type
     ([ADR-0027](0027-packages-signatures-and-delivery.md)).
   - The Configurations are named, in any number. A Configuration may sit in several
     Deployments.
   - `PUT /api/v1/deployments/{name}` creates a Deployment with `{selector}`, `PUT …/selector`
     re-aims it, and `DELETE` removes it (clause 12).
   - `PUT|DELETE …/packages/{agent_type}/{version}` holds or drops a Package.
     `PUT|DELETE …/configurations/{configuration}` holds or drops a Configuration; one the Server
     does not hold is `404`.
   - A Deployment is persisted through the store
     [ADR-0029](0029-the-servers-state-and-its-stores.md) names: owner-only, one JSON file per
     Deployment in the filesystem adapter, one row in the PostgreSQL adapter. A stored Deployment
     that does not parse, or whose name disagrees with its content, fails startup naming it.
     Saving distributes nothing.

6. **The assignment is per Agent, persisted, and pins one release.**
   - Each Agent's record ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)) stores what
     its last rollout released to it:
     - the Deployment it was released through;
     - the Package pinned, if any;
     - for every Configuration released, its name and the content hash of the revision released.
   - Operator intent survives a Server restart.
   - The Configuration store retains every revision an assignment still references, and collects
     the others when the last reference goes.
   - Editing a Configuration or a Deployment after a rollout changes nothing on any Agent. The
     fleet view shows the change waiting.

7. **Offers are composed from assignments only.** An Agent's composed config map and its hash,
   and its package offer and `all_packages_hash`, are computed over **that Agent's** assignments
   — never over stored content and never over a fleet-wide aggregate, which would re-offer on
   every exchange what this Agent is never given. The hash gates (no redundant reconfiguration,
   no re-offer) work unchanged over the assigned content. An assigned Package is offered as the
   entry for the Platform the Agent reports; an Agent reporting a Platform the assigned release
   does not hold is offered nothing rather than something else. Matching survives intact, but it
   only computes the **candidate**: what an act would assign if the operator rolled out now.

8. **The fleet view shows, per Agent, what is waiting.** For every Agent the Server derives
   candidate against assignment and reports each difference: a Configuration not yet rolled out
   (`new`) or whose saved revision differs from the assigned one (`update`); a Package not yet
   rolled out or a different version than the one assigned. The gap between "could run" and "was
   released" is shown and never acted on by the Server alone.

9. **Rollout acts name a Deployment, and only a Deployment.**
   - `POST /api/v1/deployments/{name}/rollout` releases to every Agent the Deployment claims and
     would move, and skips Agents another Deployment also claims. It answers `assigned_agents`.
   - `POST /api/v1/agents/{instance_uid}/rollout` releases the Deployment that claims that Agent.
     An empty body or `{"deployment": "<name>"}` naming that Deployment are both accepted. Naming
     another Deployment, or an Agent in no Deployment or in conflict, is `409`.
   - A rollout **replaces** the Agent's assignment. It pins two things as of the press: the
     Package the Deployment holds for the Agent's type, if it is a candidate by
     [ADR-0028](0028-package-versions-high-water-mark-and-floor.md) and clauses 14–17; and every
     Configuration of the Deployment that fits the Agent's type, at its saved revision. A
     Configuration no longer in the Deployment leaves the Agent's map. The hash gates send only
     what changed.
   - Where the Deployment holds a Package for the Agent's type that is no candidate — the Agent
     already runs it, or it would move the Agent backwards — the Package the assignment already
     pins stays pinned. A press must not drop the pin of what an Agent installed (clause 20).
     Where the Deployment holds no Package for the Agent's type, the pin goes.
   - A release that holds neither a Package nor a Configuration for the Agent leaves it with no
     assignment: it is offered nothing and keeps running what it runs. Nothing it was given is
     removed, so the fleet view shows what it keeps as `kept`, never as `remove` (clause 24).
   - The per-Agent act that would change nothing because the Package held for the Agent is no
     candidate is refused (`409`) with the version reasoning clause 19 requires.
   - Two cases are refused (`409`) for every act, and nothing is released:
     - a Deployment holding no Package and no Configuration;
     - a Deployment in which an entry of a Package it holds carries no signature
       ([ADR-0027](0027-packages-signatures-and-delivery.md)). The message names each such
       Package by its display name and the platforms it is unsigned for.
   - `POST /api/v1/configurations/{name}/rollout` does not exist (`404`). A bare Package or
     Configuration cannot be named in an act.

10. **An Agent that appears later waits.** A newly enrolled Agent, or one that starts matching
    after a Selector edit or a label move, is assigned nothing until an operator's act; it
    surfaces in the fleet view with everything it could receive marked waiting. Widening an aim
    is never a distribution event.

11. **A Deployment freezes nothing that a standing offer travels with.**
    - An assignment pins the Package and the revisions, and the signature is the entry's
      ([ADR-0027](0027-packages-signatures-and-delivery.md)). So the Selector, the Packages and
      the Configurations of a Deployment stay editable in every state.
    - Swapping the version a Deployment holds, or the revision of a Configuration, shows as
      waiting for the Agents already released, and the next press moves them.
    - Deleting the Deployment itself is clause 12's rule.

12. **Nothing an assignment pins can be deleted, and taking content away never uninstalls.**
    - Deleting a Configuration, a Package or a Deployment is refused (`409`) while any Agent's
      assignment names it. The message says how many Agents pin it.
    - The way to delete is to roll those Agents out through a Deployment without it, or to forget
      them ([ADR-0024](0024-the-fleet-record-of-agents-and-hosts.md)).
    - A deleted Configuration or Package leaves every Deployment that held it.
    - Nothing recalls what an Agent installed: the protocol has no revert, and the Agent keeps
      running it. A rollout that drops a Configuration from an Agent's map and keeps another one
      is an active change: the smaller map is applied. One that keeps nothing for the Agent
      leaves it with no assignment and applies nothing (clause 9).

13. **Immutability follows the assignment.** A Package assigned to at least one Agent has
    immutable entries: uploading, referencing or deleting an entry is refused (`409`, saying to
    create the next version as a new Package and roll that out), and an upload is refused before
    its bytes are streamed. A Package assigned to nobody is freely editable.

14. **What the Agent runs decides, in both directions.** Where the Agent reports a
    `service.version` among its identifying attributes that parses as a version
    ([ADR-0033](0033-the-products-version.md)), the Package's version must be **strictly
    greater** than it by SemVer precedence, build metadata ignored. Equal does not match: a
    Package the Agent already runs would reach it with nothing.

15. **The claim is not consulted where the running version is.** A non-empty
    `agent_has_version` neither admits a Package the running version refuses nor refuses one it
    admits. A record about the past does not overrule, and has no veto over, a statement about
    the present — so a host whose package status claims a version its own program denies running
    is reachable in either direction.

16. **Where the running version cannot be ordered, the claim is the whole test.** A
    `service.version` that does not parse (`1.19`, `24.04.1`) says nothing at all. The Package is
    then held against the non-empty `agent_has_version` the Agent reports under the Package's
    wire key: strictly greater to match, and a claim that cannot itself be ordered **refuses** —
    the safe direction for a claim about that very Package: what cannot be ordered must not be
    installed over what is running.

17. **An Agent that reports neither has nothing to be greater than** — the first rollout, matched
    on fit and aim alone. An **empty** `agent_has_version` is that case, not an unorderable one:
    it is how a Package offered, pending or downloading but not yet installed is reported.

18. **The Client applies the same rule to an offer it receives.** For the Package that carries the
    Client itself ([ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md)), *already
    installed* means the version this process runs is the offered one — not that a recorded
    package hash matches. A record whose hash equals the offer does not end the offer on a host
    that is not running it.

19. **Every consumer of matching applies the same test.** The candidate resolution, the reach
    count and the rollout act test alike, so the count, the proposal and the press cannot
    disagree. A per-Agent refusal names the version it compared against and whether that was the
    running version or the claim — and, where both were reported and disagreed, that the claim
    was not consulted. The bulk act skips an Agent it would not move.

20. **The assignment path is version-blind.** An offer composed from an assignment is never
    filtered by the version test: an offer is the Agent's desired state, and dropping an installed
    Package from it would tell the Agent the Package is no longer wanted. Matching decides what
    *may become* an assignment; it never edits one that exists.

21. **Rollback is not a rollout.** No act moves an Agent to an older version than it runs. An
    operator who wants an Agent back on its predecessor has the Agent's retention window
    ([ADR-0027](0027-packages-signatures-and-delivery.md)) and, for the Client itself, the
    pointer move of [ADR-0018](0018-the-client-updates-itself-from-a-signed-package.md); a bad
    version is otherwise taken back by publishing the old content as a new, greater version. A
    deliberate Server-driven downgrade is left undecided: it is a separate act with its own
    authorisation question, and it must never be the same press as a rollout.

22. **Zero has more than one meaning, and the view says which.** Where the Server reports whom a
    rollout would reach, it reports both the Agents the resource fits and aims at, version-blind,
    and the Agents an act would actually move. Zero of the first is the aim mistake worth hunting
    (a misspelled type, no entry for any reported platform, an aim no Agent matches); zero of the
    second with Agents aimed at means everyone is up to date — stated plainly, not as a warning.
    The count beside a rollout button is exactly what that button would move.

23. **The Configuration routes.**
    - `GET /api/v1/configurations` lists them in name order, each with the Deployments that hold
      it.
    - `GET /api/v1/configurations/{name}` returns one, or `404`.
    - `PUT /api/v1/configurations/{name}` creates or replaces one from a body of `body`, `role`
      and `service_name`. Unknown fields are refused, and a `selector` is refused (`400`) with a
      message naming the Deployment as where to aim. The answer is `200` with the stored view,
      `400` for an invalid name or an empty body, or `500` when the Configuration cannot be
      persisted.
    - `DELETE /api/v1/configurations/{name}` answers `204`, `404`, or `409` under clause 12.

24. **The bundled UI splits along the same seam.**
    - The Configurations tab is a master table and a detail form of content: name, Agent type,
      role, body, and the Deployments that hold each Configuration. It has no Selector and no
      rollout press.
    - The Deployments tab is a master table and a detail form. The form holds the Selector, the
      Packages and the Configurations, each added and removed in place.
    - The per-row rollout press sits beside the Deployment's counts, never in the form.
    - Each Agent row shows what is rolled out to it and what waits, per Configuration and for the
      Package: `new`, `update`, `remove` — dropped from a release that keeps something else, so the
      smaller assignment is applied — or `kept` — the release holds nothing for the Agent, so the
      press leaves it with no assignment and it keeps running what it runs. It also shows the
      Deployment the press releases through when the assignment names another one, even where the
      content stays the same. Whatever makes a press move the Agent shows here, with a per-Agent
      *roll out* press, so the count beside a Deployment's press and the rows agree.
    - *Save*, *Upload* and *OK* never distribute.

**Out of scope:** a fleet-wide base Configuration, and any "everyone" Deployment; releasing only
a Deployment's Configurations or only its Packages; parameters and blocks in a Configuration
(G-18, a decision of its own); where Deployments, Configurations and assignments are stored
([ADR-0029](0029-the-servers-state-and-its-stores.md)); what a Package and its entries are
([ADR-0027](0027-packages-signatures-and-delivery.md)); an opt-in convergence policy for Agents
that appear later; a batched or paused progressive walk; an audit trail of who rolled out what
(the operator plane has no per-operator identity); an operator override to re-install a version
the test holds back; a Server-driven downgrade.

## Alternatives considered

- **A publication gate on the resource** (draft until published, or a draft and a published
  revision). It answers "may the fleet have it?" once, for the whole fleet, forever — it cannot
  hold for one Agent and not another, keeps distributing to Agents that appear later, and leaves
  two lifecycles to hold in mind. Keeping it and adding a per-Agent gate behind it answers no
  question the assignment does not.
- **A standing assignment — "this Agent follows this resource".** Saving would then distribute to
  every assigned Agent, which the requirement forbids; this is Flux's suspend/resume, a gate on
  time, not content. WSUS, Jamf, Bindplane and Chef environments all pin instead.
- **A standing "roll out to all matching, including future matchers" flag.** Publication under a
  new name: an Agent enrolling later takes content nobody released to it.
- **Cohort-level approval only.** Cannot express "this one canary host first".
- **Configurations that carry their own Selector and are rolled out by themselves, beside
  Deployments for Packages.** Two targeting rules, three rollout presses (Agent row, Deployment
  row, Configuration form) and two answers to "what does this host run", with an empty Selector
  reaching every Agent. A Deployment carrying Configurations gives one aim, one press and one
  answer.
- **A separate policy object for Configurations beside the Deployment for Packages.** Each would
  get the at-most-one rule, but an Agent would then belong to two partitions, which can disagree
  about it.
- **Exclusive assignment — one Configuration per Agent (Elastic Fleet's one policy).** Forbids
  composing several named entries, which the protocol's config map provides for free; a
  Deployment holds any number of Configurations instead.
- **A Configuration without an Agent type reaches every Agent of its Deployment** (unset means
  every type). It keeps cross-type content, a certificate bundle for instance, in one
  Configuration. But a body written for one agent's language reaches agents of another, which is
  rarely what an operator meant; the human made the type required.
- **Express the type only as a Selector pair.** Zero cost, but the failure is fleet-wide churn
  behind one forgotten pair, and the UI cannot guide what it cannot tell apart.
- **Pattern or prefix matching on the type** (`otelcol*`). Two types are two Configurations;
  there is no canonical set of types to pattern against.
- **A rollout option that releases only the Configurations or only the Packages.** The hash gates
  already send only what changed. The option can be added to the act without changing the model.
- **A fleet-wide base Configuration, as an empty Selector or an implicit "everyone".** It brings
  back the ranking between Deployments that
  [ADR-0027](0027-packages-signatures-and-delivery.md) removes. The human chose to solve such a
  need differently.
- **Cascading deletion of Configurations and Packages through the assignments.** It is cheaper
  to use, but a deletion silently changes what running Agents receive. The protection a
  Deployment has applies to all three.
- **Freeze a Deployment's hold on a released Package and the signatures it holds.** It protects
  a standing offer from losing its signatures where the signatures sit in the Deployment. They
  are the entry's, so nothing a standing offer travels with sits in the Deployment.
- **Pin the Deployment and resolve its content at offer time.** This would need less in each
  record. But editing a Deployment would then change running Agents without a press, against the
  explicit act.
- **Apply the version test to the reach count only.** The count would no longer describe the
  button beside it: an operator reading "0" could still press it and change the fleet.
- **Let `Equal` match**, as a repair for a failed install. A failed install is not repaired by
  re-delivering bytes the Agent has; the assignment is in place and the Agent's own retention and
  rollback govern what follows. Every up-to-date Agent would be back in every count.
- **Compare against `agent_has_version` alone.** Cannot reach a Client that reports no package
  version, and believes a claim the Agent's own program contradicts.
- **The lower of the two versions, and never move under a claim.** Refuses a Package below a
  claim before the running version is read, stranding a host whose claim is wrong upwards.
- **Let the running version decide only for the Client's own Package.** The narrower change; it
  makes the matching rule two rules keyed on which Package is matched, and every new agent kind
  arrives with the question of which it falls under.
- **Fix the report instead of the rule** (the Client reports the version that came up rather than
  the one staged). Worth doing, but it repairs only claims this Client writes; a reinstall or a
  restored state directory still produces a claim no Client-side fix reaches.
- **Fail closed on an unorderable `service.version`.** Turns a program's numbering habit into a
  fleet that cannot deliver to it, with a symptom that reads like an aim mistake.
- **A per-Package "allow downgrade" flag**, or an operator override on the act. A standing second
  mode on the resource where the rule was asked for; an override is unavailable to the bulk act
  and the count. A downgrade, if wanted, is its own act.
- **Persist a Deployment as JSON files of its own, outside the port
  [ADR-0029](0029-the-servers-state-and-its-stores.md) puts every record behind.** A PostgreSQL
  deployment would then keep its Deployments on the host while every other record lives in the
  database. A Deployment is persisted through that store's port (clause 5).

## Sources / Prior art

- [WSUS update
  approval](https://learn.microsoft.com/en-us/windows-server/administration/windows-server-update-services/deploy/3-approve-and-deploy-updates-in-wsus)
  — nothing reaches clients until an explicit approval, which binds a concrete update; "All
  Computers" is the widest form of the same act.
  [WSUS updates operations](https://learn.microsoft.com/en-us/windows-server/administration/windows-server-update-services/manage/updates-operations)
  — the population read is the computers that **need** an update.
- [Argo CD manual sync](https://argo-cd.readthedocs.io/en/stable/user-guide/auto_sync/) and
  [selective sync](https://argo-cd.readthedocs.io/en/stable/user-guide/selective_sync/) — drift
  displayed as OutOfSync, nothing applied until Sync; the model for clause 8.
- [Bindplane rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/rollouts)
  — edits create a version, collectors keep the old one, deployment starts on an explicit act.
- [Bindplane: Fleets](https://docs.bindplane.com/feature-guides/deployment-and-management/fleets)
  — Kubernetes-style label selectors matching agent labels; and
  [progressive rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/progressive-rollouts)
  by label-selected stages.
- [Bindplane — Bring Your Own
  Collector](https://docs.bindplane.com/feature-guides/deployment-and-management/bring-your-own-collector)
  — the Agent type as a first-class property that what an agent may receive hangs off.
- [Jamf Pro patch
  policies](https://learn.jamf.com/r/en-US/jamf-pro-documentation-current/Patch_Policies)
  — new versions reported without distribution; deployment binds one version to a scope.
- [Flux Kustomization suspend](https://fluxcd.io/flux/components/kustomize/kustomizations/) and
  [`kubectl rollout pause`](https://kubernetes.io/docs/reference/kubectl/generated/kubectl_rollout/kubectl_rollout_pause/)
  — the time-gated counter-model.
- [Grafana Fleet
  Management](https://grafana.com/docs/grafana-cloud/send-data/fleet-management/set-up/configuration-pipelines/)
  — the weak-gate counter-model: activation reaches all matching collectors at once.
- [Elastic Agent policies](https://www.elastic.co/docs/reference/fleet/agent-policy) — the
  exclusive-assignment model; Elastic Fleet's Agent upgrades and the policy's
  `required_versions` (target version and percentage, generally available since 9.1) —
  <https://www.elastic.co/guide/en/fleet/current/upgrade-elastic-agent.html>,
  <https://www.elastic.co/docs/api/doc/serverless/operation/operation-put-fleet-agent-policies-agentpolicyid>.
- [Kubernetes labels and
  selectors](https://kubernetes.io/docs/concepts/overview/working-with-objects/labels/)
  — the equality-based and set-based grammar; this decision takes the equality subset.
- [Google Update
  `RollbackToTargetVersion`](https://chromeenterprise.google/policies/device-rollback-to-target-version/)
  — with the policy unset, "installs that have a version higher than that available will be left
  as-is".
- [OpAMP specification](https://github.com/open-telemetry/opamp-spec/blob/main/specification.md)
  — `PackagesAvailable` is per Agent, the offered `remote_config` is the Server's composition,
  hash comparison is the anti-redistribution primitive; `AgentRemoteConfig` with `config_hash`
  and `PackagesAvailable` with `all_packages_hash` are two independent messages;
  `PackageStatus.agent_has_version` as quoted in Context; an agent package is installed "either
  to upgrade it to a newer version or to downgrade it to an older version".
- [Kubernetes API
  conventions](https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md)
  — `status` is "the most recent observations of actual state"; a controller reconciles against
  the observation, not a record of intent.
- OpenTelemetry semantic conventions, `service.version` — "the version string of the service API
  or implementation".
- The working note on the Server's data model, in which the human chose each point of the
  Deployment model.

## Consequences

- Positive: one way to aim, one press, and one answer to "what does this host run". A Package and
  the Configuration that needs its version arrive in the same release.
- Positive: nothing distributes by side effect — not a save, a Selector edit, a label move or an
  enrolment. The canary workflow is "roll out to one Agent, watch it, then roll out to all".
- Positive: no deletion changes what a running Agent receives.
- Positive: the count means what it says, the proposal is only ever work, and neither the view
  nor the act moves an Agent backwards by the press that moves it forward — measured against what
  the Agent runs. A stale claim does not strand a host in either direction.
- Negative / trade-offs: every new Agent needs an operator's press before it runs anything, and
  the Deployment's press must be repeated after new Agents appear. An Agent in no Deployment
  receives no Configuration either; a fresh enrolment needs a Label, an attribute or a subject
  value that places it in a Deployment.
- Negative / trade-offs: deleting something in use takes two steps, a rollout without it and then
  the deletion.
- Negative / trade-offs: per-Agent state diverges across the fleet by design — ten Agents on three
  pinned revisions is an intended state the view must keep legible.
- Negative / trade-offs: a Managed Process numbered above its Package can be moved backwards — a
  Collector reporting `0.98.0` while its status claims `2.0.0` matches a Package at `1.5.0`. What
  limits it is that nothing moves on its own. Conversely, a program self-reporting a number above
  its Package is unreachable by that Package's number until the Package is re-created at the
  program's number ([ADR-0028](0028-package-versions-high-water-mark-and-floor.md)). For a
  program that reports no `service.version`, the claim decides.
- Negative / trade-offs: a program already running a version cannot be replaced by the fleet's
  Package of that same version; adopting it means publishing under the next version.
- Follow-ups: an opt-in convergence policy for late Agents; a progressive walk; an audit trail
  once the operator plane has an identity; a diff view between an Agent's assigned revision and
  the saved one; the Client reporting the version that came up after a self-update rather than
  the staged one; showing the package status and the reported `service.version` side by side
  where they disagree; an operator override for a genuine reinstall; a per-part rollout option,
  if one is ever needed.

## Enforcement

- [`crates/fleet-server/src/configs.rs`](../../crates/fleet-server/src/configs.rs):
  `a_saved_configuration_reaches_nobody_without_an_assignment`,
  `an_assignment_pins_a_snapshot_and_later_edits_wait`,
  `retain_only_collects_unreferenced_revisions`, `a_revision_reaches_only_agents_of_its_type`,
  `an_agent_without_a_type_receives_no_configuration`,
  `candidates_are_the_held_configurations_that_fit`, `the_store_refuses_a_revision_without_a_type`.
- [`crates/fleet-server/src/deployments.rs`](../../crates/fleet-server/src/deployments.rs):
  `a_deployment_holds_configurations_beside_its_packages`,
  `every_selector_pair_must_equal_a_reported_attribute`, `non_identifying_attributes_match_too`;
  [`fs/deployments.rs`](../../crates/fleet-server/src/fs/deployments.rs):
  `a_deployment_survives_a_reopen`;
  [`fs/configs.rs`](../../crates/fleet-server/src/fs/configs.rs):
  `a_file_in_a_retired_shape_refuses_to_open_and_names_it`,
  `a_stored_configuration_without_a_type_refuses_to_open_and_names_it`;
  [`fs/agents.rs`](../../crates/fleet-server/src/fs/agents.rs):
  `a_record_with_the_old_assignment_shape_fails_the_load_by_name`,
  `a_record_without_an_assignment_restores_with_none`.
- [`crates/fleet-server/src/fleet.rs`](../../crates/fleet-server/src/fleet.rs):
  `rollout_acts_assign_and_a_late_agent_waits`,
  `a_deployment_releases_its_package_and_the_fitting_configurations`,
  `a_second_press_sends_only_the_part_that_changed`,
  `a_configuration_the_deployment_dropped_leaves_the_map_on_the_next_press`,
  `a_release_holding_nothing_for_an_agent_keeps_what_it_runs`,
  `a_new_claiming_deployment_with_the_same_content_waits_in_the_view`,
  `nothing_an_assignment_pins_can_be_deleted`,
  `the_per_agent_act_refuses_without_exactly_one_claiming_deployment`,
  `editing_a_deployment_changes_no_offer_until_the_next_press`,
  `a_record_without_assignments_loads_assigned_to_nothing`.
- [`crates/fleet-server/src/packages.rs`](../../crates/fleet-server/src/packages.rs):
  `a_saved_set_reaches_nobody_without_an_assignment`,
  `fits_agent_checks_fit_but_neither_aim_nor_the_ranking`,
  `fits_agent_refuses_what_is_not_an_upgrade`, `a_set_that_is_no_upgrade_is_no_candidate`,
  `a_set_is_held_against_the_version_an_agent_reports_running`,
  `the_version_an_agent_runs_wins_over_the_version_it_claims`,
  `a_claim_the_running_program_denies_no_longer_holds_the_set_back`,
  `a_claim_above_the_set_no_longer_holds_it_back_either`,
  `a_program_version_nothing_can_order_leaves_the_set_reaching`,
  `the_aggregate_hash_is_per_agent_and_follows_the_assignment`.
- [`crates/fleet-server/tests/rest_api.rs`](../../crates/fleet-server/tests/rest_api.rs):
  `a_configuration_waits_until_it_is_rolled_out`, `an_agent_that_appears_later_waits`,
  `a_label_moves_an_agent_into_a_rollout_ring`,
  `a_configuration_takes_no_selector_and_has_no_rollout_of_its_own`,
  `a_configuration_without_an_agent_type_is_refused`,
  `a_typed_configuration_reaches_only_agents_of_its_type`,
  `a_pinned_configuration_or_deployment_is_not_deleted`,
  `the_per_agent_act_without_one_claiming_deployment_is_a_conflict`,
  `the_ui_edits_configurations_as_content_and_deployments_hold_them`.
- [`crates/fleet-server/tests/packages.rs`](../../crates/fleet-server/tests/packages.rs):
  `a_set_reaches_an_agent_only_as_an_upgrade`,
  `a_set_waits_until_rolled_out_and_is_immutable_while_assigned`,
  `the_act_names_the_version_it_releases`,
  `the_aggregate_hash_an_agent_echoes_is_the_one_it_was_offered`,
  `a_deployment_that_released_a_package_refuses_to_be_deleted`,
  `a_released_package_freezes_its_entries_and_not_the_ring`,
  `a_deleted_package_leaves_every_deployment_with_its_signatures`.
- [`crates/fleet-server/tests/ws_transport.rs`](../../crates/fleet-server/tests/ws_transport.rs):
  `selectors_target_a_subset_and_compose_named_entries`;
  [`http_transport.rs`](../../crates/fleet-server/tests/http_transport.rs):
  `a_selector_aims_a_configuration_at_part_of_the_fleet`.
- [`crates/fleet-agent/src/supervisor/agent.rs`](../../crates/fleet-agent/src/supervisor/agent.rs):
  `the_clients_own_offer_is_settled_by_the_version_it_runs_not_by_a_recorded_hash`;
  [`crates/fleet-agent/src/update/installer.rs`](../../crates/fleet-agent/src/update/installer.rs):
  `install_refuses_a_downgrade`.
