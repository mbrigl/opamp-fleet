# Backlog

Where a finding goes that no ticket covers: a defect, a doubt, or an idea a session meets
outside its scope. The plan holds the work that is decided; the backlog holds what is noticed and
not yet decided. Without one, a finding stays in the conversation or an agent's memory and is lost
with them, or it widens the ticket that found it. The template ships the file form; ADR-0006 makes
it the default while no ADR of the project chooses.

## Signals

- A hand-off names a finding outside its ticket, and nothing records it.
- Two sessions report the same defect.
- Reports arrive from people outside the project, through the host's issue tracker.
- A ticket grows during its work by something it found.

## Fits when

- **Files**, one per entry under `docs/backlog/`: the project plans in the tree, works offline or
  on more than one host, and its findings come from its own sessions. Each entry is a new file, so
  two branches that each record one never conflict. An entry holds a title, what was observed,
  and where, and is created in a commit of its own, so a branch that is dropped can hand it on by
  cherry-pick.
- **Issues** of the host: reports arrive from outside, from users or other teams, and the project
  answers them there. Recording an agent's finding as an issue is a publication, so the human
  approves each.

## Does not fit

- Both forms at once: two places to look, and each finding is recorded twice or in the wrong one.
- One file for the whole backlog: every two branches that add an entry conflict on merge.
- Entries under `docs/plan/`: the frontier sensor reads every file there as a ticket.
- Issues for a project whose host is not decided, or which must work offline.

## The rule a test decides

None decides whether a finding was recorded; the review does. What a test decides is the
mechanics: with files, the backlog sensor lists the entries on `main` and those only on unmerged
local branches, and an entry is no ticket. With issues, the project's ADR names the label or
the project board the entries carry, and no file test applies.

## Sensors

`scripts/sensor-backlog.sh`, which ships with the template: it lists the entries on `main` and
those that local branches add and `main` does not have yet, with the branch, and fails nothing.
The design revision lists the entries that touch the modules it revisits. With issues, the host's
own list is the sensor.

## Alternatives

The two forms above are each other's alternative. A planning tool's own format, such as git-bug
or Backlog.md, is a third: one more tool in the environment, as ADR-0006 says for the plan.

## Sources

- towncrier, *news fragments* — <https://towncrier.readthedocs.io/>: one file per change, so that
  parallel branches never conflict on one changelog; the file form of the backlog borrows it.
- GitHub Docs, *About issues* —
  <https://docs.github.com/en/issues/tracking-your-work-with-issues/about-issues>.
