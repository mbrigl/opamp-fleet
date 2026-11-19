# NUC-large-work-is-planned: Work larger than one session is planned as a feature spec cut into tracer-bullet tickets, and findings outside the plan go to a backlog

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** every change larger than one agent session, `docs/plan/`, the spec and ticket templates of the `plan-feature` skill, the `work-tickets` skill and its claim script `.agents/skills/work-tickets/claim.sh`, `scripts/sensor-plan-frontier.sh`, the pull request template, `docs/backlog/`, and `scripts/sensor-backlog.sh`

## Context

Authority runs from the specification to the accepted ADRs to the task
([NUC-decisions-are-proposed-then-accepted §7](NUC-decisions-are-proposed-then-accepted.md)).
Nothing in that chain says what this month's work is. A change that fits one session is planned in
the conversation. A change that does not has no place to keep its state: each session starts without
memory, re-derives the plan, and drifts. The rule to cut work into vertical slices
([`AGENTS.md` §4](../../AGENTS.md#4-working-style)) has no artifact that carries a slice, and the
conditions for unattended work have nothing to be written in.

Most work in a system that already exists is change, not addition. A plan written from the
user's side alone says nothing about what the system does today and what must stay. A session
without memory gets that part wrong first.

The artifact cannot live in the specification, which must not churn. It must be disposable,
because a plan that outlives its use is noise for every later session. Any agent must read it
with the tools it already has, without one vendor's tracker or an account on it.

Sessions that run in parallel need to know which tickets another session already works. Findings
that no ticket covers need a place too, or they stay in a conversation or widen the ticket that
found them.

A derived project may already plan in the issues of its host. There the tracker already gives
assignment, sub-issues, and an open list, and a second plan in the tree would split the record.

## Decision

We will plan each change larger than one session as a feature spec cut into tickets, and record
findings outside the plan in a backlog.

1. **Threshold.** A change larger than one agent session must be planned before its first slice,
   as a feature spec cut into tracer-bullet tickets.
2. **Place.** The plan lies under `docs/plan/`, unless the project plans in issues (clause 9).
   - A feature spec is a directory `docs/plan/<spec>/` with `spec.md` and one file
     `<NN>-<slug>.md` per ticket, numbered in the order they are cut.
   - A ticket filed outside a spec lies in a directory of its own without `spec.md`.
   - A directory without files is gone, so `docs/plan/` holds exactly the open work.
3. **Spec and ticket.** A spec and a ticket carry the sections of the `plan-feature` templates.
   - A spec names no file path and no code.
   - A ticket is sized to one fresh session and is demoable or verifiable on its own.
   - A ticket names each blocker by its path, as text. A blocker is closed once its file is gone.
4. **Frontier and claim.** The frontier is the open tickets on `main` whose blockers are closed
   and that no session claims.
   - An agent must claim a ticket from the frontier before any work on it.
   - The claim is the branch `ticket/<spec>/<NN>-<slug>`. It must be visible to every parallel
     session.
   - The `work-tickets` skill carries the steps of a claim and of its release.
5. **Disposal.** The branch that lands a ticket's work must delete its file. Before a ticket's
   or a spec's file goes, what must last moves into the specification, an ADR, the overview, the
   glossary, or the conventions.
6. **Finding.** A finding is a defect, a doubt, or an idea that no ticket covers. A session must
   record it in the backlog and must not widen its ticket for it.
7. **Backlog.** The backlog is files, unless an ADR of the project chooses the host's issues. A
   file entry lies under `docs/backlog/` and holds a title, what was observed, and where. A
   session creates it in a commit of its own.
8. **Triage.** Only the human decides which entry becomes a ticket. The commit that creates the
   ticket must delete the entry.
9. **Plan in issues.** A project may plan in its host's issues instead, recorded in its harness
   ADR. Then these rules hold:
   - Each spec and each ticket is an issue, and a ticket names its blockers as issues.
   - Closing an issue replaces deleting its file.
   - The merge of the pull request that names an issue with a closing keyword closes the issue.
     An agent must not close an issue by a write of its own.
   - The assignment of a ticket's issue is its claim. An agent may assign an issue only under the
     harness ADR's grant, as [NUC-main-only-via-pr §2](NUC-main-only-via-pr.md) allows.
   - The host's list of open, unblocked, unassigned ticket issues is the frontier, in place of
     the frontier sensor.

**Out of scope:**

- work smaller than one session, and an artifact for an effort too foggy for a spec;
- triage beyond whether a finding becomes a ticket;
- the design revision, which [NUC-design-is-revised](NUC-design-is-revised.md) decides;
- the order of work, which the skills `plan-feature` and `work-tickets` carry;
- the claim push and its hook exception, which [NUC-main-only-via-pr](NUC-main-only-via-pr.md)
  decides;
- the isolation, number, and limits of parallel sessions, which the harness ADR decides
  ([NUC-base-is-fixed §3](NUC-base-is-fixed.md)).

## Latitude

A project may choose:

- the form of its backlog, files or the host's issues, from the
  [backlog entry](../../.agents/skills/propose-adr/concepts/backlog.md) of the catalogue;
- to plan in the issues of its host (clause 9), recorded in its harness ADR from the
  [harness entry](../../.agents/skills/propose-adr/concepts/harness.md) of the catalogue;
- how a claim is made among sessions that share neither a repository nor a remote, in the same
  harness ADR.

## Alternatives considered

- **Plan in the conversation.** The plan lives in a context window that is gone by the next
  session.
- **The host's issue tracker for every project.** It closes itself and carries assignment and
  sub-issues. An agent needs its interface and an account to read the plan. The plan is out of
  reach offline and of review with the code, and a project that moves host loses it.
- **Plan in issues forbidden.** A project that already plans in its tracker would keep two plans,
  or break the ADR. The latitude lets it keep its tracker. It gives up what the tree gives:
  offline reading, review with the code, and a frontier sensor in CI.
- **A long-lived document per feature under `docs/`.** Every later session reads it, true or not.
  Nobody deletes a document that no step of the process deletes.
- **A ticket marked done rather than deleted.** Closed work stays in front of every session that
  lists the directory. The history already keeps it.
- **A planning tool's own format, such as git-bug or Backlog.md.** One more tool in the
  environment. Plain Markdown files need none.
- **Tickets only, no spec.** Slices without the destination they serve: nothing says when the
  feature is done or which goal it meets.
- **A decision map while the way is unclear.** A map names the destination, with one decision
  ticket per question that can be stated now, resolved one per session. No project has needed one
  yet, and an artifact nobody needs is ceremony every reader pays for. It returns when one does.
- **Plan the whole effort upfront as tickets.** It slices what the first answer will change.
- **A process-owning framework or skills plugin.** It brings artifacts and a pipeline in one
  piece, in one vendor's format.
- **A change spec of its own for work on what exists.** One more artifact for the common case. The
  two questions, what changes and what stays, fit as a section of the feature spec.
- **The claim steps in this ADR, as before.** Every reader of a plan file paid for a page of push
  mechanics that only the dispatching session runs. The skill carries them; the ADR keeps the rule.
- **Findings in the conversation or an agent's memory.** Lost with the session, and read by one
  agent only.
- **The backlog as one file, or under `docs/plan/`.** One file conflicts on every merge of two
  branches that each add an entry. Under the plan, the frontier would read each entry as a ticket.

## Sources / Prior art

The shape follows Matt Pocock's skills — <https://github.com/mattpocock/skills>:

- *to-spec* — <https://www.aihero.dev/skills-to-spec>: the spec as a tracker issue, disposable
  once the work ships; here a file the work deletes, or an issue it closes.
- *to-tickets* —
  <https://github.com/mattpocock/skills/blob/main/skills/engineering/to-tickets/SKILL.md>:
  tracer-bullet vertical slices sized to one fresh context window, blocking edges, "what to
  build" from the user's side, no file paths because they go stale.
- *wayfinder* —
  <https://github.com/mattpocock/skills/blob/main/skills/engineering/wayfinder/SKILL.md>: the
  decision map under Alternatives.
- *Tracer bullets* — <https://www.aihero.dev/tracer-bullets>: why the first slice through new
  ground goes end to end.

Addy Osmani, *Agentic autonomy levels* — <https://addyosmani.com/blog/agentic-autonomy-levels/>:
the questions a ticket answers before it runs unattended, and its stopping condition. *Loop
engineering* — <https://addyosmani.com/blog/loop-engineering/>: why the stop is written on the
ticket rather than judged by the session that is stopping.

towncrier, *news fragments* — <https://towncrier.readthedocs.io/>: one file per entry, so that
parallel branches never conflict; the file form of the backlog borrows it.

## Consequences

- Positive: the slicing rule has a carrier. A session reads the plan instead of re-deriving it.
- Positive: in the tree, the plan is reviewed with the code, works offline, and moves with the
  repository. The history keeps every deleted file at no cost.
- Positive: the ticket answers whether it may run unattended and when it stops.
- Positive: what the system does today is written down before it is changed.
- Positive: a project that plans in issues keeps its tracker and still follows this ADR.
- Negative: a feature costs a spec and its tickets before the first slice. The threshold is a
  judgement under the proportionality principle ([`AGENTS.md` §1](../../AGENTS.md#1-principles)).
- Negative: an effort whose questions cannot all be asked at once has no artifact of its own.
- Negative: a plan lands on `main` like any change, so a ticket is claimable only once merged.
- Negative: a ticket file whose deletion a branch forgot stays open until review notices.
- Negative: with issues, the plan needs the host's interface and an account, and no sensor in the
  tree reads it.
- Follow-ups: the decision map, once an effort too foggy to slice has been met; triage of findings
  beyond the decision whether each becomes a ticket.

## Enforcement

- `scripts/sensor-plan-frontier.sh`, in the job `sensors`, lists the open tickets as claimed,
  blocked, or on the frontier, with their *Mode* (clauses 3 and 4). It fails on a ticket without
  a *Blocked by* or a *Mode* section. Its self-test cites this ADR. The self-test claims a
  ticket in one repository and across two clones, and the second claim must lose.
- `scripts/sensor-backlog.sh` lists the entries on `main` and on unmerged local branches, and
  fails nothing (clause 7). Its self-test cites this ADR.

**Not mechanically decidable:** the review holds whether:

- an effort needed a plan (clause 1);
- the landing branch deleted the ticket file (clause 5);
- a finding went to the backlog (clause 6);
- with a plan in issues, the issues carry the sections and the claims (clause 9).
