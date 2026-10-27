---
name: plan-feature
description: Plan work larger than one agent session in the tree under docs/plan/ — a feature spec cut into tracer-bullet tickets, files deleted when their work lands. Use when a request will not fit one session, or when the human asks for a spec, tickets, or a plan.
---

# Plan a feature

The rule is [`AGENTS.md` §4](../../../AGENTS.md#4-working-style); the artifacts and their
lifecycle are decided in [ADR-0006](../../../docs/adr/0006-feature-layer.md). The shapes are the
templates [`spec.md`](spec.md) and [`ticket.md`](ticket.md) next to this file. This skill is the
order of work and states no rule of its own
([ADR-0003](../../../docs/adr/0003-procedures-as-skills.md)).

## 1. Decide the artifact

- Fits one session: plan in the conversation, no file.
- Larger: a **feature spec** with **tickets** (step 2).

The threshold is a judgement under proportionality ([§1](../../../AGENTS.md#1-principles)). Say
which you chose and why. An effort whose questions cannot all be asked yet is not ready for a
spec: chart it with the human in the conversation until the questions run out.

## 2. Write the spec and cut the tickets

1. Interrogate the request with the human before drafting: which goal it serves by identifier,
   what was not said, which alternative nobody named, what happens without it. If the questions
   do not run out, the scope is too big: split first.
2. Copy [`spec.md`](spec.md) to `docs/plan/<spec>/spec.md`, `<spec>` a short kebab-case name, and
   fill it from that conversation: problem and solution from the user's side; the existing
   behaviour it changes, what must stay, and which tests pin it; goals served (`G-n`, `Q-n`);
   user stories; implementation and testing decisions in the project vocabulary, without file
   paths or code; out of scope; ADR impact. A decision that is architecture-relevant is an ADR,
   not a bullet: use `propose-adr`.
3. Cut tickets as vertical slices, each a copy of [`ticket.md`](ticket.md) at
   `docs/plan/<spec>/<NN>-<slug>.md`, numbered in the order they are cut. Each is an end-to-end
   behaviour that is demoable or verifiable on its own, sized to one fresh session, with
   Given/When/Then acceptance criteria, the tickets that block it by path, and *Read first*: the
   ADRs in force whose *Applies to* it touches, the glossary terms and conventions in play, the
   golden path it copies, the seam its tests go on. The first slice through new ground is the
   tracer bullet. Where it takes a decision the catalogue of `propose-adr` has an entry for and no
   ADR in force decides, `propose-adr` runs before it.
4. Set each ticket's *Mode*: "unattended" only under the conditions of
   [`AGENTS.md` §4](../../../AGENTS.md#4-working-style), each answered on the ticket; the stop
   is concrete, the attempts, time, or criterion past which the session hands off. Otherwise
   "with the human", and why.
5. Commit the plan on a branch and hand it to the human; it is on `main` once merged.

Unfinished work a session leaves behind is a single ticket in a directory of its own,
`docs/plan/<slug>/01-<slug>.md`, without a spec.

## 3. Work and close

The tickets are worked through [`work-tickets`](../work-tickets/SKILL.md), one at a time or in
parallel: claimed from the frontier by their branch, each branch deleting its ticket
file. The spec closes only after `design-revision` has run over what its tickets touched and
deleted `spec.md`. Whatever in the spec has to last moves into the specification, an ADR, the
architecture overview, the glossary, or the conventions before it does.
