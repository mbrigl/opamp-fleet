# ADR-0034: What reaches an Agent — fit, aim, and the version it is already running

- **Status:** 🟢 accepted
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

Amends [ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md) point 8 and
[ADR-0028](0028-a-rollout-is-an-explicit-act.md) point 7 on acceptance: rollback is not a rollout.

Everything the first three tests decide stands and is **cited, not restated**:
[ADR-0017](0017-package-artifacts.md) (the Agent type),
[ADR-0017](0017-package-artifacts.md) (the platform),
[ADR-0016](0016-what-a-package-is-and-how-it-is-aimed.md) (the Selector and its specificity rule),
ADR-0016 (what a Set is), ADR-0028 (the act that releases one) and
[ADR-0009](0009-version-baking-comparison-and-display.md) (how versions
compare).

## Context

Three defects a running fleet has shown decide the shape of the rule.

- **Type, platform and Selector alone do not say whom a Set reaches.** A Set matched by those three
  tests and no version test reaches Agents already running its version. The reach count then includes
  them, the fleet view proposes the Set to Agents that need nothing, and the rollout act can aim
  backwards under the same label as a forward one. That is the shape in which a compromised Server
  pushes a known-vulnerable build, and the Client's own `install_offer` already refuses it.
- **A version test against the package status alone cannot reach the hosts that need it most.** Every
  Client released before the one that reports its own package version cannot state it, because the
  code that would is the code it does not have. Each of those hosts does report the version it runs as
  `service.version`, so that version has to stand in where an Agent reports no package version at all.
- **The package status cannot be the sole authority.** A host reporting `service.version` 0.4.0 while
  claiming `supervisor` 0.4.1 installed was believed, found no greater Set, and stayed on 0.4.0 for
  good. A claim about the past stood over a statement about the present, because an install record
  outlives the binary it describes.

Each defect alone suggests a narrower rule, and each narrower rule fails on the next defect. Those
rules are recorded under **Alternatives considered**; the decision below states the rule once and
whole.

## Decision

We will state the whole matching rule once: **a Set reaches an Agent when it fits it, aims at it, and
moves it forward** — where *forward* is measured against both versions the Agent reports, each in the
direction it is good for.

1. **Fit and aim come first and are unchanged.** A Set built for another Agent type is nobody's
   candidate (ADR-0017), nor is one holding no entry for the platform the Agent reports (ADR-0017),
   nor one whose Selector does not match (ADR-0016) — and among candidates sharing a *name*, the most
   specific Selector wins and the greater version breaks a tie (ADR-0016). An Agent that reports no
   type or no platform fits nothing. The test below runs *with* the fit, so a Set it holds back never
   enters the ranking and never raises a conflict.

2. **Forward over what the Agent runs.** The Set's version must be strictly greater than the **lower**
   of the two versions the Agent reports — its non-empty `agent_has_version` for the Set's name, and
   its `service.version` — compared as ADR-0009 compares: major, minor, patch, then the pre-release
   rules, build metadata ignored. Equal does not match: a Set an Agent already runs would reach it
   with nothing.

3. **Never backwards over what it claims.** The Set's version must not be *lower* than a non-empty
   `agent_has_version` for its name. A claim may no longer block delivery of the version it names, but
   it still forbids moving that package back.

4. **Unorderable values, and the asymmetry is deliberate.** A package status that cannot be ordered
   **refuses** the match — it is a claim about that very package, and the safe direction is the one
   `selfupdate.rs` already takes: what cannot be ordered must not be installed over what is running. A
   `service.version` that cannot be ordered **abstains** and says nothing, because it is a
   best-effort stand-in: failing closed on it would make every Agent whose program numbers itself its
   own way — `1.19`, `24.04.1` — unreachable by any package at all.

5. **An Agent that reports neither has nothing to be greater than**: the first rollout, which matches
   on fit and aim alone. An **empty** `agent_has_version` is that same case and not an unorderable
   one — it is how a package that is offered, pending or downloading but not yet installed is
   reported.

6. **The Client applies the same rule to an offer it receives.** For the package that carries the
   Client itself ([ADR-0018](0018-client-self-update.md),
   [ADR-0022](0022-the-fleets-own-agent-is-called-supervisor.md)), *already installed* means the
   version this process runs is the offered one — not that a recorded package hash matches. A record
   whose hash equals the offer may not end the offer on a host that is not running it.

7. **All three consumers of matching apply it.** The candidate resolution (`resolve`,
   `candidate_ids`), the reach count (`package_reach`) and the rollout act (`fits_agent`) test alike,
   so the count, the proposal and the press cannot disagree. A per-Agent refusal names **both**
   versions it read and which one it compared against; the bulk act skips such Agents.

8. **The assignment path stays version-blind.** `assigned_entries`, `assigned_hash_for` and
   `offer_for_assigned` compose an Agent's offer from what was rolled out to it. They must: an offer
   is the Agent's desired state, so filtering an installed package out of it would tell the Agent the
   package is no longer wanted. Matching decides what *may become* an assignment; it never edits one
   that exists.

9. **The pre-ADR-0028 migration seed stays version-blind.** `formerly_offered` reproduces what the old
   publication model had offered at upgrade time (ADR-0028 point 9). It is history, not a proposal,
   and reading it through this test would seed a different fleet than the one that was running.

10. **Rollback is not a rollout.** ADR-0028 point 7's *"rollback of a package is rolling out the older
    Set"* and ADR-0016 point 8's *"greater-version-wins makes rollbacks publication moves"* are
    withdrawn. An operator who wants an Agent back on its predecessor has
    [ADR-0015](0015-package-delivery-for-managed-processes.md)'s retention window on the
    Agent and, for the Client itself, ADR-0018's pointer move; a bad version is otherwise taken back
    by publishing the old content as a new, greater version. A deliberate Server-driven downgrade is
    left undecided, on purpose: it is a separate act with its own authorisation question, and the
    point of this rule is that it must never be the same press as a rollout.

11. **Zero splits into two answers, and the view says which.** A Set can reach nobody for two
    unrelated reasons: it aims at nobody (a misspelled type, no entry for any reported platform, a
    Selector matching no Agent — ADR-0028's warning case), or it aims at Agents that are all already
    at this version or newer (the healthy case). The Set view therefore reports **two** counts —
    `matching_agents`, whom it fits and aims at, version-blind, and `targeted_agents`, whom an act
    would actually move. The bundled UI keeps the amber `⚠ 0` only where both are zero.

Two pieces of reasoning are load-bearing and belong with the clauses above rather than in a
consequence: a program's own number may never *block* a Set, because the two are numbered in
different spaces — a GLPI Agent at `1.19` under a Set an operator numbered `1.0.0`, an Icinga 2 at
`2.14.5-1` under one numbered `2.0.0`; and a claim may never be the *sole* authority, because the
record it comes from outlives the binary it describes, and the Client's own discard of a stale record
runs only at startup and only where the self-update consent names a package.

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
  package version at all, and it believes a claim the Agent's own program contradicts. These are the
  second and third defects under **Context**.
- **Make `service.version` the authority whenever it is present.** The literal reading of "what the
  Agent reports running always decides". Rejected: a program numbering itself above its package would
  then block every Set that genuinely upgrades it, trading one unreachable fleet for another. Hence
  two directions rather than one authority.
- **Compare against the lower of the two and nothing else**, dropping point 3. Simpler, and it also
  unsticks the host on 0.4.0 that claims 0.4.1. Rejected: with a Collector reporting `0.98.0` under an
  `otelcol` Set at `2.0.0`, every Set between the two becomes a candidate and the ranking would
  propose *downgrading* the package whenever the store holds no newer sibling.
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

- [ADR-0009](0009-version-baking-comparison-and-display.md) — the comparison
  every direction of the test uses, build metadata ignored, `opamp::version::precedence` the one
  implementation.
- [ADR-0015](0015-package-delivery-for-managed-processes.md) and the Baseline's `PackageStatuses` —
  `agent_has_version` is "the version of the package the Agent has", and the Baseline's own note that
  an Agent which already has the offered version "does not need to do anything".
- OpenTelemetry semantic conventions, `service.version` — "the version string of the service API or
  implementation": a statement about the running program, which is why it is decisive about what runs
  and silent about what a package numbered it.
- [`selfupdate.rs`](../../crates/client/src/selfupdate.rs) — the Client's own anti-downgrade check,
  which refuses an offer older than the running version because an Ed25519 signature covers the bytes
  and carries no version ordering: an old release stays validly signed forever. The Server offering
  what the Client refuses was a seam, not a feature.

## Consequences

- **Positive:** one document answers "which package reaches which Agent", and it answers it whole.
- **Positive:** the count means what it says, the proposal is only ever work, and neither the fleet
  view nor the act can move an Agent backwards by the same press that moves it forward.
- **Positive:** a host the fleet can see is behind stays reachable — including every Client released
  before the one that reports its own package version, by upgrading the Server alone.
- **Negative / trade-off: an Agent whose program numbers itself below its package version is a
  candidate for the version it already has.** A Collector reporting `0.98.0` under an `otelcol` Set at
  `2.0.0` shows that Set as waiting, and a rollout re-installs bytes it already runs. The fleet cannot
  tell that apart from a false claim — both are "the program denies running the version its package
  status names" — so the operator sees both versions in the refusal and decides.
- **Negative / trade-off:** a program already running a version it reports cannot be replaced by the
  fleet's package of that same version; adopting it means publishing the artifact under the next
  version. A Supervisor-backed Agent gets `service.version` only from its Managed Process's own
  description, so this touches a Collector carrying `opampextension` and anything else that
  self-reports — not an Icinga 2 or GLPI Agent, and not a program directory that starts empty.
- **Negative / trade-off:** the test is two comparisons over two reports instead of one, and "why does
  this Agent see this Set" takes one more step to answer.
- **Follow-ups:** an explicit operator override for a genuine reinstall is left open, as is whether an
  Agent should be able to state that a version it reports is *not* under package management.
