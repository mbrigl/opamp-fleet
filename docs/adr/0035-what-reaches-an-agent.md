# ADR-0035: What reaches an Agent — fit, aim, and the version it is already running

- **Status:** 🟢 accepted
- **Date:** 2026-08-20
- **Deciders:** Markus Brigl

Everything the first three tests decide stands and is **cited, not restated**:
[ADR-0016](0016-a-package-is-a-versioned-set.md) (the Agent type),
[ADR-0021](0021-one-platform-vocabulary.md) (the platform),
[ADR-0016](0016-a-package-is-a-versioned-set.md) (the Selector and its specificity rule),
[ADR-0016](0016-a-package-is-a-versioned-set.md) (what a Set is),
[ADR-0030](0030-a-rollout-is-an-explicit-act.md) (the act that releases one) and
[ADR-0009](0009-version-from-cargo-toml-and-git.md) (how versions
compare).

## Context

Matching by type, platform and Selector alone answers which Sets *fit* an Agent, not which ones
would *move* it. Each gap below is a defect a running fleet has shown.

- **Without a version test, a Set reaches Agents that need nothing.** The reach count includes
  Agents already running the version, the fleet view proposes a Set to Agents that need nothing, and
  the rollout act can aim backwards under the same label as a forward one — the shape in which a
  compromised Server pushes a known-vulnerable build, and one the Client's own `install_offer`
  already refuses.
- **Without the running version, the test cannot reach the hosts that need it most.** An Agent may
  report its version as `service.version` and no package version at all: every Client released
  before the one that reports its own version cannot state it, because the code that would
  is the code it does not have.
- **With the package status as the authority, a stale claim strands a host.** A host reporting
  `service.version` 0.4.0 while claiming `supervisor` 0.4.1 installed is believed, finds no greater
  Set, and stays on 0.4.0 for good — a claim about the past standing over a statement about the
  present, because an install record outlives the binary it describes.
- **And the same wall from the other side.** A `supervisor` Set at 0.4.1 does not reach a host whose
  package status claims 0.4.2 while its program reports 0.4.1, if a Set *under* a claim is refused
  before `service.version` is read at all.

**Why the claim keeps being wrong, and why that is not a detail.** `agent_has_version` is derived
from what an install once wrote — for the Client `<state_dir>/installed-package.json` — and the
terminal status a self-update reports after the restart names the version the *marker* carried, not
the version of the process that came up ([`selfupdate::commit`](../../crates/client/src/selfupdate.rs)).
A staged update that did not take, a host reinstalled from an older artifact, a state directory
restored beside a downgraded binary — each leaves a record about an intention standing over a
statement about the present.

**The Baseline already says which of the two the field is.** `PackageStatus.agent_has_version` is
*"the version of the package that the Agent **has**. MUST be set if the Agent has this package"*, and
*"MUST be empty if the Agent does not have this package. This may be the case for example if the
package was offered by the Server but failed to install and the Agent did not have this package
previously."* It is a statement about the present, and the failed-install sentence is the very shape
that produces the false claims above. A record naming a version the program is not running is
therefore not a claim the protocol sanctions and a Server must respect — it is a report that is
already non-conforming. What a Server offers, the Baseline leaves to the Server.

## Decision

We will state the whole matching rule once: **a Set reaches an Agent when it fits it, aims at it, and
moves it forward** — where *forward* is measured against what the Agent reports it is **running**,
and against what it claims to have only where it reports no running version that can be ordered.

1. **Fit and aim come first and are unchanged.** A Set built for another Agent type is nobody's
   candidate (ADR-0016), nor is one holding no entry for the platform the Agent reports (ADR-0021),
   nor one whose Selector does not match (ADR-0016) — and among candidates sharing a *name*, the most
   specific Selector wins and the greater version breaks a tie (ADR-0016). An Agent that reports no
   type or no platform fits nothing. The test below runs *with* the fit, so a Set it holds back never
   enters the ranking and never raises a conflict.

2. **What it runs decides, in both directions.** Where the Agent reports a `service.version` that
   parses as [ADR-0009](0009-version-from-cargo-toml-and-git.md)'s grammar, the Set's version must be
   **strictly greater** than it, compared as ADR-0009 compares: major, minor, patch, then the
   pre-release rules, build metadata ignored. Equal does not match: a Set an Agent already runs would
   reach it with nothing.

3. **The claim is not consulted where the running version is** — neither to admit a Set the running
   version refuses, nor to refuse one it admits. A record about the past does not overrule a
   statement about the present, and it does not get a veto over it either. This is what lets a Set
   reach a host whose package status claims a version its own program denies running, in either
   direction.

4. **Where the running version cannot be ordered, the claim is the whole test.** A `service.version`
   that does not parse — `1.19`, `24.04.1` — says nothing at all, and the Set is held against a
   non-empty `agent_has_version`: strictly greater to match, and a claim that cannot itself be
   ordered **refuses** the match, which is the safe direction for a claim about that very package and
   the one `selfupdate.rs` already takes — what cannot be ordered must not be installed over what is
   running.

5. **A Set is numbered in the space its program numbers itself.** Once the running version decides, a
   Set numbered below the program it carries can never reach it — for a program that self-reports one,
   which is the Client and any OpAMP-aware Managed Process — and this stops being a matter of taste.
   `opamp-package-fetch` already names a release after the Set it becomes
   ([ADR-0022](0022-agent-type-instance-name-and-the-supervisor-name.md) clause 8); an operator numbering a
   Set by hand takes the program's own number, not one of their own invention.

6. **An Agent that reports neither has nothing to be greater than**: the first rollout, which matches
   on fit and aim alone. An **empty** `agent_has_version` is that same case and not an unorderable
   one — it is how a package that is offered, pending or downloading but not yet installed is
   reported.

7. **The Client applies the same rule to an offer it receives.** For the package that carries the
   Client itself ([ADR-0017](0017-client-self-update-and-its-consent.md), ADR-0022), *already installed* means the
   version this process runs is the offered one — not that a recorded package hash matches. A record
   whose hash equals the offer may not end the offer on a host that is not running it.

8. **All three consumers of matching apply it.** The candidate resolution (`resolve`,
   `candidate_ids`), the reach count (`package_reach`) and the rollout act (`fits_agent`) test alike,
   so the count, the proposal and the press cannot disagree. A per-Agent refusal names the version it
   compared against and whether that was the running one or the claim, so an operator reading "not an
   upgrade" can tell which of the two decided; the bulk act skips such Agents.

9. **The assignment path stays version-blind.** `assigned_entries`, `assigned_hash_for` and
   `offer_for_assigned` compose an Agent's offer from what was rolled out to it. They must: an offer
   is the Agent's desired state, so filtering an installed package out of it would tell the Agent the
   package is no longer wanted. Matching decides what *may become* an assignment; it never edits one
   that exists.

10. **The pre-ADR-0030 migration seed stays version-blind.** `formerly_offered` reproduces what the
    old publication model had offered at upgrade time (ADR-0030 point 9). It is history, not a
    proposal, and reading it through this test would seed a different fleet than the one that was
    running.

11. **Rollback is not a rollout.** ADR-0030 point 7's *"rollback of a package is rolling out the older
    Set"* and ADR-0016 point 3's *"greater-version-wins makes rollbacks publication moves"* are
    withdrawn. An operator who wants an Agent back on its predecessor has
    [ADR-0015](0015-package-delivery-for-managed-processes.md)'s retention window on the
    Agent and, for the Client itself, ADR-0017's pointer move; a bad version is otherwise taken back
    by publishing the old content as a new, greater version. A deliberate Server-driven downgrade is
    left undecided, on purpose: it is a separate act with its own authorisation question, and the
    point of this rule is that it must never be the same press as a rollout.

12. **Zero splits into two answers, and the view says which.** A Set can reach nobody for two
    unrelated reasons: it aims at nobody (a misspelled type, no entry for any reported platform, a
    Selector matching no Agent — ADR-0030's warning case), or it aims at Agents that are all already
    at this version or newer (the healthy case). The Set view therefore reports **two** counts —
    `matching_agents`, whom it fits and aims at, version-blind, and `targeted_agents`, whom an act
    would actually move. The bundled UI keeps the amber `⚠ 0` only where both are zero.

One piece of reasoning is load-bearing and belongs with the clauses above rather than in a
consequence: a claim may never be the *sole* authority, because the record it comes from outlives the
binary it describes, and the Client's own discard of a stale record runs only at startup and only
where the self-update consent names a package. Points 2 and 3 give up the opposite principle — *a
program's own number may never block a Set, because the two are numbered in different spaces* — and
the price is stated in **Consequences** rather than hidden there.

## Alternatives considered

- **Apply the test to the reach count only**, leaving the act free to aim backwards. Rejected: the
  count would no longer describe the button beside it — an operator reading "0" could still press
  "roll out to all matching" and change the fleet. One meaning of "matches", or the word is worth
  nothing.
- **Let `Equal` match**, keeping a re-push of the same version available as a repair for a failed
  install. Rejected: a failed install is not repaired by re-delivering bytes the Agent already has —
  the assignment is in place, the Agent re-reports its failure, and ADR-0015 governs what follows.
  Admitting `Equal` puts every up-to-date Agent back into every reach count.
- **Compare against `agent_has_version` alone.** Rejected: it cannot reach a Client that reports no
  package version at all, and it believes a claim the Agent's own program contradicts.
- **Hold the Set against the lower of the two versions and forbid moving under a claim.** Rejected:
  the second half refuses a Set *below* a claim before the running version is read at all, so a host
  whose claim is wrong in the upward direction is stranded exactly as a host whose claim is wrong at
  the equal version would be. It fixes one face of one defect and leaves the other.
- **Scope the change to the package that carries the Client itself** (`supervisor`, ADR-0022) — let
  the running version outrank the claim only where the two numbers are provably the same number, and
  leave every Managed Process on the claim-first rule. This keeps the Collector case below intact and
  fixes the case the fleet actually hits. Put to the maintainer with that example and
  **not chosen**: it makes the matching rule two rules keyed on which package is being matched, and
  every new agent kind then arrives with the question of which of the two it falls under. Recorded
  because it remains the narrower change, and because the negatives below are the price of not taking
  it.
- **Fix the report instead of the rule** — have `selfupdate::commit` report the version of the process
  that came up rather than the one the marker named, so the false claim is never sent. Not chosen as
  *the* answer, because it repairs only the claims this Client writes: a host reinstalled from an
  older artifact, or a state directory restored beside a downgraded binary, still produces a claim no
  Client-side fix reaches. It stays worth doing and is listed as a follow-up.
- **Fail closed on an unorderable `service.version`**, as point 4 does for a package status.
  Rejected: it turns a program's numbering habit into a fleet that cannot deliver to it, and the
  symptom — a rollout that assigns nobody — reads like a Selector mistake rather than a version one.
- **Rely on the Client discarding a stale record**, or on publishing the next version to adopt past a
  false claim. Both exist and both stay available; rejected as *the* answer, because the first
  depends on the misreporting side to correct itself and runs only at startup, and the second makes
  every false claim cost a release.
- **A per-Set "allow downgrade" flag**, the Google Update shape: refuse by default, permit when a
  policy says so. Rejected under *simplicity first* (AGENTS.md §1) — a standing second mode on the
  resource, where what was asked for is the rule. A Server-driven downgrade, if it is ever wanted,
  should be its own act rather than a modifier on this one.
- **An operator override on the per-Agent act ("roll out anyway").** Rejected as the primary answer:
  it asks an operator to overrule a rule rather than fixing it, and it is unavailable to the bulk act
  and to the count. It stays open as a follow-up for a genuine reinstall.

## Sources / Prior art

- [ADR-0009](0009-version-from-cargo-toml-and-git.md) — the comparison
  every direction of the test uses, build metadata ignored, `opamp::version::precedence` the one
  implementation.
- **The OpAMP specification**, `PackageStatus.agent_has_version`: *"The version of the package that
  the Agent has. MUST be set if the Agent has this package"*, *"MUST be empty if the Agent does not
  have this package. This may be the case for example if the package was offered by the Server but
  failed to install and the Agent did not have this package previously."* The field is defined as a
  statement about the present, and the failed-install sentence names the exact origin of the stale
  claims points 2 and 3 overrule. The same specification defines the package messages and their
  statuses but does not prescribe which packages a Server offers to which Agent — that is this
  project's to decide, which is why this rule exists at all.
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>
- **Kubernetes API conventions**, `spec` versus `status`: *"The specification is a complete
  description of the desired state"*, while *"`status` should be the most recent observations of
  actual state"* — observed state is reconstructed from the running system rather than kept as a
  record of what was intended, and a controller reconciles against the observation. That is the same
  separation points 2 and 3 apply: `service.version` is the observation, the package status a record
  of an intention that may not have taken.
  <https://github.com/kubernetes/community/blob/master/contributors/devel/sig-architecture/api-conventions.md>
- [ADR-0015](0015-package-delivery-for-managed-processes.md) and the Baseline's `PackageStatuses` —
  and the Baseline's own note that an Agent which already has the offered version "does not need to
  do anything".
- OpenTelemetry semantic conventions, `service.version` — "the version string of the service API or
  implementation": a statement about the running program.
- [`selfupdate.rs`](../../crates/client/src/selfupdate.rs) — the Client's own anti-downgrade check,
  which refuses an offer older than the running version because an Ed25519 signature covers the bytes
  and carries no version ordering: an old release stays validly signed forever. A Server offering
  what the Client refuses would be a seam, not a feature.

## Consequences

- **Positive:** one document answers "which package reaches which Agent", and it answers it whole.
- **Positive:** the count means what it says, the proposal is only ever work, and neither the fleet
  view nor the act can move an Agent backwards by the same press that moves it forward — where
  "backwards" is measured against what the Agent runs rather than against what it claims.
- **Positive: a stale claim does not strand a host, in either direction.** A claim stale at the
  equal version and a claim stale upward are the same defect seen from two sides, and one rule
  answers both: the Agent's own program is believed about the Agent's own program.
- **Positive: it matches the field's own definition.** A Server that believed `agent_has_version`
  over a contradicting `service.version` would grant authority the Baseline does not give it.
- **Positive:** the test is one comparison over one report where a running version can be ordered,
  instead of two comparisons ranked against each other in opposite directions. "Why does this Agent
  see this Set" takes one step to answer.
- **Negative / trade-off: a Managed Process numbered above its Set can be moved backwards.** A
  Collector reporting `0.98.0` with its package status claiming `2.0.0` matches a Set numbered
  `1.5.0`, because `1.5.0 > 0.98.0` and the claim is not consulted — moving that package from `2.0.0`
  back to `1.5.0`. What limits it is that nothing moves on its own: a rollout is an explicit act
  (ADR-0030), the operator sees the version they are pressing, and ADR-0015's retention window is the
  way back.
- **Negative / trade-off: an Agent numbered far above its Set becomes unreachable by that Set's
  number.** Where a program self-reports an orderable version above the Set that carries it, no Set
  below that number can ever be offered to it — the Set is undeliverable wherever the two numbering
  spaces disagree upward. Point 5 is the answer: number the Set in the program's space, and re-create
  an existing Set at the program's number — a new Set, not an edit — before it reaches those Agents
  again.

  **How far this reaches is narrower than it looks, and the reason is worth stating.** It requires a
  program that *self-reports*, and only an OpAMP-aware Managed Process does: a Supervisor-backed Agent
  gets `service.version` from its Managed Process's own description or not at all (see the clause
  below). Icinga 2 and a GLPI Agent report none, so points 2 and 3 never apply to them and their Sets
  keep being matched on the package status — the `2.14.5-1` and `1.19` numbers this document cites as
  different-space examples are numbers the fleet never receives. What is exposed is a Collector
  carrying `opampextension`, and the Client itself — where the two numbers are the same number, which
  is the case this rule exists to fix.
- **Negative / trade-off: an unorderable running version falls back to the claim rather than
  abstaining.** The practical test is then point 4's claim test, and it is worth stating plainly
  that a program numbering itself `1.19` is matched entirely on its package status, with no statement
  about the present involved at all.
- **Negative / trade-off:** a program already running a version it reports cannot be replaced by the
  fleet's package of that same version; adopting it means publishing the artifact under the next
  version. A Supervisor-backed Agent gets `service.version` only from its Managed Process's own
  description, so this touches a Collector carrying `opampextension` and anything else that
  self-reports — not an Icinga 2 or GLPI Agent, and not a program directory that starts empty.
- **Follow-up — report what came up, not what was staged.** `selfupdate::commit` naming the marker's
  version is the origin of the false claims on this project's own Client, and it is worth correcting
  regardless of this rule, so that the Server and the operator see a claim that is true.
- **Follow-up — surface the disagreement in the UI.** Since the Server prefers one of two
  contradicting versions, the operator should be able to see that they contradicted: the `pkg` badge
  and the reported `service.version` side by side, with the disagreement marked rather than silently
  resolved.
- **Follow-ups:** an explicit operator override for a genuine reinstall is left open, as is whether an
  Agent should be able to state that a version it reports is *not* under package management; and
  point 5's numbering guidance needs a home in the manual, where an operator reads it before naming a
  Set rather than after their rollout reaches nobody.
