# ADR-0079: The version an Agent reports running stands in for a package version it does not report

- **Status:** ⚪ superseded by [ADR-0083](0083-what-reaches-an-agent.md)
- **Date:** 2026-08-18
- **Deciders:** Markus Brigl

Amends [ADR-0076](0076-a-set-reaches-an-agent-only-as-an-upgrade.md) point 2 — what *unknown* means
in the fourth matching test. Everything else that ADR decides — the test itself, its comparison, its
three consumers, the untouched assignment path, and rollback no longer being a rollout — stands.

## Context

ADR-0076 made a Set reach an Agent only as an upgrade over the version the Agent reports installed
for that package name, and decided that an Agent reporting nothing "has no version to be greater
than", so the Set matches on the other three tests alone. That is the right reading of an Agent that
has genuinely installed nothing. It is the wrong reading of an Agent that simply cannot say.

**Every Client released before this one is such an Agent.** A Client reported a package version only
when a *package* had put the binary there: the status was keyed by the last offer and its version
came from `<state_dir>/installed-package.json`. A Client that arrived by `.deb`, `.rpm`, MSI or by
hand had no record, so it reported an empty package map — and the fleet then proposed it the very
version it was already running, and would have installed an *older* Set over a newer Client. The
Client side of that is fixed: it now reports the version it runs under the name `[self_update]`
consents to, whatever put the binary there.

**That fix cannot reach the Clients that need it most.** A host running 0.4.0 will never report a
package version, because the code that would report it is the code it does not have. The Server
therefore keeps proposing 0.4.0 to a fleet already on 0.4.0, and no amount of correctness on the
Client side changes that for a single deployed host. The Server, meanwhile, is the one component an
operator upgrades centrally — and it already holds the answer: every one of those Clients reports
`service.version`, and for a Client that value *is* the version of the package that carries it
([ADR-0078](0078-a-release-is-named-after-the-set-it-becomes.md) made the Set and the product one
package; ADR-0029 says how it compares).

**The same is true beyond the Client**, which is what makes this a rule rather than a special case:
an Agent that reports `service.version` has told the fleet what it is running. Installing a package
of that same version over it changes provenance, not software.

Two things make the stand-in weaker than a package status, and the decision has to respect both.
A package status names *that package*; `service.version` names the program, which for an addon or a
repacked tree may be numbered differently. And an Agent's `service.version` is whatever its program
says about itself — `1.19` for a GLPI Agent, `2.14.5-1` for a vendor Icinga build — where a package
version is what an operator typed.

## Decision

We will let the version an Agent reports as **`service.version`** stand in for an installed package
version it does not report:

1. **The package status wins whenever there is one.** A non-empty `agent_has_version` for the Set's
   name is a statement about that package, and it is the authority — unchanged, including ADR-0076
   point 3: a value it cannot order refuses the match.
2. **Otherwise the reported `service.version` is compared**, exactly as ADR-0029 compares versions.
   A Set that is not strictly greater does not match.
3. **A `service.version` that cannot be ordered says nothing** — the Set matches, as if the Agent
   had reported no version at all. This is the one asymmetry against point 1 and it is deliberate:
   a package status is a claim about the package and failing closed there is safe, while this is a
   best-effort stand-in, and failing closed on it would silently make every Agent whose program
   numbers itself its own way — `1.19`, `24.04.1`, `unknown` — unreachable by any package at all.
4. **An Agent that reports neither is unchanged**: nothing to be greater than, the Set matches on
   fit and aim alone. That is ADR-0076 point 2 for the case it was written for — the first rollout.
5. **All three consumers keep sharing the test** (ADR-0076 point 4), so the count, the proposal and
   the press cannot disagree, and `fits_agent`'s refusal names the version it compared against and
   where that version came from.

## Alternatives considered

- **Leave it to the Client fix alone.** Correct, and it fixes every host that takes the next
  release. Rejected because it fixes nothing an operator can see today: a fleet on 0.4.0 keeps being
  offered 0.4.0 until each host has been moved by exactly the mechanism that is misreporting, and
  the one component that could have known better — the Server — was told and ignored it.
- **Apply the stand-in only to the Client's own Agent type.** It is the case that prompted this, and
  it would leave supervised agents untouched. Rejected: the Server has no protocol-level way to know
  which type is "the Client's" without hard-coding this project's own type name into the component
  that is deliberately agnostic about what an Agent is (ADR-0034). The rule is either about what an
  Agent reports or it is a special case with a name in it.
- **Fail closed on an unorderable `service.version`**, as ADR-0076 point 3 does for a package
  status. Rejected as stated in point 3: it would turn a program's own numbering habit into a fleet
  that cannot deliver to it, and the symptom — a rollout that assigns nobody — reads like a Selector
  mistake rather than a version one.
- **Report it in the view without acting on it** — mark a proposal the Agent's own version says is
  pointless and leave the decision to the operator. Rejected: the operator's answer is always "then
  do not offer it", and the fleet can draw that conclusion itself.

## Sources / Prior art

- [ADR-0076](0076-a-set-reaches-an-agent-only-as-an-upgrade.md) — the test this amends, and the
  reasoning about equality and unorderable versions that it keeps.
- [ADR-0029](0029-a-version-is-compared-and-shown-without-its-build-metadata.md) — the comparison
  both sides of this rule use, build metadata ignored.
- [ADR-0015](0015-package-delivery-for-managed-processes.md), the Baseline's `PackageStatuses` — `agent_has_version` is
  "the version of the package the Agent has", which an Agent that installed no package cannot state
  and which nothing in the protocol asks it to derive from its own version.
- OpenTelemetry semantic conventions, `service.version`: "the version string of the service API or
  implementation" — the program's own number, which is why it is a stand-in here and not the
  authority.

## Consequences

- **Positive:** a fleet stops being offered what it already runs, on the hosts that are already out
  there, by upgrading the Server alone. The downgrade this also refuses — an older Set reaching a
  newer Agent — is the case that matters most, since for the Client it is the program that manages
  the host.
- **Positive:** the rule reads the same for every Agent, and it needs no knowledge of what kind of
  thing an Agent is.
- **Negative / trade-off: a program already running a version it reports cannot be replaced by the
  fleet's package of that same version.** Installing it would change provenance and nothing else,
  which is why this is a trade-off and not a defect — but it is a door that closes.

  How far it reaches is bounded by who reports a version at all. A Supervisor-backed Agent gets its
  `service.version` from the Managed Process's own description and never from the Client
  ([`agent.rs`](../../crates/client/src/supervisor/agent.rs), ADR-0033), so this touches a Collector
  carrying `opampextension` and anything else that self-reports, and not an Icinga 2 or GLPI Agent,
  which report none. Taking a host over from a machine-installed program is unaffected either way:
  that route names an absolute path and declares no `AcceptsPackages` at all (ADR-0021), and
  switching it to the fleet-owned form points the block at a program directory that starts empty —
  no process, no reported version, and the Set reaches as before.

  Where it does bite, the way round is the operator's: publish the artifact under the next version
  and adopt with that.
- **Negative / trade-off:** two different meanings now feed one comparison, and which one applied is
  not visible in the fleet view — only in the refusal a per-Agent act returns. A Set that reaches
  nobody is already reported as such (ADR-0076 point 8); *why* it reached nobody is one step less
  obvious than before.
- Follow-ups: whether an Agent should be able to state that a version it reports is *not* under
  package management — and so ask to be adopted — is left open; nothing here decides it.
