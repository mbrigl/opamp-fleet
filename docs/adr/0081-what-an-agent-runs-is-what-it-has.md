# ADR-0081: What an Agent runs is what it has — the lower of the two versions it reports

- **Status:** ⚪ superseded by [ADR-0083](0083-what-reaches-an-agent.md)
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

Amends [ADR-0079](0079-the-version-an-agent-runs-stands-in-for-an-unreported-package-version.md)
point 1 — the package status is no longer the *sole* authority when the Agent's own program
contradicts it. Everything else ADR-0079 and
[ADR-0076](0076-a-set-reaches-an-agent-only-as-an-upgrade.md) decide stands: the fourth matching
test, its comparison, its three consumers, and the untouched assignment path.

## Context

ADR-0079 made the version an Agent reports as `service.version` stand in for a package version it
does *not* report, and kept the package status as the authority wherever there is one: a non-empty
`agent_has_version` is a statement about that very package, and a claim beats a stand-in.

**A fleet has now shown what happens when the claim is false.** A host reporting `service.version`
0.4.0 also reports `supervisor` 0.4.1 as installed. The fourth test reads the claim, finds no Set
strictly greater than 0.4.1, and offers nothing — so the host stays on 0.4.0 for good. Nothing here
is a first rollout, an equal version or an unorderable one: it is the fleet believing a version the
program itself denies running.

**The two reports are not the same kind of fact.** `agent_has_version` is derived from what an
install once wrote — for the Client, `<state_dir>/installed-package.json` — and that record outlives
the binary it describes. The Client drops a record that does not name the version it runs, but only
where the self-update consent names a package and only while starting up
([`accept_packages_named`](../../crates/client/src/supervisor/agent.rs)); the terminal status a
self-update reports after the restart is the version the *marker* named, not the version of the
process that came up ([`selfupdate::commit`](../../crates/client/src/selfupdate.rs)). A version
switch that did not take effect, a host reinstalled from an older artifact, a state directory
restored beside a downgraded binary — each leaves a claim about the past standing over a statement
about the present. `service.version` is that statement about the present: whatever else it is
uncertain about, a running program does know which version it is.

**The asymmetry ADR-0079 rested on is real and stays respected.** A package status names *that
package*; `service.version` names the program, which for an addon or a repacked tree is numbered in
a different space entirely — a GLPI Agent at `1.19` under a Set the operator numbered `1.0.0`, an
Icinga 2 at `2.14.5-1` under one numbered `2.0.0`. That is exactly why the program's number must
never be allowed to *block* a Set: it would make a fleet undeliverable wherever the two spaces
disagree upward. What it may do is refuse to keep a Set out that the Agent's own program says it is
not running.

## Decision

We will hold a Set against **both** versions an Agent reports, each in the direction it is good for:

1. **Forward over what it runs.** The Set's version must be strictly greater than the **lower** of
   the two versions the Agent reports — its non-empty `agent_has_version` for the Set's name and its
   `service.version` — compared as [ADR-0029](0029-a-version-is-compared-and-shown-without-its-build-metadata.md)
   compares versions. Where only one of the two is reported, that one is the lower one and this is
   ADR-0076/ADR-0079 unchanged.
2. **Never backwards over what it claims.** The Set's version must not be *lower* than a non-empty
   `agent_has_version` for its name. This is the guard the package status keeps: a claim may no
   longer block delivery of the version it names, but it still forbids moving that package back.
3. **Unorderable values are unchanged.** A package status that cannot be ordered refuses the match
   (ADR-0076 point 3, the safe direction for a claim about the package); a `service.version` that
   cannot be ordered abstains and says nothing (ADR-0079 point 3), so a program numbering itself
   `1.19` or `24.04.1` stays reachable.
4. **An Agent that reports neither is unchanged**: the first rollout, which matches on fit and aim.
5. **The Client applies the same rule to an offer it receives.** For the package that carries the
   Client itself (ADR-0020, ADR-0078), *already installed* means the version this process runs is
   the offered one — not that a recorded package hash matches. A record whose hash equals the offer
   may no longer end the offer on a host that is not running it.
6. **All three consumers keep sharing the test** (ADR-0076 point 4), and a per-Agent refusal names
   both versions it read and which one it compared against.

## Alternatives considered

- **Leave ADR-0079 point 1 and rely on the Client discarding a stale record.** That discard exists
  and is right, but it runs only at startup, only where the self-update consent names a package, and
  it cannot repair a claim made after it — while the hosts that need it are precisely the ones the
  fleet can no longer reach. A rule that depends on the misreporting side to correct itself is the
  same dead end ADR-0079 was written to leave.
- **Publish the next version and adopt with that** — ADR-0079's stated way round. It works, and it
  stays available. Rejected as *the* answer: it makes every false claim cost a release, and it
  cannot repair a host whose claim is simply wrong about a version that is already correct.
- **Make `service.version` the authority whenever it is present.** The literal reading of "what the
  Agent reports running always decides". Rejected: a program numbering itself above its package —
  Icinga 2 at `2.14.5-1` under a Set at `1.1.0` — would then block every Set that genuinely upgrades
  the package, trading one unreachable fleet for another. Hence the two directions in the decision
  rather than one authority.
- **Compare against the lower of the two and nothing else.** Simpler, and it also unsticks the case
  at hand. Rejected: with a Collector reporting `0.98.0` under an `otelcol` Set at `2.0.0`, every
  Set between the two becomes a candidate, and the ranking would propose *downgrading* the package
  whenever the store holds no newer sibling. Point 2 is what keeps that shut.
- **An operator override on the per-Agent rollout act ("roll out anyway").** Rejected as the primary
  answer: it asks an operator to overrule a rule that is wrong here rather than fixing the rule, and
  it is unavailable to the bulk act and the count. It stays open as a follow-up for the genuine
  reinstall case.

## Sources / Prior art

- [ADR-0076](0076-a-set-reaches-an-agent-only-as-an-upgrade.md) and
  [ADR-0079](0079-the-version-an-agent-runs-stands-in-for-an-unreported-package-version.md) — the
  test this amends and the stand-in it generalises.
- [ADR-0029](0029-a-version-is-compared-and-shown-without-its-build-metadata.md) — the comparison
  both directions use, build metadata ignored.
- [ADR-0015](0015-package-delivery-for-managed-processes.md), the Baseline's `PackageStatuses` —
  `agent_has_version` is "the version of the package the Agent has", and the Baseline's own note
  that an Agent which already has the offered version "does not need to do anything".
- OpenTelemetry semantic conventions, `service.version` — "the version string of the service API or
  implementation": a statement about the running program, which is why it is decisive about what is
  running and silent about what a package numbered it.

## Consequences

- **Positive:** a host the fleet can see is behind is reachable again, by upgrading the Server alone
  and without cutting a release for it. The case that prompted this — a Client on 0.4.0 claiming
  0.4.1 — resolves by offering 0.4.1 once more.
- **Positive:** the rule still needs no knowledge of what kind of thing an Agent is, and it adds no
  new way for a Set to be blocked; every Set that reaches an Agent today still reaches it.
- **Negative / trade-off: an Agent whose program numbers itself below its package version is now a
  candidate for the version it already has.** A Collector reporting `0.98.0` under an `otelcol` Set
  at `2.0.0` shows that Set as waiting for a rollout, and a rollout re-installs bytes it already
  runs. The fleet cannot tell that case apart from the false claim this ADR is about — both are "the
  program denies running the version its package status names" — so the operator sees it, decides,
  and the refusal message says which two versions were read.
- **Negative / trade-off:** the fourth test is now two comparisons over two reports instead of one,
  and "why does this Agent see this Set" takes one more step to answer.
- **Follow-ups:** an explicit operator override for a genuine reinstall (offer a Set the test would
  otherwise hold back) is left open; so is ADR-0079's own open question, whether an Agent should be
  able to state that a version it reports is not under package management. Whether the Client should
  refuse to commit a self-update whose version the process that came up does not report as its own
  is a defect in that path rather than a decision, and is fixed with this ADR's point 5.
