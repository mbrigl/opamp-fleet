# ADR-0075: The self-update consent stands unless it is withdrawn — and the installers can ask

- **Status:** 🟢 accepted
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

## Context

[ADR-0020](0020-client-self-update.md) gave the Client the ability to be replaced by a package the
Server offers, and [ADR-0027](0027-interactive-install-writes-the-first-configuration.md) point 4
decided how that ability is switched on:

> `[self_update]` (ADR-0020) is offered last and defaults to **no**: consent for the Server to
> replace the binary that manages every other binary on the host is the larger grant, and it stays a
> deliberate answer rather than a default one.

The reasoning is sound and the shape it produced is not. Two things have happened since.

**The default is unreachable on the path most hosts take.** ADR-0046's MSI collects one answer —
the endpoint — and calls `service install --endpoint`, which had no self-update flag at all. So a
Windows host installed the documented way could not consent, *at install time or ever*, without
someone knowing that a section exists, what it is called, and that its absence is what silences it.
The `client.toml` the installer writes lists every other key it did not ask about as a comment;
`[self_update]` is not among them. This was found on a live fleet: a Windows Client sat with a
package assigned to it and never fetched it, because its Agent declared no `AcceptsPackages` — and
nothing anywhere said so.

**"Absent means no" is the wrong way round for a fleet.** The absent section is the common state,
and it makes the Client the one program in the fleet that has to be patched by hand on every host —
which is the work fleet management exists to end. The grant is real, but so is its alternative: an
un-updatable agent is a security position too, and a worse one, because a Client that cannot be
updated cannot be *fixed*. The asymmetry ADR-0027 weighed — a large grant against a small
convenience — is actually a large grant against a fleet-wide patching gap.

What has *not* changed is why the grant needs narrowing. A package with an empty Selector reaches
every Agent that accepts packages ([ADR-0017](0017-selector-targeted-packages.md)), so a consent
with no name attached would let the first fleet-wide artifact an operator uploads be written over
the Client and take the host out of reach. The name is what makes the consent specific, and it is
the part worth keeping.

## Decision

We will make the self-update consent **stand by default, narrowed to the Client's own Agent type**,
and give every install path a way to withdraw it.

This **supersedes [ADR-0027](0027-interactive-install-writes-the-first-configuration.md) point 4
only**. Everything else that ADR decides — opt-in interactivity, never overwriting an existing file,
the config path, the `0600` mode, validation before registration, the warning on a bare install —
stands unchanged.

Bound by this decision:

- **An absent `[self_update]` section is consent**, under the Client's own Agent type
  ([ADR-0028](0028-the-client-is-named-opamp-fleet-client.md)), which is what a Set carrying this
  Client is keyed by anyway
  ([ADR-0034](0034-a-package-states-the-agent-type-it-is-built-for.md)). The default is therefore
  not a wildcard — it is the one package that could legitimately be this Client, and an offer under
  any other name is refused and reported exactly as before. (That type was `opamp-fleet-client` when
  this ADR was written; [ADR-0077](0077-the-clients-own-agent-type-is-supervisor.md) renamed it to
  `supervisor`, and the default — still the type — follows it.)
- **The withdrawal is written down**, as `enabled = false`. A Client the fleet cannot update says so
  in its own configuration rather than saying nothing at all, which is the state that was
  indistinguishable from an oversight.
- **An empty package name with the consent standing fails at load**, naming the key. The name is the
  whole of the narrowing, and an empty one would widen the consent to whatever the Server offers
  next — a failure to catch at startup, not at the first offer.
- **Every install path can answer it.** `service install --no-self-update` is the non-interactive
  withdrawal, beside `--endpoint`; `--self-update-package <NAME>` names a different package for a
  deployment whose Set is named differently; the questionnaire asks as it always did and now
  defaults to yes; the MSI gains a checked-by-default checkbox on its endpoint dialog and a public
  `SELFUPDATE` property, so `msiexec /qn … SELFUPDATE=0` withdraws it the way Intune and Group
  Policy will.
- **The answer lands in `client.toml` either way.** The installer's job is to write the first
  configuration ([ADR-0027](0027-interactive-install-writes-the-first-configuration.md)), not to
  hold state of its own, so what the checkbox decided is visible and editable on the host afterwards.

## Alternatives considered

- **Keep the default at "no" and only make the MSI able to say "yes".** The minimal fix, and it
  answers the incident that prompted this. Rejected because it leaves the fleet-wide default at the
  state that produced the incident: every host installed before the checkbox existed, and every host
  installed by a script that does not know the flag, stays un-updatable and silent about it.
- **Default to consent with no name at all** — accept any package the Server offers this Agent.
  Rejected outright: ADR-0017's empty Selector reaches every Agent, so the first fleet-wide artifact
  would be written over the Client. The name is not ceremony.
- **A third state: "ask the Server whether it has one for us".** No such negotiation exists in the
  Baseline, and inventing one to avoid writing a boolean would be a protocol extension for a
  configuration question.
- **Leave the config default alone and have the *installers* write `[self_update]` explicitly.**
  Tempting, and it is what the first draft of this decision did: existing hosts keep their behaviour
  exactly, and only new installs are updatable. Rejected because it splits the meaning of the same
  file — a `client.toml` without the section would mean "no" on an upgraded host and nothing at all
  on a fresh one — and because it leaves every already-installed Client in the state this ADR exists
  to end. The cost is recorded under Consequences instead.

## Sources / Prior art

- The incident, 2026-08-18: a Windows Client reporting `capabilities` without `AcceptsPackages` (8)
  or `ReportsPackageStatuses` (16), with `package_assignments` set and `package_statuses` absent —
  an assigned package that could never be offered, and no diagnostic anywhere in the path.
- [ADR-0020](0020-client-self-update.md) — what the consent grants and how the update proves itself
  before the pointer moves.
- [ADR-0027](0027-interactive-install-writes-the-first-configuration.md) point 4 — the decision this
  supersedes, and the questionnaire it belongs to.
- [ADR-0046](0046-a-release-ships-native-installers.md),
  [ADR-0049](0049-the-msi-prefills-the-development-endpoint.md) — the installers that now carry the
  answer, and the precedent for a public MSI property an administrator can set.
- Elastic Agent and the OpenTelemetry `opampsupervisor`, re-read for this: neither ships an
  agent-updates-itself consent switch, because neither updates its own binary from the control
  plane at all. There is no upstream default to follow.

## Consequences

- Positive: a fleet can patch its own agent. The Client stops being the one program on the host that
  an operator has to reach by hand, which is the whole proposition of ADR-0020 finally reaching the
  hosts it was built for.
- Positive: the answer is askable and visible — a checkbox at install time, a key in the file
  afterwards, and a flag for every scripted path.
- **Negative / trade-offs: this changes the behaviour of Clients that are already installed.** A host
  whose `client.toml` has no `[self_update]` section — which is most of them, and includes every host
  where the questionnaire's old default of "no" was accepted — begins accepting self-update offers
  when it comes up on this version. That is a real widening applied without asking, and the only
  honest mitigation is that it is loud: the CHANGELOG names it, and the withdrawal is one line. An
  operator who wants the old behaviour writes `enabled = false` before upgrading.
- Negative / trade-offs: an operator who never chose either way is now in the larger of the two
  positions. The narrowing to the package name is what bounds it, and the Server still only offers
  what an explicit rollout act assigned ([ADR-0061](0061-a-rollout-is-an-explicit-act.md)) — but a
  compromised Server reaches further than it did yesterday.
- Follow-ups: the silence that made the incident expensive is not fixed here. An Agent assigned a
  package it can never be offered — because it declares no capability, or because the Set holds no
  entry for its platform — is skipped without a word by `rollout_package` and shown as *assigned* in
  the fleet view. That deserves its own change.
