# Harness

Which agents and models work in the project, the permission mode they run under, who reviews
what, and what may run unattended. Every project decides it at setup, because every later session
works inside it. Its **guides** steer an agent before it acts: the rule file, the skills, the
golden path. Its **sensors** check what the agent did: the computational ones, tests and checks
that are deterministic and cheap enough to run on every change, and the inferential one, the
review in a fresh context. The harness ADR is the one
[`AGENTS.md` §4](../../../../AGENTS.md#4-working-style) names for the parts kept with the human
and the cap on open agent work.

## Signals

- A new project takes its founding decisions.
- A second agent, a second human, or parallel sessions start working in the repository.
- A ticket asks to run unattended, and nothing says what may.
- Reviews pile up faster than the humans read them.
- Proposed ADRs wait on the one human who may decide them.

## Fits when

- **One agent, the human reviews every change**: one agent and model; the human reads every pull
  request; nothing runs unattended; no parallel sessions; a mini-ADR. A tool, a library, or a
  one-person project starts here and grows the harness when the work does.
- **Implementer and reviewer apart**: one agent and model implements, another reviews in a fresh
  context, a stronger model where one exists; the parts the specification's *Threats & Forbidden
  Actions* protects get a second reviewer of a different kind and the human who owns the part.
- **Parallel tickets under a cap**: tickets whose *Mode* allows it run without the human
  ([ADR-0006](../../../../docs/adr/0006-feature-layer.md)), several at once; a dispatching
  session claims each from the frontier by creating its branch together with a worktree, or
  a container, of its own, and starts a fresh session of the implementing agent in each; where
  several humans or machines dispatch from clones of their own, the claim is made on the
  shared remote, the one push the ADR grants without an instruction per push; the
  open agent work is capped at what the humans can review. The
  [`work-tickets`](../../work-tickets/SKILL.md) skill is the order of that work, and every
  agent that reads skills and runs from a command line can be dispatcher or worker.

Whichever fits, the ADR answers: who decides ADRs and lands changes to the specification, one
maintainer or several, and where several, which of them decides for which part of the system,
named by the paths an ADR's *Applies to* names, with the code owners of `docs/adr/` and the
specification being exactly those who may decide and each ADR's `Deciders` line naming the one
who did; which agent and model implements and which
reviews, and the
command that starts a session of each without a human, such as the agent's non-interactive
mode; how parallel sessions are kept apart, if there are any, and on which remote claims on
tickets are made where the sessions do not all share one repository; how much agent work may
be open at once; and which parts of the system are never worked unattended however small the
ticket, such as authentication and authorization, payment, public interfaces, data migrations,
and whatever else the specification's threats protect.

## Does not fit

- Unattended work without a cap: with agents the bottleneck moves from writing to reviewing, and
  a harness that outruns the review produces a backlog, not software.
- The implementer reviewing its own change:
  [`AGENTS.md` §5](../../../../AGENTS.md#5-quality-bar--definition-of-done) excludes it whatever
  the harness says.
- A gate in one agent's own configuration as the enforcement of a rule
  ([`AGENTS.md` §6](../../../../AGENTS.md#6-project-rules)).

## The rule a test decides

None, as a rule: who reviews, who decides which ADRs, and what runs unattended is decided
outside the tree. The ADR says so in a `**Not mechanically decidable:**` paragraph
([ADR-0002](../../../../docs/adr/0002-decisions-verified-by-tests.md)). What the harness relies
on mechanically is already checked: the git hooks, the required checks, and the Code Owners
review of the repository settings.

## Sensors

The review skill, the inferential sensor; the git hooks under `.githooks/`; the required checks
and the Code Owners review listed under *Repository settings* in the README;
[`scripts/sensor-plan-frontier.sh`](../../../../scripts/sensor-plan-frontier.sh), which lists
the tickets that may be claimed, for the dispatching session.

## Alternatives

The three forms above grow into each other. A process-owning framework that brings agents,
artifacts, and pipeline in one piece is a fourth; ADR-0003 and ADR-0006 decline it for the
template.

## Sources

- Birgitta Böckeler, *Harness engineering* —
  <https://martinfowler.com/articles/harness-engineering.html>: guides that feed forward and
  sensors that feed back, and committing early to what the sensors can hold.
- Addy Osmani, *Agentic autonomy levels* — <https://addyosmani.com/blog/agentic-autonomy-levels/>:
  how quickly a wrong result is noticed, how cleanly it is undone, what would prove it right.
