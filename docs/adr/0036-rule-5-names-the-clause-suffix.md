# ADR-0036: Rule 5 of `AGENTS.md` names the clause suffix among the edits it permits

- **Status:** 🟢 accepted
- **Date:** 2026-10-27
- **Deciders:** Markus Brigl
- **Applies to:** rule 5 of the ADR rules in `AGENTS.md`

## Context

[ADR-0007](0007-an-adr-may-supersede-named-clauses-of-another.md) lets an ADR supersede named
clauses of another, and has the old ADR's `Status` line gain a suffix naming the successor, added in
the change that proposes it. Its clause 2 calls that suffix the `Status` line edit rule 5 of
[`AGENTS.md` §3](../../AGENTS.md#3-adr-rules) permits, and its *Out of scope* leaves `AGENTS.md`
unchanged.

Rule 5 as the template states it permits two edits of an accepted ADR, both by a human: flipping
the `Status` line, and adding the project's decider to an inherited ADR. A suffix is neither: it
leaves the status as it is and lands with a proposed ADR, written by whoever writes that ADR. A
reader of rule 5 alone takes the suffix for an edit the rules forbid.
[ADR-0001](0001-agent-governance-model.md) has an ADR on the ADR process edit the block of
[`AGENTS.md` §3](../../AGENTS.md#3-adr-rules) in the change that lands it, so the rule file can say
it.

## Decision

We will name the clause suffix of ADR-0007 in rule 5 of
[`AGENTS.md` §3](../../AGENTS.md#3-adr-rules) as a third permitted edit: an ADR that supersedes
named clauses of another adds, in the change that proposes it, the suffix
naming it to that ADR's `Status` line.

**Out of scope:** the form of the suffix and the mechanics of a partial supersession, which
ADR-0007 decides; who flips a status, which rule 4 decides.

## Alternatives considered

- **Leave rule 5 as it is.** ADR-0007 then permits what the rule file, which governs, does not
  name, and the two disagree for every reader who reads one of them.
- **Read the suffix as part of the status flip.** The status does not change with a suffix, and
  the flip is a human's, while the suffix lands with the proposed successor.

## Sources / Prior art

None — a wording of the rule file that follows ADR-0007.

## Consequences

- Positive: the rule file and ADR-0007 say the same; check 8 of `scripts/check-docs.sh` holds the
  suffix's form as before.
- Negative / trade-offs: the block of [`AGENTS.md` §3](../../AGENTS.md#3-adr-rules) departs from the
  template's wording in one
  sentence, which a template release taken up later keeps, as the `normalize` skill's entry point
  for a release says.
- Follow-ups: none.

## Enforcement

**Not mechanically decidable:** that rule 5 names the edit is the wording of the rule file, which
review holds. The suffix's form and its agreement with the successor's `Supersedes` field are
check 8 of [`scripts/check-docs.sh`](../../scripts/check-docs.sh), as ADR-0007 states.
