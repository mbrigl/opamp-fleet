---
name: work-tickets
description: Work the tickets under docs/plan/ — one ticket in a session of its own, or several at once, each claimed by its branch in a worktree of its own and worked by a fresh agent session. Use when a session is to pick up a ticket, when the human asks to work the plan or to run agents in parallel, or when you are started in a worktree with a ticket path.
---

# Work tickets

The rules are [`AGENTS.md` §4](../../../AGENTS.md#4-working-style) and
[§5](../../../AGENTS.md#5-quality-bar--definition-of-done); the frontier and the claim are
decided in [ADR-0006](../../../docs/adr/0006-feature-layer.md); who works, in parallel or not,
how many at once, and what never runs unattended is the project's harness ADR, the one the
[harness entry](../propose-adr/concepts/harness.md) of the catalogue produced. This skill is the
order of work and states no rule of its own
([ADR-0003](../../../docs/adr/0003-procedures-as-skills.md)).

Two roles: the **dispatcher** claims tickets and starts the sessions that work them; a
**worker** works one ticket. A worker started with a ticket path skips to step 2.

## 1. Read the frontier and claim

1. Where the harness ADR has claims made on a remote, fetch it first
   (`git fetch --prune <remote>`), so that the claims of other clones are seen. Run
   [`scripts/sensor-plan-frontier.sh`](../../../scripts/sensor-plan-frontier.sh). Its
   frontier is what may be claimed; the claimed and the blocked are not.
2. Choose. Alone, one ticket. In parallel, only tickets whose *Mode* says unattended and that
   touch nothing the harness ADR keeps with the human, and no more than its cap on open agent
   work allows, counting the branches of agents whose work is not yet on `main`, claims included. A
   ticket "with the human" is worked in a session the human sits in, never dispatched.
3. Claim each with [`claim.sh`](claim.sh): `.agents/skills/work-tickets/claim.sh <ticket path>`
   creates the branch `ticket/<spec>/<NN>-<slug>` at `main`, and with the remote the harness
   ADR names as a second argument creates it on that remote first, so the claim holds across
   clones. Exit code 2 says another session was first: choose again. Then isolate the claim in a
   worktree, `git worktree add .scratch/worktrees/<spec>-<NN>-<slug> ticket/<spec>/<NN>-<slug>`,
   in the ignored `.scratch/`, so it stays where the repository is mounted. Should that fail, the
   claim is still this session's: clear the path and add the worktree again, rather than
   reading the branch as another session's. Where the sessions share neither a repository nor a
   remote, the harness ADR says how the claim is made instead.
4. In parallel, start one fresh session per worktree, of the agent and model the harness ADR
   names for implementing, with the command it gives for that agent, inside the worktree and
   handed only the ticket's path and this skill. The dispatcher's session does no ticket's
   work, so the review of each stays with a session that did not write it
   ([§5](../../../AGENTS.md#5-quality-bar--definition-of-done)).

## 2. Work one ticket

1. Read the ticket and what its *Read first* names, and nothing else up front.
2. Turn each acceptance criterion into a failing test at the seam the ticket names, then make it
   pass, along the golden path of [`docs/ARCHITECTURE.md`](../../../docs/ARCHITECTURE.md).
3. Stop where the *Mode* says. A session past its stop writes what it found and where it stands
   into the ticket file, commits it on the branch, and hands off instead of trying on.
4. Done when [`scripts/check-all.sh`](../../../scripts/check-all.sh) is green. The change
   deletes the ticket file in the same branch, and the hand-off draft under `.scratch/` says
   which ticket it lands and carries the *Doubts*.

## 3. Review, merge, and the next wave

1. Each finished branch goes to a fresh session that did not write it, through
   [`review`](../review/SKILL.md), of the agent and model the harness ADR names for reviewing.
2. The branch reaches `main` by its pull request, or merges into the `feat/` branch the human
   names ([`AGENTS.md` §6](../../../AGENTS.md#6-project-rules)); its hand-off is the pull
   request's description or the merge's message. A ticket whose spec has no ticket file left
   hands that spec to [`design-revision`](../design-revision/SKILL.md).
3. Remove each finished worktree (`git worktree remove .scratch/worktrees/<spec>-<NN>-<slug>`)
   and, once its work is on `main`, its branch: a ticket merged into a `feat/` branch keeps its
   branch until then, since `main` still has its file and the branch is what claims it; prune
   the branches a remote has deleted (`git fetch --prune`),
   since the sensor reads remote ones as claims; run it again, and claim from the new frontier.
   A claim given up without a merge is released by deleting its branch; on a remote, ask
   the human to delete it, since the push the harness ADR grants only creates.
