# ADR-0076: A Set reaches an Agent only as an upgrade — the reported installed version is the fourth matching test

- **Status:** ⚪ superseded by [ADR-0083](0083-what-reaches-an-agent.md)
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

Amends [ADR-0052](0052-a-package-is-a-versioned-set.md) point 4 and
[ADR-0061](0061-a-rollout-is-an-explicit-act.md) point 7 on acceptance: rolling out an older Set
stops being the rollback, because an older Set no longer matches an Agent that already runs a
newer one. Everything else in both ADRs — the versioned Set, its identity, one entry per
platform, the assignment, the two rollout acts — stands.

## Context

A Set is matched to an Agent by three tests today ([`packages.rs`](../../crates/server/src/packages.rs)):
its Agent type must be the one the Agent reports (ADR-0034), it must hold an entry for the
platform the Agent reports (ADR-0031), and its Selector must match the Agent's attributes
(ADR-0017). Version enters only *between Sets of the same name*: the greater version wins the
candidate resolution (ADR-0052 point 4). It never enters *between the Set and the Agent*.

The Agent, meanwhile, reports exactly that missing side. `PackageStatuses.packages[name].agent_has_version`
carries, per package name, the version the Agent has installed — the Server stores it in the
Agent record ([`fleet.rs`](../../crates/server/src/fleet.rs)) and already shows it in the fleet
view. Nothing consults it when deciding whom a Set reaches.

Three consequences, all visible in operation:

1. **The reach count overstates the work.** "Roll out (2)" counts two Agents when both already
   run this very version. The count exists to answer "whom would this reach" (ADR-0061 point 10);
   an Agent that would receive a package it already has is not reached by anything.
2. **The candidate is proposed to Agents that need nothing.** The fleet view marks a Set as
   waiting for an Agent that is already at that version, so "waiting" and "up to date" look alike.
3. **The rollout act can aim backwards without saying so.** Rolling out `0.3.1` to a fleet that
   runs `0.3.2` is, by ADR-0061 point 7, the sanctioned rollback — but it is the same press, with
   the same label, as a forward rollout, and the Server does not distinguish the two.

The forces:

- **A downgrade is the shape a compromised Server pushes a known-vulnerable build in.** This
  codebase already draws that line for the Client's own binary: `install_offer` refuses an offer
  older than the running version outright, because the Ed25519 signature covers the bytes and
  carries no version ordering, so an old release stays validly signed forever
  ([`selfupdate.rs`](../../crates/client/src/selfupdate.rs#L157-L170)). The Server offering what
  the Client refuses is a seam, not a feature.
- **"Whom would this reach" must mean what it says.** ADR-0061 made the count first-class
  precisely because zero is the value worth looking at. A count that includes Agents for which
  the act is a no-op is a count an operator learns to distrust.
- **Version comparison is settled here.** ADR-0029 fixed SemVer precedence — major, minor, patch,
  then the pre-release rules, build metadata ignored — and `opamp::version::precedence`
  ([`version.rs`](../../crates/opamp/src/version.rs#L143-L153)) is the one implementation, already
  used by the candidate resolution and by the Client's anti-downgrade check.
- **Rollback has other, better mechanisms.** ADR-0058 keeps a superseded version on the Agent for
  a configurable grace period and restores it when an apply fails; ADR-0020's crash-loop pointer
  move takes the Client back to its predecessor. Both act where the predecessor's bytes actually
  are. Rollout-the-older-Set was the *third* path, and the weakest: it re-delivers bytes to undo
  something the Agent can already undo locally.

The Baseline permits either choice. It describes an agent package as one the Agent installs
"either to upgrade it to a newer version or to downgrade it to an older version", so a Server
that never offers the second is conformant — it simply exercises less of the latitude, exactly
as the Client's own refusal already does.

## Decision

We will make the version a **fourth matching test**: a Set matches an Agent only when its version
is **strictly greater**, by SemVer precedence, than the version the Agent reports as installed
for that package name.

1. **The test.** For a Set of name *N* and version *V*, and an Agent reporting
   `agent_has_version = W` for the package named *N*: the Set matches only if
   `precedence(V, W) == Greater`. Equal does not match — a Set the Agent already runs reaches it
   with nothing. This is ADR-0029's comparison, unchanged and shared: major, minor, patch, then
   pre-release, build metadata ignored.

2. **Unknown means nothing is installed.** An Agent that reports no status for that package name
   — it never received one, or it reports no `PackageStatuses` at all — has no version to be
   greater than, and the Set matches on the other three tests alone. An **empty**
   `agent_has_version` is that same case and not an unorderable one: it is how a package that is
   offered, pending or downloading but not yet installed is reported.

3. **Unorderable means no match.** A reported version `precedence` cannot order is not treated as
   "probably older". It is the one case with no defensible answer, and the safe direction is the
   one `selfupdate.rs` already takes: what cannot be ordered must not be installed over what is
   running.

4. **All three consumers of matching apply it.** The candidate resolution (`resolve`,
   `candidate_ids`), the reach count (`package_reach`), and the rollout act (`fits_agent`) test
   the version alike, so the count, the proposal, and the press cannot disagree. `fits_agent`
   gains the refusal it deliberately did not have; its refusal message names the version the
   Agent reports.

5. **The assignment path is untouched.** `assigned_entries`, `assigned_hash_for` and
   `offer_for_assigned` compose an Agent's offer from what was rolled out to it and stay
   version-blind. They must: an offer is the Agent's desired state, so filtering an installed
   package out of it would tell the Agent the package is no longer wanted. Matching decides what
   *may become* an assignment; it never edits one that exists.

6. **The pre-ADR-0061 migration seed is untouched.** `formerly_offered` reproduces what the old
   publication model had offered at upgrade time (ADR-0061 point 9). It is history, not a
   proposal, and reading it through today's test would seed a different fleet than the one that
   was actually running.

7. **Rollback is no longer a rollout.** ADR-0061 point 7's "rollback of a package is rolling out
   the older Set" and ADR-0052 point 4's "greater-version-wins makes rollbacks publication moves"
   are withdrawn. An operator who wants an Agent back on its predecessor has ADR-0058's retention
   window on the Agent, and — for the Client itself — ADR-0020's pointer move. A deliberate
   Server-driven downgrade is left undecided here, on purpose: it is a separate act with its own
   authorisation question, and the point of this ADR is that it must never be the same press as a
   rollout.

8. **Zero splits into two answers, and the view must say which.** A Set can now reach nobody for
   two unrelated reasons: it aims at nobody (a misspelled type, no entry for any reported
   platform, a Selector matching no Agent — ADR-0061's warning case), or it aims at Agents that
   are all already at this version or newer (the healthy case). The Set view therefore reports
   **two** counts: the Agents the Set fits and aims at, version-blind, and the Agents it would
   actually reach. `targeted_agents` keeps its name and becomes the second; `matching_agents` is
   added beside it as the first. The bundled UI keeps the amber `⚠ 0` only where both are zero and reads the
   version-blind-but-nothing-to-do case as a plain, unalarming statement.

## Alternatives considered

- **Apply the test to the reach count only.** The count would tell the truth while the act
  stayed free to aim backwards, which keeps the rollback path of ADR-0061 intact. Rejected: the
  count would then no longer describe the button beside it — an operator reading "0" would still
  be able to press "roll out to all matching" and change the fleet. One meaning of "matches", or
  the word is worth nothing.
- **Compare against the Agent's own `service.version` instead of the reported package status.**
  Simpler, and it needs no `PackageStatuses`. Rejected: it is only meaningful for the one package
  that *is* the Agent, and says nothing about an addon (ADR-0052's second kind of Set). The
  package status is per package name, which is the granularity the test needs.
- **Let `Equal` match.** It would keep a re-push of the same version available as a repair for a
  failed install. Rejected: re-delivering bytes the Agent already has is not how a failed install
  is repaired — the assignment is already in place, the Agent re-reports its failure, and
  ADR-0058 governs what happens next. Admitting `Equal` would put every up-to-date Agent back
  into every reach count, which is the problem this ADR exists to remove.
- **Make the test a per-Set flag ("allow downgrade").** The Google Update shape: refuse by
  default, permit a downgrade when a policy explicitly says so. Rejected for now under
  *simplicity first* (AGENTS.md §1) — it is a second, standing mode on the resource, and the
  requirement in hand asks for the rule, not for an exemption to it. If a Server-driven downgrade
  is wanted later it should be its own act, not a modifier on this one.

## Sources / Prior art

- OpenTelemetry OpAMP specification, *Packages* — an agent package is installed "either to
  upgrade it to a newer version or to downgrade it to an older version", and `PackageStatuses`
  carries `agent_has_version` per package:
  <https://github.com/open-telemetry/opamp-spec/blob/main/specification.md>
- Microsoft WSUS, *Updates Operations* — the per-update population an administrator reads is the
  set of computers that **need** the update; a computer that already installed a superseding
  update reports the older one as *Not Applicable* rather than *Needed*. The count is defined by
  what is missing on the client, not by what the update targets:
  <https://learn.microsoft.com/en-us/windows-server/administration/windows-server-update-services/manage/updates-operations>
- Google Update / Chrome Enterprise, `RollbackToTargetVersion` — with the policy unset, "installs
  that have a version higher than that available will be left as-is"; downgrading requires
  enabling an explicit rollback policy, and is bounded to the three latest major releases:
  <https://chromeenterprise.google/policies/device-rollback-to-target-version/> and
  <https://support.google.com/chrome/a/answer/7125792>
- In this codebase: `install_offer`'s downgrade refusal
  ([`crates/client/src/selfupdate.rs`](../../crates/client/src/selfupdate.rs)) — the same rule,
  already applied at the receiving end — and ADR-0029's version comparison, ADR-0058's retention
  window, ADR-0020's crash-loop pointer move.

## Consequences

- Positive: the reach count means "how many Agents this would actually upgrade", so the press
  beside it is never a no-op. The Server stops offering what the Client would refuse. An Agent is
  no longer marked as waiting for a Set it already runs, which removes the standing false
  positive from the fleet view. A compromised or mistaken Server cannot walk a fleet backwards
  through the ordinary rollout path.
- Negative / trade-offs: the operator loses the one Server-driven way to put an Agent back on an
  older version; until a separate downgrade act exists, a genuine fleet-wide rollback needs the
  Agent-side retention window (ADR-0058) or a new Set with a greater version carrying the older
  content — the latter is a lie about the version and should be called out as one. Matching now
  depends on what the Agent *reports*, so an Agent that stops reporting package statuses is
  matched as if it had nothing installed. The reach computation reads each Agent's statuses in
  addition to its description; it stays one pass over the fleet.
- Follow-ups: a deliberate, separately authorised downgrade act — what may aim it, what it does
  to the assignment, and whether the Client's own anti-downgrade refusal admits an exception —
  is left for its own ADR. The second count added in point 8 is a REST response field; the UI's
  wording for "aims at Agents, none of them behind" is a design detail to settle with it.
