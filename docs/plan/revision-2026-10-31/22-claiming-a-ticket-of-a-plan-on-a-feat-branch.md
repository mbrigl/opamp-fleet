# Ticket: Decide how a ticket of a plan that lives on a `feat/` branch is claimed

## What to build

`propose-adr` is needed. The plan for several Server instances lives on `feat/multi-instance`, and
`.agents/skills/work-tickets/claim.sh` refuses a ticket that is not on `main`, as ADR-0006 decides
("A ticket is claimable once it is on `main`"). [`AGENTS.md` §6](../../../AGENTS.md#6-project-rules)
lets branches integrate first on a `feat/` branch, so a plan can be worked there before it reaches
`main`; the ticket branch was then created by hand on the feat branch, outside the script and the
frontier sensor. The script does what the ADR says: this is a gap between two rules, not a bug.

Propose the decision: either a ticket is claimable from the `feat/` branch that carries its plan,
and the claim script takes that branch as its base, or a plan always lands on `main` first, and the
hand-made claim was wrong. Either way the claim script, the `work-tickets` skill and the frontier
sensor follow the ADR in its implementation.

The human decided for the first: a ticket is claimable from the `feat/` branch that carries its
plan, and the claim script and the frontier sensor take that branch as the base. The ADR is
proposed for the human to accept.

## Acceptance criteria

- [ ] Given the proposed ADR, when the human has decided it, then its commits are ADRs only, and the
  claim script, the skill and their self-test follow it on the same branch.
- [ ] Given a ticket on a `feat/` branch, when it is claimed after the decision, then the claim
  script either takes it from that branch or refuses it with a message that names the decided way.

## Read first

- ADR-0006 (Decision, the claim), ADR-0007 clause 6,
  [`AGENTS.md` §6](../../../AGENTS.md#6-project-rules) on integration branches.
- `.agents/skills/work-tickets/SKILL.md` and `claim.sh`, `scripts/test-sensor-plan-frontier.sh`.
- The `propose-adr` skill.

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human. It needs an ADR the human decides.
