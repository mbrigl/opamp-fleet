# ADR-0007: One maintainer decides and merges, any agent that reads AGENTS.md implements, a fresh context reviews every change, and nothing is worked unattended

- **Status:** 🟡 proposed
- **Date:** 2026-10-26
- **Deciders:** Markus Brigl
- **Applies to:** every session that works in the repository and every pull request, `AGENTS.md`, `.claude/`, the `review` skill under `.agents/skills/review/`, and `.github/CODEOWNERS`

## Context

Every session works inside the harness: which agents work in the project, who reviews what, who
decides, and what may run unattended. [`AGENTS.md` §4](../../AGENTS.md#4-working-style) names the
harness ADR as the place for the parts kept with the human and for the cap on open agent work, and
lets a task run unattended only when its ticket fits under both.

The profile of the project is complex: its work spans sessions, and the specification's
*Threats & Forbidden Actions* name assets worth protecting, among them every host the fleet
reaches and the operator's signing key. But it has one maintainer: the specification's
*Constraints* say that one maintainer decides and that coding agents do most of the work under the
rules of [`AGENTS.md`](../../AGENTS.md). The harness has to fit both: room for more than one agent
and for parallel sessions, and a review and a merge that one human can keep up with.

What the repository shows:

- `AGENTS.md` is the rule file, written for no vendor. The only agent configuration in the tree is
  Claude Code's under `.claude/`, whose `CLAUDE.md` points to `AGENTS.md` and whose `skills/` holds
  one pointer per skill into `.agents/skills/`.
- The `Deciders` lines of ADR-0007 to ADR-0034 name Markus Brigl. Those of the template ADRs
  ADR-0001 to ADR-0006 name the NUC maintainer and Markus Brigl, who took them up for the project.
- [`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done) requires a review in a fresh
  context before a change is handed off, through the `review` skill, and never by the session that
  wrote the change.
- Changes reach `main` only through a pull request
  ([`AGENTS.md` §6](../../AGENTS.md#6-project-rules)).
  [`.github/CODEOWNERS`](../../.github/CODEOWNERS) names the maintainer for
  `docs/SPECIFICATION.md` and `docs/adr/`.
- No ticket is dispatched, and `docs/plan/` holds no feature spec or ticket.

## Decision

We will keep the harness at implementer and reviewer apart: one maintainer decides and merges, any
agent that reads `AGENTS.md` implements, a fresh context reviews every change, and nothing is
worked unattended.

1. **One maintainer decides.** Markus Brigl decides every ADR and lands every change to the
   specification. `.github/CODEOWNERS` names him for `docs/SPECIFICATION.md` and `docs/adr/`. The
   `Deciders` lines of the template ADRs ADR-0002 to ADR-0006 name only the NUC maintainer until
   he adds himself.
2. **Any agent that reads `AGENTS.md` may implement.** `AGENTS.md` is the one rule file, and no
   agent or model is prescribed. An agent's own configuration, such as Claude Code's under
   `.claude/`, points to `AGENTS.md` and the shared skills and states no rule of its own.
3. **Every change is reviewed in a fresh context before its pull request opens,** through the
   `review` skill, by a session other than the one that wrote it.
4. **The maintainer reviews and merges every pull request.**
5. **Nothing is worked unattended.** No session works without a human, and no ticket is dispatched
   to one; the cap on open unattended agent work is zero, so every part of the system is kept with
   the human.
6. **Parallel sessions work in git worktrees of their own.**

**Out of scope:** which model implements or reviews; a second reviewer of a different kind for the
parts the specification's threats protect; how many attended sessions may be open at once; the
command that starts a session without a human, and the remote on which a ticket is claimed, which
matter only once work runs unattended.

## Alternatives considered

- **One agent, the human reviews every change.** The smallest form: one agent and model, the human
  reading every pull request, nothing unattended, no parallel sessions. It leaves no room for a
  second agent or for sessions that run in parallel.
- **Parallel tickets under a cap.** The form a complex project starts from: tickets whose *Mode*
  allows it run without the human, several at once, each claimed by a dispatching session in a
  worktree of its own and worked through the `work-tickets` skill, with the open agent work capped
  at what the humans can review. Not taken: no work runs unattended here, and no plan is kept under
  `docs/plan/`. Taking it up changes this decision.
- **A process-owning framework that brings agents, artifacts and pipeline in one piece.** Declined
  by [ADR-0003](0003-procedures-as-skills.md) and [ADR-0006](0006-feature-layer.md).
- **A gate in one agent's own configuration as the enforcement of a rule.** A gate one agent
  honours and the next does not looks like a rule and is not one
  ([`AGENTS.md` §6](../../AGENTS.md#6-project-rules)).
- **Superseding named clauses of an ADR, the rest staying accepted.** A detail of a long ADR
  would change by a short successor, the old ADR's `Status` line naming the clauses it replaces.
  Not taken: the template supersedes an ADR whole
  ([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules), rule 5), and a mechanic beside the template's
  is carried through every release of it taken up. A detail of a long ADR changes by a successor
  that restates it whole.

## Sources / Prior art

- Birgitta Böckeler, *Harness engineering*:
  <https://martinfowler.com/articles/harness-engineering.html>, guides that feed forward and
  sensors that feed back, and committing early to what the sensors can hold.
- Addy Osmani, *Agentic autonomy levels*:
  <https://addyosmani.com/blog/agentic-autonomy-levels/>, how quickly a wrong result is noticed,
  how cleanly it is undone, what would prove it right.
- The harness entry of the `propose-adr` skill's concept catalogue,
  [`.agents/skills/propose-adr/concepts/harness.md`](../../.agents/skills/propose-adr/concepts/harness.md):
  the three forms and the questions a harness ADR answers.

## Consequences

- Positive: a session knows who decides, who reviews and that it works with a human; any agent
  that reads `AGENTS.md` can take up the work, and parallel sessions do not share a working tree.
- Negative / trade-offs: every change waits on one human's review and merge, and so does every
  proposed ADR. No ticket can run unattended under
  [`AGENTS.md` §4](../../AGENTS.md#4-working-style) until this decision changes.
- Follow-ups: the model that implements and the one that reviews; a second reviewer for the parts
  the threats protect; a cap on attended parallel sessions; unattended tickets under a cap, once
  the work outgrows one maintainer's attended sessions.

## Enforcement

**Not mechanically decidable:** who reviews, who decides which ADRs, and what runs unattended is
decided outside the tree. What the harness relies on mechanically is checked elsewhere: the git
hooks under [`.githooks/`](../../.githooks/), the required checks, and the Code Owners review
listed under [Repository settings](../../README.md#repository-settings), which
[`scripts/sensor-repo-settings.sh`](../../scripts/sensor-repo-settings.sh) compares with the
repository on a human's instruction.
