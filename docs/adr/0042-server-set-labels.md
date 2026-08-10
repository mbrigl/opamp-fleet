# ADR-0042: The Server labels an Agent — rollout rings that are not a file on the host

- **Status:** 🟢 accepted
- **Date:** 2026-08-09
- **Deciders:** Markus Brigl

## Context

[ADR-0017](0017-selector-targeted-packages.md) aimed packages at part of a fleet so that a rollout
could be tried on a few hosts first, and [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md)
did the same for Configurations. Both match a Selector against the attributes an Agent **reports**,
and the attribute a staged rollout actually wants — `rollout = "canary"` — is one an operator invents.
Today it can only be invented in one place: the `[attributes]` table in `client.toml`, on the machine.

So moving a host from the canary ring into the general one is a file edit plus a restart, on that
host. [ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md) named this exactly
when it separated an Agent's type from its name: *"Attributes usable for staged rollouts
(`rollout = "canary"`) still live in a file on each host, so moving a host between rings is a file
edit plus a restart — the per-host wiring ADR-0017 set out to remove, in the one place it remains."*
[ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md) came back to it and called
Server-set labels *"now more clearly worth it"*, because with the Agent type doing the fitting, a
Selector's remaining job is almost entirely rollout rings.

It is the specification's own Problem statement — *"changing what a hundred agents do means reaching
a hundred machines"* — surviving inside the mechanism built to abolish it.

**Two forces decide the shape.**

- **Reported attributes are load-bearing facts, not annotations.** `os.type` and `host.arch` choose
  which artifact an Agent is offered (ADR-0031); `service.name` decides which packages fit it at all
  (ADR-0034). These are matched by the same `matches` function a Selector uses
  ([`configs.rs`](../../crates/server/src/configs.rs)), against the same attribute set. Anything that
  can rewrite that set can hand a Windows binary to a Linux host.
- **The identity a label hangs on has to survive.** The fleet is a map in memory that a restart
  empties, and an Agent can be forgotten deliberately (ADR-0039). A ring assignment that evaporated
  with either would be worse than none, because it would evaporate quietly.

## Decision

We will let the Server attach **labels** to an Agent — operator-set key/value pairs that join the
attribute set Selectors match against, for Configurations and packages alike, and that can never
restate anything the Agent reports.

1. **`PUT /api/v1/agents/{instance_uid}/labels` sets the whole set.** Body `{"labels": {…}}`,
   replacing what was there; an empty map clears them. Whole-map replacement rather than
   add-and-remove operations, for the same reason a package's Selector is set that way (ADR-0017):
   the resource is small, the write is idempotent, and an operator can see the resulting state in the
   request they sent. `AgentView` carries `labels`, so the fleet view shows them.

2. **Labels join what a Selector matches, everywhere one is matched.** Both Configuration targeting
   (ADR-0012) and package targeting (ADR-0017, and the fit of ADR-0031 and ADR-0034) resolve against
   an Agent's `AgentDescription`. Labels are merged into a derived **effective description** and that
   is what matching sees — one seam, both consumers, so a label cannot mean one thing for a
   Configuration and another for a package.

3. **A label can never restate a reported attribute.** This is the crux, and it is where this
   decision deliberately parts company with Bindplane. A key the Agent already reports is **refused**
   at the API, naming it; and at merge time the reported value wins regardless, with the shadowed
   label surfaced on the fleet row rather than silently dropped (ADR-0014's rule). The reason is the
   first force: labels that could override `os.type`, `host.arch`, or `service.name` would let a
   typo in the UI offer an Agent an artifact built for another machine. Labels annotate; they do not
   correct.

4. **Labels are the operator's, and they outlive the Agent record.** They are keyed by Instance UID —
   the only identity that is unique across a fleet, since `service.instance.name` is a name an
   operator may reuse on every host (ADR-0033) — and persisted, so they survive a Server restart. In
   particular **forgetting an Agent (ADR-0039) does not clear its labels**: forgetting drops what the
   Server *learned*, and a label is something the operator *decided*. A host that comes back is in
   the ring it was put in. Clearing them is its own act: an empty map.

5. **They never travel to the Agent.** No new capability, no new message, nothing added to what the
   Client reports about itself. A label is an input to matching on the Server, and the Agent
   experiences it the only way that matters — as the Configuration and the packages it is offered.
   That also keeps the reported attributes honest: everything an Agent reports is still something it
   observed about itself.

6. **Setting labels takes effect at once.** The write bumps the push revision the WebSocket loops
   watch, exactly as editing a Configuration does, so a connected Agent that has just been moved into
   a ring receives its new Configuration on the spot rather than at its next poll.

7. **Stored beside the Configurations, one file per Agent.** `<config_dir>/labels/<instance-uid>.json`
   — a directory the Configuration store's loader ignores, and one file per Agent so a write touches
   nothing else and clearing is a deletion. Configurations already persist here and labels are the
   same kind of thing: operator intent about the fleet, which must be there after a restart.

## Alternatives considered

- **Let a Server label win over a reported attribute** — what Bindplane does: once an agent's labels
  are *bootstrapped*, *"labels in the agent description are not applied"*, and an `overwrite` flag
  exists for changing a pre-existing value. Rejected on the first force. Bindplane can afford it
  because its collector fit is not attribute-driven the way this project's is; here `os.type`,
  `host.arch`, and `service.name` choose which binary an Agent installs (ADR-0031, ADR-0034), so a
  label that outranks them turns a mislabelling into a mis-installation. Refusing the collision costs
  an operator one error message and removes that failure entirely.
- **Flat tags rather than key/value pairs** — Elastic Fleet's model, where an Agent carries a list of
  strings and the API adds and removes them. Simpler to type. Rejected: this project's Selectors are
  equality over key/value attributes (ADR-0012), so a flat tag would need a second matching rule
  beside the one that already exists. Bindplane's labels are key/value for the same reason, and its
  progressive rollout stages match *"all collectors whose labels include every label specified by the
  stage"* — which is precisely this project's Selector semantics already.
- **Add/remove operations** (`tagsToAdd` / `tagsToRemove`, as Elastic's bulk endpoint takes).
  Genuinely better for bulk work, and the natural companion to a fleet-wide "move these fifty hosts
  to stable". Rejected *now* as the first shape: a whole-map `PUT` is one operation with no ordering
  questions, and bulk deserves a decision of its own rather than being smuggled in as a second body
  format.
- **Push labels down to the Client and have it report them**, so the fleet view's attributes remain a
  single list. Rejected twice: it makes an Agent report as *observed* something it was merely told,
  and it puts a round trip between labelling a host and being able to target it — so the first
  Configuration aimed at a fresh label would match nobody.
- **Key labels by `service.instance.name`** so they survive a re-keyed Instance UID. Tempting, since
  the UID is exactly what `AgentIdentification` may change. Rejected: that name is the operator's name
  for an Agent within a Client (ADR-0033) and nothing makes it unique across hosts, so labelling one
  host would silently label its namesakes. The Instance UID is the identity the whole Server is keyed
  on.
- **A separate `labels_dir` in `server.toml`.** Consistent with `config_dir` and `packages_dir`, and
  rejected only on surface: it is a third path an operator must know, back up, and get right, for data
  that belongs to the same store as the Configurations. It can be split out later without changing
  what a label means.
- **Do nothing and let operators put `rollout` in `client.toml`.** What happens today, and it works —
  for a fleet small enough to reach by hand. Rejected because that is the definition of the problem
  this project exists to solve, and because ADR-0034 has already narrowed Selectors to almost exactly
  this job.

## Sources / Prior art

- **[Bindplane labels and the agents API](https://docs.bindplane.com/cli-and-api/api/agents)** — the
  closest precedent, and the one this decision measures itself against. Labels are key/value, set per
  agent (`PATCH /agents/{id}/labels`) or in bulk (`PATCH /agents/labels`), with an `overwrite` flag
  documented as *"if true, overwrite any existing labels with the same names"* and an empty value
  meaning the label is deleted. Its collision rule is the opposite of point 3: `LabelsBootstrapped`
  *"is true if the labels have been 'bootstrapped' … When this is true, labels in the agent
  description are not applied"* — the Server wins. Point 3 explains why this project cannot follow it.
- **[Bindplane progressive rollouts](https://docs.bindplane.com/feature-guides/deployment-and-management/progressive-rollouts)**
  — what labels are *for*, and confirmation that the matching semantics needed are the ones this
  project already has: a stage *"will include all collectors whose labels include every label
  specified by the stage"*, and an operator advances the rollout stage by stage. It also shows where
  this decision stops: staged rollouts as a first-class object are a further step, and labels are
  their prerequisite.
- **[Elastic Fleet agent tags](https://www.elastic.co/docs/reference/fleet/filter-agent-list-by-tags)**
  and the [bulk update endpoint](https://www.elastic.co/docs/api/doc/kibana/operation/operation-post-fleet-agents-bulk-update-agent-tags)
  — the other model: flat string tags, added and removed in bulk
  (`POST /api/fleet/agents/bulk_update_agent_tags` with `tagsToAdd` / `tagsToRemove`, up to 10 000
  agents), used for filtering the agent list. The source of the second and third alternatives: its
  bulk shape is better than a per-agent `PUT` for fleet-scale work, and its flat tags are worse for a
  Selector model that is already key/value.
- This repository: [ADR-0033](0033-an-agents-type-and-its-instance-name-are-two-attributes.md) and
  [ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md), which both named this as the
  next decision; [ADR-0012](0012-selector-targeted-configurations-and-openapi-rest-api.md) and
  [ADR-0017](0017-selector-targeted-packages.md) for the Selector model labels feed;
  [ADR-0031](0031-per-platform-package-variants.md) for the platform fit point 3 protects;
  [ADR-0039](0039-forgetting-an-agent.md), whose forget point 4 deliberately does not undo; and
  [ADR-0014](0014-server-driven-connection-settings.md) for the rule that what is dropped gets said.

## Consequences

- Positive: the last per-host wiring goes away. A rollout ring becomes a Server-side decision, so
  moving a host between rings is one API call instead of an edit and a restart on that host — and it
  applies to both halves of what a Selector aims, configuration and software.
- Positive: it costs no protocol surface at all. No capability, no message, nothing the Agent has to
  support — it is a Server-side input to a matching function that already exists.
- Positive: a package or Configuration Selector written for `rollout = "canary"` now has something to
  match that an operator can change, which is what ADR-0017's staged-rollout argument assumed all
  along.
- Negative / trade-offs: **the fleet's targeting now depends on Server-side state that no Agent
  reports.** Reading `client.toml` on a host no longer tells you what that host will be sent. That is
  the point, and it is also a new place to look when a rollout surprises someone — which is why the
  labels are on the fleet row rather than only in the store.
- Negative / trade-offs: labels are keyed by Instance UID, and `AgentIdentification` may re-key an
  Agent (on a duplicate UID, which this Server does deliberately). A re-keyed Agent loses its labels
  and silently falls back to whatever its reported attributes match. Rare, and the fleet row shows the
  empty set, but it is a real hole that a name-based key would not have — and that a name-based key
  would have paid for with cross-host collisions.
- Negative / trade-offs: refusing a colliding key (point 3) means an operator who genuinely wants to
  correct a badly reported attribute cannot. That is deliberate — the fix belongs on the host, where
  the wrong value is — but it will read as an arbitrary restriction the first time someone hits it,
  so the error says which attribute and where it came from.
- Negative / trade-offs: labels outliving a forgotten Agent (point 4) means a decommissioned host's
  labels stay in the store. They are small, and they are what makes a returning host land in the right
  ring, but nothing prunes them.
- Follow-ups: bulk labelling, which is where Elastic's shape is plainly better than a per-agent `PUT`
  and which a fleet of any size will want; staged rollouts as a first-class object on top of labels,
  which is what Bindplane builds with them and which would answer "roll this out to canary, then to
  the rest" rather than leaving the operator to move the labels; pruning labels for Agents nobody has
  seen in a long time, which meets the retention question ADR-0039 also left open; and set-based
  Selector operators (ADR-0012's own follow-up), which labels make considerably more useful.
