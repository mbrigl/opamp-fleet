---
name: work-tickets
description: Work the tickets under docs/plan/, each claimed by its branch and worked in a fresh session, alone or several in parallel worktrees. Use when a session is to pick up a ticket, when the human asks to work the plan or to run agents in parallel, or when you are started in a worktree with a ticket path.
---

# Work tickets

The rules are [`AGENTS.md` §4](../../../AGENTS.md#4-working-style) and
[§5](../../../AGENTS.md#5-quality-bar--definition-of-done); the frontier and the claim are
decided in [NUC-large-work-is-planned §4](../../../docs/adr/NUC-large-work-is-planned.md); who
works, in parallel or not, how many at once, and what never runs unattended is the project's
harness ADR, the one the [harness entry](../propose-adr/concepts/harness.md) of the catalogue
produced. This skill is the order of work and states no rule of its own
([NUC-procedures-are-skills §3](../../../docs/adr/NUC-procedures-are-skills.md)).

Two roles: the **dispatcher** claims tickets and starts the sessions that work them; a
**worker** works one ticket. A worker started with a ticket path skips to step 2.

A project that plans in its host's issues, as its harness ADR records
([NUC-large-work-is-planned §9](../../../docs/adr/NUC-large-work-is-planned.md)), reads its
frontier from the host's list of open, unblocked, unassigned ticket issues instead of the sensor,
and claims a ticket by assigning its issue instead of creating its branch, only where the harness
ADR grants that write. Where these steps delete the ticket file, the pull request names the issue
with a closing keyword (`Closes #<n>`), and the human's merge closes it; no session closes an issue
itself. The rest of this skill holds as written.

## 1. Read the frontier and claim

1. Where the harness ADR has claims made on a remote, fetch it first
   (`git fetch --prune <remote>`), so that the claims of other clones are seen. Run
   [`scripts/sensor-plan-frontier.sh`](../../../scripts/sensor-plan-frontier.sh). Its
   frontier is what may be claimed; the claimed and the blocked are not.
2. Choose. Alone, one ticket. In parallel, only tickets whose *Mode* says unattended and that touch
   nothing the harness ADR keeps with the human, and no more than its cap on open agent work allows,
   counting the branches of agents whose work is not yet on `main`, claims included. Among those,
   only tickets whose `Touches:` lines share no path with each other, with those of the tickets
   other `ticket/` branches claim, as each branch's ticket file says, or with the diff of any other
   branch still open, since two that collide cost a rework and its review; a ticket whose *Read
   first* names no paths is dispatched only while no other ticket is claimed. A ticket "with the
   human" is worked in a session the human sits in, never dispatched.
3. Claim each with [`claim.sh`](claim.sh): `.agents/skills/work-tickets/claim.sh <ticket path>`
   creates the branch `ticket/<spec>/<NN>-<slug>` at `main`. Only a ticket on `main` is
   claimable ([NUC-large-work-is-planned §4](../../../docs/adr/NUC-large-work-is-planned.md)):
   the worker's branch starts there, and other clones read the frontier there.
   - Among sessions that share the repository, the claim is the local branch, which git refuses
     to create twice. It needs no commit and nothing pushed.
   - Among clones that share a remote, pass the remote the harness ADR names as a second
     argument. The script claims on the remote first, by a push that only creates the branch,
     with an empty commit of its own on top of the remote's `main`: a push of a commit the branch
     already points at succeeds without creating anything, and two clones would both win. That
     push skips the pre-push hook, since its commit carries the remote's `main` tree unchanged.
     It is the one push the harness ADR may grant without an instruction per push
     ([NUC-main-only-via-pr §2](../../../docs/adr/NUC-main-only-via-pr.md)).

   Exit code 2 says another session was first: choose again. Then isolate the claim in a
   worktree, `git worktree add .scratch/worktrees/<spec>-<NN>-<slug> ticket/<spec>/<NN>-<slug>`,
   in the ignored `.scratch/`, so it stays where the repository is mounted. Should that fail, the
   claim is still this session's: clear the path and add the worktree again, rather than
   reading the branch as another session's. Where the sessions share neither a repository nor a
   remote, the harness ADR says how the claim is made instead.
4. In parallel, start one fresh session per worktree, of the agent and model the harness ADR
   names for implementing, with the command it gives for that agent, inside the worktree and
   handed only the ticket's path and this skill. The dispatcher's session does no ticket's
   work, so the review of each stays with a session that did not write it
   ([§5](../../../AGENTS.md#5-quality-bar--definition-of-done)). It waits for each session to
   end rather than asking after it, and reads its hand-off, never its transcript. A session that
   ends without one has handed its ticket back: the dispatcher leaves the claim for the human.

## 2. Work one ticket

1. Read the ticket and what its *Read first* names, and nothing else up front. Check the paths it
   touches against the tree first, since a path goes stale where the behaviour it serves does
   not, and correct them in the ticket file; then read the ADRs that bind them through
   [`scripts/sensor-binding-adrs.sh`](../../../scripts/sensor-binding-adrs.sh) `--sections`,
   with a `--path` for each, the rest by the sections it names.
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
2. The branch reaches `main` by its pull request, or is rebased onto the `feat/` branch the human
   names ([NUC-main-only-via-pr §5](../../../docs/adr/NUC-main-only-via-pr.md)). Its hand-off is
   the pull request's description; on a `feat/` branch, the description of that branch's pull
   request gathers it. A ticket whose spec has no ticket file left hands that spec to
   [`design-revision`](../design-revision/SKILL.md).
3. Before it removes a finished worktree, the dispatching session copies the ticket's hand-off
   draft from the worktree's `.scratch/` to `.scratch/<spec>-<NN>-<slug>.md` of the main checkout.
   There the pull request of a `feat/` branch gathers it
   ([Integration branches](../../../CONTRIBUTING.md#integration-branches), step 6). Then remove
   the worktree (`git worktree remove .scratch/worktrees/<spec>-<NN>-<slug>`) and, once its work
   is on `main`, its branch. A ticket rebased onto a `feat/` branch keeps its branch until then,
   since `main` still has its file and the branch is what claims it. The rebuild of the `feat/`
   branch by topic rewrites its commits, so its deletion then takes `git branch -D`. Prune the
   branches a remote has deleted (`git fetch --prune`), since the sensor reads remote ones as
   claims; run it again, and claim from the new frontier.
   A claim given up without landing is released by deleting its branch, only on the human's
   instruction ([NUC-main-only-via-pr §2](../../../docs/adr/NUC-main-only-via-pr.md)); on a
   remote, the human deletes it, since the push the harness ADR grants only creates.
