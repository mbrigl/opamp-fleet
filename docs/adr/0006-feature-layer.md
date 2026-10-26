# ADR-0006: Work larger than one session is planned in the tree as a feature spec cut into tracer-bullet tickets, files deleted when their work lands, and the design is revised when a spec closes or after a number of changes

- **Status:** 🟢 accepted
- **Date:** 2026-09-17
- **Deciders:** NUC maintainer
- **Applies to:** every change larger than one agent session, `docs/plan/`, the spec and ticket templates of the `plan-feature` skill, the pull request template, the `Last design revision` line of `docs/ARCHITECTURE.md`, `scripts/sensor-revision-due.sh`, `scripts/sensor-plan-frontier.sh`, and the claim script `.agents/skills/work-tickets/claim.sh`

## Context

Authority runs specification → accepted ADRs → task ([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules)),
and nothing in that chain says what *this month's work* is. A change that fits one session is
planned in the conversation. A change that does not has no place to keep its state: each session
starts without memory, re-derives the plan, and drifts. The rule to cut work into vertical slices
([`AGENTS.md` §4](../../AGENTS.md#4-working-style)) has no artifact that carries a slice, and the
condition for running a task unattended has nothing to be written in.

Two more forces come with age. Most work in a system that already exists is change, not addition,
and a plan written from the user's side alone says nothing about what the system does today, what
must stay as it is, and how what exists gets from the old behaviour to the new — the part a session
without memory gets wrong first. And every feature adds structure faster than anyone revisits it:
a concept lands in one more file, a piece of logic is written a second time, an interface widens
by one parameter — none of it wrong in the change that brought it, all of it entropy no
check sees. A record protects decisions (ADR-0002); nothing in the process protects the shape.
The moment to look is while the work that added the structure is still known, and a project that
is not writing features never reaches it: one in maintenance, with tickets and fixes only,
revisits its design when somebody remembers to ask.

The artifact cannot live in the specification, which must not churn (ADR-0001); cannot be a
seventh rule-bearing document; and must be disposable by design, because a plan that outlives its
usefulness is noise for every later session. It must also be readable by any agent with the tools
it already has, without one vendor's tracker or an account on it: the template stays
vendor-neutral.

## Decision

We will plan every change larger than one agent session in the tree, under `docs/plan/` — as a
**feature spec** broken into **tracer-bullet tickets** that each declare which tickets block them
— and treat both as **disposable**: a ticket's file is deleted by the branch that lands its
work, a spec's by the design revision that closes it, and whatever in them has to last moves into
the specification, an ADR, the architecture overview, the glossary, or the conventions. The
history keeps the deleted files.

Each spec is a directory `docs/plan/<spec>/` holding `spec.md` and one file per ticket,
`<NN>-<slug>.md`, numbered in the order they are cut. Tickets filed outside a spec — by a design
revision, or for unfinished work a session leaves — get a directory of their own without a
`spec.md`. A directory that has lost its last
file is gone, so what lies under `docs/plan/` is exactly the open work. The shapes are templates
of the `plan-feature` skill (ADR-0003).

A **feature spec** states the problem and the solution from the user's side, the existing
behaviour it changes and what must stay as it is, the goals of the specification it serves by
identifier (`G-n`, `Q-n`), the user stories, the implementation decisions and the seams at which
the change is tested — without file paths or code, which go stale — what is out of scope, and
whether it needs an ADR. A **ticket** states the end-to-end behaviour it delivers, its acceptance
criteria, the tickets that block it, what the session that works it reads first, and whether it is
worked with the human or may run unattended — and, if so, when the session stops
([`AGENTS.md` §4](../../AGENTS.md#4-working-style)). A ticket is sized to one fresh session and is
demoable or verifiable on its own. It names its blockers by path under `docs/plan/`, as text
rather than links, and a blocker is closed once its file is gone. The frontier — open tickets
whose blockers are all closed and that no branch claims — is what an agent picks from, and a
sensor lists it with each ticket's *Mode*. The agent claims a ticket by creating the branch
`ticket/<spec>/<NN>-<slug>` before any work, so that parallel sessions skip it, through the
claim script of the `work-tickets` skill. Among sessions that share a repository the claim is
the local branch, which git refuses to create twice, and needs no commit and nothing pushed.
Among clones that share a remote, the claim is made there first: a push that only creates the
branch, with an empty commit of its own on the remote's `main`, because a push of a commit the
branch already points at succeeds without creating anything, and two clones would both win.
That push skips the pre-push hook: its commit carries the tree of the remote's `main`
unchanged, so the checks have nothing to decide.
That push is the one the harness ADR may grant without an instruction per push
([`AGENTS.md` §6](../../AGENTS.md#6-project-rules)); a project whose sessions share a
repository grants none. A ticket is claimable once it is on `main`, where the worker's branch
starts and other clones read it. A claim given up is released by deleting its branch: a local
one by the session that made it, one on a remote by a human, since the push the harness ADR
grants only creates. Sessions that run in parallel each work in a worktree or a container of
their own, as the harness ADR says, which also says how a claim is made among sessions that
share neither a repository nor a remote. The branch that lands a ticket's work deletes its file. A
feature spec closes when its tickets have merged and a fresh session has revisited the design
of what they touched — the modules, the accepted ADRs that bind them, and the specification
criteria they serve — and filed what it found
([`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done)).

The revision also comes due **without a spec**, on a count of the work done rather than on a
date. [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md) carries on one line the date of the last
revision and the number of changes after which the next is due — `**Last design revision:**
<YYYY-MM-DD or none yet>, due after <N> changes.` — and a sensor counts the changes since
the commit that set that date, so the history decides where the count starts, not the clock. A
**change** is a commit that touches anything outside `docs/`, merges excluded. The number ships
as 20 and is the project's to set, on the line rather than in the sensor. The count is
reported, never enforced: once the number is reached the revision runs over the modules those
changes touched, and it moves the date. The line sits in the overview because the revision
corrects that document anyway, and because the overview is the one document that says how the
system stands (ADR-0001).

**Out of scope:** how work smaller than one session is planned; an artifact for an effort too
foggy to be written as a spec; how reports that arrive from outside — bug reports, questions — are
received and triaged; whether a due revision blocks a merge, which it does not.

## Alternatives considered

- **Plan in the conversation.** The state this ADR ends: the plan lives in a context window that
  is gone by the next session.
- **The repository's issue tracker.** Closes itself and carries assignment and sub-issues, at the
  price of one vendor's tracker: an agent needs its API or CLI and an account to read the plan,
  the plan is out of reach offline and of review with the code, and a project that moves
  host loses it. Claiming by assignment needs no shared repository, which is the one thing a file
  in the tree cannot give.
- **A long-lived document per feature under `docs/`.** Read by every later session whether still
  true or not; nobody deletes a document that no step of the process deletes. Here the deletion
  is a step of landing the work.
- **A ticket marked done rather than deleted.** Keeps closed work in front of every session that
  lists the directory; the history already keeps it.
- **A planning tool's own format, such as git-bug or Backlog.md.** One more tool in the
  environment and its format for the plan; the same files in plain Markdown need neither.
- **Tickets only, no spec.** Slices without the destination they serve: nothing says when the
  feature is done or which goal it meets.
- **A decision map while the way is unclear.** A map document naming the destination, with one child
  decision ticket per question that can be stated now — typed research, prototype, conversation,
  or task — resolved one per session, the spec cut once no question remains. The right shape for
  an effort whose questions cannot all be asked at once, and not in the template because no
  project has had one yet: an artifact nobody has needed is ceremony every reader pays for
  ([`AGENTS.md` §1](../../AGENTS.md#1-principles)). It returns when one does.
- **Plan the whole effort upfront as tickets.** Slices what the first answer will change.
- **A process-owning framework or skills plugin.** Brings artifacts and pipeline in one piece, at
  the price of one vendor's format; the template stays vendor-neutral (ADR-0001).
- **A change spec of its own for work on what exists.** One more artifact for the common case;
  the same two questions — what changes, what stays — fit as a section of the feature spec, and
  "greenfield" is an answer to them.
- **A standing "improve the architecture" chore on a calendar.** Nothing ties it to the work that
  caused the entropy, so it is skipped when busy and pointless when idle; a spec that closes and
  a count of changes are both moments the touched modules are still known.
- **A failing check once the revision is due.** Blocks every merge until a session that
  ships no behaviour has run; this layer makes entropy visible, not impossible.
- **The number of changes inside the sensor.** The sensor belongs to the template and travels
  with it; how much drift a project tolerates belongs in the project's own document.
- **Counting lines changed rather than commits.** Nearer to entropy and far noisier: a formatter
  run is thousands of lines and no structure at all.

## Sources / Prior art

The shape of this layer follows Matt Pocock's skills — <https://github.com/mattpocock/skills> —
closely, and a reader who knows them should recognise it:

- *to-spec* — <https://www.aihero.dev/skills-to-spec>: the spec as a tracker issue, disposable
  once the work ships; here a file the work deletes.
- *to-tickets* —
  <https://github.com/mattpocock/skills/blob/main/skills/engineering/to-tickets/SKILL.md>:
  tracer-bullet vertical slices sized to one fresh context window, blocking edges on the tracker,
  "what to build" from the user's side, no file paths or snippets because they go stale; the
  expand–contract exception for wide refactors.
- *wayfinder* —
  <https://github.com/mattpocock/skills/blob/main/skills/engineering/wayfinder/SKILL.md>: a map
  issue with child decision tickets typed research, prototype, grilling, or task, resolved one per
  session; the decision map under Alternatives.
- *Tracer bullets* — <https://www.aihero.dev/tracer-bullets>: why the first slice through new
  ground goes end to end.
- *improve-codebase-architecture* —
  <https://github.com/mattpocock/skills/blob/main/skills/engineering/improve-codebase-architecture/SKILL.md>
  — and the repository's README, which quotes Kent Beck, *Extreme Programming Explained*:
  "Invest in the design of the system every day"; the design revision at the close of a spec.

The conditions for unattended work come from Addy Osmani, *Agentic autonomy levels* —
<https://addyosmani.com/blog/agentic-autonomy-levels/>: how quickly will we know we are wrong,
how cleanly can we undo, what would prove us right — the three questions a ticket answers — and
the contract every run gets, whose "stopping condition: when to stop; ideally, a measurable
variable" and budget are the fourth. *Loop engineering* on the same site —
<https://addyosmani.com/blog/loop-engineering/>: a loop that runs "until a condition you wrote is
actually true", with a fresh model deciding whether it is done; why the stop is written on the
ticket rather than judged by the session that is stopping.

## Consequences

- Positive: the slicing rule has a carrier; a session reads `docs/plan/` instead of re-deriving
  the plan, with the file tools every agent has; the plan is reviewed like
  the code, works offline, and moves with the repository; "may this run unattended?" and "when
  does it stop?" are answered by the ticket, so a session that is not converging hands off
  instead of widening its scope; the history is an audit trail of closed work that costs nothing
  to keep; what the system does today is written down before it is changed;
  the design of what a feature touched is revisited while it is still known, and the overview is
  checked against the tree at the same moment; a project that writes no specs revisits its design
  at a rhythm its own changes set, and "when did we last look?" is answered in the tree.
- Negative / trade-offs: a feature now costs a spec and its tickets before the first slice —
  ceremony a one-session change must not pay, so the threshold is a judgement under the
  proportionality principle ([`AGENTS.md` §1](../../AGENTS.md#1-principles)). An effort whose
  questions cannot all be asked at once has no artifact of its own and is charted in the
  conversation, which is gone by the next session. Blocking is a list of paths, and the
  sensor that resolves it reports the frontier and refuses only a ticket it cannot place. A plan
  lands on `main` like any change, so it is claimable only once merged. A claim
  across clones costs a push the harness ADR has to grant, an empty commit on the branch, and a
  fetch before every read of the frontier, and holds only where the sessions share a remote. A
  ticket file whose deletion a branch forgot stays open until review notices. A feature
  spec stays open one session longer, and the revision costs a session that ships no behaviour;
  what it files is tickets, so nothing obliges anyone to work them — the decision makes entropy
  visible, not impossible. A commit is a coarse measure of drift, so a project that squashes
  reaches the number more slowly than one that does not, and the date is moved by hand, by the
  revision's own change.
- Follow-ups: the decision map, once an effort too foggy to slice has been met; triage of reports
  arriving from outside.

## Enforcement

[`scripts/sensor-revision-due.sh`](../../scripts/sensor-revision-due.sh) (job `sensors` in
[`checks.yml`](../../.github/workflows/checks.yml)) reads the `Last design revision` line of the
overview, counts the changes since the commit that set its date, reports the count, and lists
the top-level directories and files they touched, with the changes each, once the number is
reached; it fails when the line is missing or unreadable, because then it measures nothing. Its
self-test cites this ADR ([ADR-0002](0002-decisions-verified-by-tests.md)).
[`scripts/sensor-plan-frontier.sh`](../../scripts/sensor-plan-frontier.sh), in the same job, lists
the open tickets as claimed, blocked, or on the frontier with their *Mode*, and fails on a ticket
without a *Blocked by* or a *Mode* section, which it cannot place; its self-test cites this ADR
and runs the claim script of the `work-tickets` skill, in one repository and across two clones of
one remote, where the second claim of a ticket must lose.

**Not mechanically decidable:** whether an effort was larger than one session and so had to be
specced and sliced, whether the branch that landed a ticket's work deleted its file, and
whether a spec was deleted without its design revision are review, as is whether a due revision
was run at all and the date moved with it. The templates of the
[`plan-feature`](../../.agents/skills/plan-feature/) skill carry the shape, and the merge message
names the ticket a change deletes ([`AGENTS.md` §6](../../AGENTS.md#6-project-rules)).
