# ADR-0085: The Client manages only programs it installs — a Managed Process is always the fleet's

- **Status:** 🟡 proposed
- **Date:** 2026-08-19
- **Deciders:** Markus Brigl

Supersedes the **absolute-path row** of [ADR-0021](0021-supervisor-directory-and-path-implied-package-consent.md)'s
clause 2 and, with it, the "declined" half of its clause 4. Everything else ADR-0021 decides — the
relocatable Supervisor root, one directory per Supervisor, `program/` over `bin/`, staging beside
the program so an install is a rename — stands unchanged.

Supersedes the machine-installed route of
[ADR-0063](0063-the-glpi-agent-is-supervised-by-the-command-kind.md) and the machine-installed fallback of
[ADR-0068](0068-icinga-2-is-supervised-by-a-kind-of-its-own.md). Amends
[ADR-0023](0023-multi-file-packages.md) and
[ADR-0059](0059-a-removed-supervisor-is-purged.md) where they restate the three-way rule, and makes
[ADR-0057](0057-server-pushed-supervisor-blocks-name-only-client-owned-programs.md) tautological
without repealing it. [ADR-0022](0022-supervisor-path-placeholders-in-process-arguments.md)'s
placeholders are untouched and become the only way to name a path in an argument.

## Context

ADR-0021 made the *shape* of a program's path the whole of a host's consent to package updates. A
bare file name means the program lives in the Supervisor's own directory, the Client owns it, and
the Agent declares `AcceptsPackages`. An absolute path means someone else's file — a distribution
package, configuration management — spawned but never written to, with no package capability
declared.

Six decisions have been built on the second row in the two weeks since — and it has become the seam
along which this Client does two different jobs. That the seam spread this fast is the argument for
closing it now rather than later.

**A fleet that cannot update an Agent is supervising, not managing.** The absolute-path route
delivers health, restart and central configuration, and explicitly *not* updates: ADR-0063 states it
plainly — "GLPI Agent updates stay with the machine's package manager, invisible to the fleet." That
is a coherent product, but it is a different one. Every capability this project builds for the
managed case — Selector-targeted packages (ADR-0017), the version rules (ADR-0083), rollback
(ADR-0058), the health gate (ADR-0015) — stops at the boundary, and every feature has to be reasoned
about twice: once for a program the Client owns and once for a program it merely runs.

**The rule is invisible where it matters and irreversible where it does not.** An operator who
"fixes" a path to an absolute one silently revokes a fleet-visible capability; ADR-0021 clause 4
added a startup log line precisely because nothing else makes the derived state readable. And the
Server cannot correct it: ADR-0057 already refuses to deliver a block naming an absolute path, so a
host that has drifted into the unmanaged shape can only be fixed by hand, on the host.

**The two principals have already diverged.** ADR-0057 draws the line where it does — the operator
may write an absolute path locally, the Server may not deliver one — because the local file is the
operator's authority. That is right as far as it goes, but it means the fleet's model of a host
depends on which of two files the block came from, and only one of them the fleet can see.

**What makes this decidable now is that the alternative exists.** ADR-0070 repacks vendor software
as relocatable trees, and ADR-0064 does it for GLPI Agent on both platforms. When ADR-0021 was
written there was no way to bring a vendor agent under fleet ownership; today there is, and it has
been exercised. The absolute-path row is no longer the only route to a vendor agent — it is the
route that keeps one outside the fleet.

## Decision

We will require every Managed Process to be a program **this Client installs and owns**. The
program's path is a bare file name; an absolute path is a startup error naming the rule.

1. **One shape, and it means what it always meant.** `binary` and `command` take a bare file name —
   no path separator, no `..` — resolving to `<supervisor_dir>/<name>/program/<value>`, or
   `program/tree/<program_path>` for a multi-file package (ADR-0023, unchanged). The Client owns
   that directory, so it may replace what is in it, and **every** Agent therefore declares
   `AcceptsPackages`. There is no second row and no third.

2. **An absolute path is refused at startup**, naming both the rule and the way across: the program
   belongs to the machine, and a program the fleet is to manage must be delivered as a package
   (ADR-0018, ADR-0070). The message says which block, which value, and what to do — it is the
   only notice an operator upgrading into this change will get, so it carries the whole
   explanation rather than a rule number. The Windows drive-relative case ADR-0021 rejected
   (`\Program Files\…`, no drive letter) folds into the same error; it was only ever a
   near-miss of the absolute form.

3. **Consent stops being derived, because there is nothing left to derive.** ADR-0021's clause 4
   log line loses its "declined" half and becomes a statement of where the program is. The
   `AcceptsPackages` capability is a constant of this Client, not a function of its configuration —
   which is what ADR-0021 clause 2 was for, and it is now discharged by the type system rather than
   by a rule.

4. **The Server's delivery check stays, and stops being able to fire.**
   ADR-0057's rule — a delivered `[[supervisor]]` block must name a program the Client owns — is now
   satisfied by every block that parses. The check remains as defence in depth against a future
   shape nobody has thought of yet, with a comment saying so; deleting a guard because it currently
   cannot trigger is how it comes back.

5. **`[self_update]` keeps naming its package explicitly.** ADR-0021's asymmetry is preserved and its
   reasoning is unchanged: a package written over the Client takes the host out of reach, which is
   exactly where implicit consent would be wrong. Nothing here touches ADR-0075's consent rule.

6. **A vendor agent is brought in by repacking, and that is the supported route.** ADR-0064 for GLPI
   Agent and ADR-0070 for Icinga 2 are what an operator uses instead. The manual's
   machine-installed walkthroughs are replaced by the fleet-delivered ones, not merely deleted, so
   every case the old route documented has a page describing the new one.

## Alternatives considered

- **Keep the rule and leave it to operators.** The status quo, and it costs nothing today. Rejected
  because the cost is not in the rule but in everything built beside it: every package, version and
  rollback decision carries a second case, and the fleet's picture of a host depends on which file
  a block came from. The seam does not stay still — ADR-0079 already had to reason about hosts
  crossing it.

- **Deprecate rather than remove — warn at startup, remove later.** Genuinely tempting, and it was
  the recommendation until the field was checked: with nothing installed, there is no host to warn.
  A deprecation period protects deployments, and there are none. What it would buy instead is time
  to prove ADR-0070 on Windows — see the trade-off below, which is the real cost of not waiting.

- **Keep absolute paths for *supervision only* — a documented, capability-less mode.** This is what
  the rule already is, named honestly. Rejected because naming it does not reduce it: the two cases
  still exist in the code, in every feature's reasoning, and in the fleet view. If the mode is worth
  having, it is worth having as its own decision with its own model, not as a fallthrough in a path
  parser.

- **Let the Server deliver absolute paths too, and drop ADR-0057's restriction instead.** The
  opposite resolution of the same asymmetry: make both principals equal by widening rather than
  narrowing. Rejected outright — it hands a Server the ability to run any binary on any host by
  absolute path, which is the escalation ADR-0057 exists to prevent.

## Sources / Prior art

- **ADR-0021's own framing** — that the path's shape *is* the consent — is what makes removal a
  single-row change rather than a redesign: there is no separate capability flag to retire, because
  ADR-0021 deliberately refused to add one (its retired `accepts_packages` key is still refused
  loudly by the loader).
- **ADR-0070's constraints** are the honest measure of what replaces the removed route: glibc cannot
  be bundled, so a relocatable tree is per distribution family, and ADR-0023 forbids symlinks and
  hard links in a package tree. Repacking is a real path, not a free one.
- **The OpAMP Baseline's `AcceptsPackages`** is a per-Agent capability, not a per-fleet one; making
  it constant here is a narrowing of this implementation, not of the protocol, and a future decision
  could widen it again without a wire change.
- **Elastic Agent and the OpenTelemetry Collector's own supervisor** both manage only binaries they
  install; neither offers a supervise-but-never-update mode for a distribution-packaged program.

## Consequences

- **Positive: one kind of Managed Process.** Every feature that touches packages — targeting,
  versions, rollback, the health gate, the version probe — has one case to reason about. The
  `owned` flag and the branches reading it leave the code, and with them the class of bug where a
  capability depends on how a path was spelled.

- **Positive: the fleet's picture of a host stops depending on which file a block came from.** With
  ADR-0057's restriction now matching what the loader accepts, a locally written block and a
  Server-delivered one describe the same kind of thing.

- **Positive: a silent revocation becomes impossible.** An operator who writes an absolute path gets
  a refusal at startup instead of an Agent that comes up managed-looking and quietly takes no
  packages.

- **Negative: the supervise-but-do-not-update use case is gone**, and it was a first-class,
  documented route. An operator who wants the host's package manager to keep owning an agent's
  updates while the fleet watches and configures it has no way to say so. This is the deliberate
  content of the decision, not a side effect: that operator must either repack the agent or not
  manage it here.

- **Negative: Icinga 2 on Windows loses a fallback that was designed in.** ADR-0068 kept the
  absolute form in scope from the start because "Windows is unproven — if the MSI payload cannot be
  relocated, the same kind supervises a machine-installed Icinga 2 there." That safety net is
  removed before the thing it protected against was resolved. If the Windows repack proves
  impossible, the Icinga 2 kind is Linux-only until some other decision addresses it — and this ADR
  should be revisited rather than worked around.

- **Negative: two manual walkthroughs must be rewritten, not deleted.** `glpi-agent.md`'s Linux and
  Windows machine-installed examples and `icinga2.md`'s fallback describe working setups; their
  replacements are the fleet-delivered routes, which exist but are less familiar. A page that simply
  loses its example leaves an operator with no path at all.

- **Negative: adoption gets harder.** Taking over a host that already runs a vendor agent used to be
  a one-line block naming the existing binary; it is now a repack plus a package rollout before the
  Agent appears in the fleet at all.

- **Follow-ups:** whether a supervise-only mode should exist as its own decision — with its own
  capability model rather than as a path-parser fallthrough — is left open and would need one. So is
  what happens to the Icinga 2 kind if the Windows repack cannot be made to work. Neither is decided
  here.
