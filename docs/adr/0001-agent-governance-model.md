# ADR-0001: Specification + ADRs governed through a single `AGENTS.md`

- **Status:** 🟢 accepted
- **Date:** 2026-06-20
- **Deciders:** Maintainer
- **Note:** Records a decision already embodied in this template. Documented retroactively so the
  template follows its own method; accepted by the maintainer on 2026-06-20.

## Context

Several coding agents (Claude Code, Codex, Cursor, Mistral Vibe, GitHub Copilot, OpenCode) may work
in this repository. Without explicit, written governance their behaviour drifts, intent stays implicit,
and structural decisions are made silently and become hard to review. Each agent also looks for its
instructions in a different place, which invites duplicated and diverging rule sets.

## Decision

We will make [`docs/SPECIFICATION.md`](../SPECIFICATION.md) the constitution, record every
architecture-relevant decision as an ADR that derives from it, and expose **one** vendor-neutral
[`AGENTS.md`](../../AGENTS.md) as the single source of agent rules. Per-agent files exist only as thin
pointers where a tool cannot read `AGENTS.md` natively (currently only `.claude/CLAUDE.md`), and they
contain no rules of their own.

## Alternatives considered

- **Per-tool instruction files (one rule set each)** — guarantees drift between agents and multiplies
  maintenance.
- **Rules embedded in `README.md`** — mixes human onboarding with agent governance and bloats both.
- **No written governance** — maximal agent drift and unreviewable, implicit decisions.

## Sources / Prior art

- The `AGENTS.md` convention adopted by multiple agent vendors — <https://agents.md/>.
- Architecture Decision Records as introduced by Michael Nygard and collected at
  <https://adr.github.io/>.

## Consequences

- Positive: one place to change a rule; consistent agent behaviour; decisions are explicit and
  reviewable; onboarding humans and agents read the same documents.
- Negative / trade-offs: one pointer file (`.claude/CLAUDE.md`) and the ADR index must be kept in
  sync by hand; the discipline only pays off if contributors actually write ADRs.
- Follow-ups: keep per-agent pointers minimal; revisit if Claude Code gains native `AGENTS.md`
  support (tracking issue anthropics/claude-code#34235).
