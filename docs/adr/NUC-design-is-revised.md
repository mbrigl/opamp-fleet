# NUC-design-is-revised: The design is revised when a feature spec closes, and after a number of changes without one

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** the closing of a feature spec, the `Last design revision` line of `docs/ARCHITECTURE.md`, `scripts/sensor-revision-due.sh`, and the `design-revision` skill

## Context

Every feature adds structure faster than anyone revisits it. A concept lands in one more file, a
piece of logic is written a second time, an interface widens by one parameter. None of it is
wrong in the change that brought it. All of it is entropy that no check sees. The record protects
decisions; nothing in the process protects the shape.

The moment to look is while the work that added the structure is still known. A feature spec
that closes is such a moment. A project that writes no specs never reaches it: one in
maintenance, with tickets and fixes only, revisits its design when somebody remembers to ask.

A count of the work done marks the second moment better than a date. A calendar chore is skipped
when the team is busy and pointless when it is idle. A count stays tied to the changes that caused
the drift.

The line that carries the count sits in the architecture overview. The revision corrects that
document anyway, and the overview is the one document that says how the system stands.

A revision that files whatever it finds fills the backlog with smells. In the derived projects of
November 2026 most entries named a shape, not a mistake the work had made, and few were taken up.
At 20 changes the revision also came due every day or two, so each one read mostly what the last
one had filed.

## Decision

We will revise the design of what recent work touched when a feature spec closes, and after a
number of changes without a spec.

1. **Closing a spec.** A feature spec closes only after a design revision, run once its last
   ticket has merged. That revision deletes the spec's `spec.md`.
2. **Scope.** A fresh session revisits the modules the work touched, the accepted ADRs that bind
   them, and the criteria of the specification they serve.
3. **Filing.** The revision files what it finds before it ends. It files a finding about the
   shape of the code only when the finding names its evidence, one of:
   - a review finding of the work in scope;
   - a commit that was red alone;
   - a rework;
   - a cost a measure of the work shows.

   It names a finding about the shape without evidence in its hand-off and files nothing for it.
   A rule to drop, a drafted change of the specification, and a proposed ADR need no such
   evidence, since the human decides each. The revision lists the backlog entries that touch its
   modules.
4. **Count.** Without a spec, a revision comes due after a number of changes.
   `docs/ARCHITECTURE.md` holds the count on this line:
   `**Last design revision:** <YYYY-MM-DD [(<n>)] or none yet>, due after <N> changes.`
   - A change is a commit that touches a path outside `docs/`. Merge commits do not count.
   - The count starts at the commit that set the date of the line.
   - A second revision on one day writes its ordinal after the date, such as `(2)`.
   - The number ships as 50. The project may set it on the line. The sensor must not hold it.
5. **Reported.** The count is reported and must not block a merge. Once it is reached, a revision
   runs over the modules those changes touched and moves the date.
6. **Stale entries.** A backlog entry without a ticket is stale once two dates of the line of
   clause 4 are newer than it. The revision proposes to the human the deletion of each stale
   entry, and the human decides. A spec's revision that leaves the line as it reads does not
   count.

**Out of scope:**

- the order of work of a revision, which the `design-revision` skill carries;
- the form of the tickets it files, which [NUC-large-work-is-planned](NUC-large-work-is-planned.md)
  decides.

## Latitude

A project may choose:

- the number of changes after which the revision is due, on the line of the overview (clause 4);
- revisions beyond the two triggers, whenever the human asks for one.

The catalogue has no entry for these choices, and none of them needs an ADR.

## Alternatives considered

- **A standing "improve the architecture" chore on a calendar.** Nothing ties it to the work that
  caused the entropy. A closing spec and a count of changes are both moments the touched modules
  are still known.
- **A failing check once the revision is due.** It blocks every merge until a session that ships
  no behaviour has run. This decision makes entropy visible, not impossible.
- **The number of changes inside the sensor.** The sensor belongs to the template and travels with
  it. How much drift a project tolerates belongs in the project's own document.
- **Counting lines changed rather than commits.** It is nearer to entropy and far noisier: a
  formatter run is thousands of lines and no structure at all.
- **A revision only when a spec closes.** A project in maintenance writes no specs and never
  revises.
- **File every finding, as before.** The backlog filled with smells that prevented nothing, and
  each revision read the last one's output. Evidence ties a filed finding to a mistake the work
  made.
- **Keep 20 changes.** Revisions came due every day or two and read their predecessor's output.
- **Delete stale entries without asking.** Only the human triages the backlog
  ([NUC-large-work-is-planned §8](NUC-large-work-is-planned.md)), and deleting an entry is triage
  too. The revision only proposes.

## Sources / Prior art

- Matt Pocock, *improve-codebase-architecture* —
  <https://github.com/mattpocock/skills/blob/main/skills/engineering/improve-codebase-architecture/SKILL.md>:
  the design revision at the close of a spec.
- Kent Beck, *Extreme Programming Explained*, quoted in the README of
  <https://github.com/mattpocock/skills>: "Invest in the design of the system every day."

## Consequences

- Positive: the design of what a feature touched is revisited while it is still known.
- Positive: the overview is checked against the tree at the same moment.
- Positive: a project that writes no specs revisits its design at a rhythm its own changes set.
- Positive: the tree answers when the design was last looked at.
- Positive: a filed finding about the shape names the mistake or the cost behind it, so the
  backlog holds work that pays.
- Negative: a feature spec stays open one session longer.
- Negative: the revision costs a session that ships no behaviour.
- Negative: the revision files tickets, and nothing obliges anyone to work them.
- Negative: at 50 changes a revision reads more changes at once, and drift waits longer.
- Negative: a real flaw that has not yet caused a mistake waits in a hand-off until it does.
- Negative: a commit is a coarse measure of drift. A project that squashes reaches the number more
  slowly.
- Negative: the date moves by hand, in the revision's own change.

## Enforcement

`scripts/sensor-revision-due.sh`, in the job `sensors`, reads the line of the overview (clause 4).
It reports the count since the line's date. Once the revision is due, it lists the top-level
paths the changes touched. It fails only on a missing or unreadable line (clause 4), never on the
count (clause 5). Its self-test cites this ADR.

**Not mechanically decidable:** the review holds whether:

- a spec closed without its revision (clause 1);
- a filed finding about the shape names its evidence (clause 3);
- a due revision ran and moved the date (clause 5);
- a revision proposed each stale backlog entry (clause 6).
