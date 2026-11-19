---
name: design-revision
description: Revisit the design of what recent work touched — its modules, the architecture overview, and the ADRs and criteria that bind them. Use when a feature spec's tickets have all merged, when scripts/sensor-revision-due.sh reports the revision DUE, or when the human asks for a retro or an architecture review.
---

# Revise the design

The rule is [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done); that a spec
closes only after this has run, and that it is due after a number of changes without a spec, is
decided in [NUC-design-is-revised](../../../docs/adr/NUC-design-is-revised.md). This skill runs
in a fresh session and states no rule of its own
([NUC-procedures-are-skills §3](../../../docs/adr/NUC-procedures-are-skills.md)).

A project that plans in its host's issues, as its harness ADR records
([NUC-large-work-is-planned §9](../../../docs/adr/NUC-large-work-is-planned.md)), takes the
spec's closed ticket issues as its tickets in step 1, and files what it finds as issues in
step 7. Where step 7 deletes `spec.md`, the revision's pull request names the spec's issue with a
closing keyword (`Closes #<n>`), and the human's merge closes it; the session never closes it
itself. The rest of this skill holds as written.

## 1. Scope

Take the feature spec and its merged tickets: once only its `spec.md` is left, the tickets are the
files the history deleted from its directory (`git log --diff-filter=D -- docs/plan/<spec>/`).
The modules those tickets' merges touched are the scope; the rest of the tree is context, not a
target. One session, one spec.

Without a spec, when [`scripts/sensor-revision-due.sh`](../../../scripts/sensor-revision-due.sh)
says the revision is due, the scope is the directories it lists: the changes since the last
revision. One session, one count.

A spec's revision runs the sensor as it begins. Where it says the revision is due, that revision
takes the directories it lists too: moving the line resets the count, and changes no spec covered
would otherwise go unrevised. Where it says not yet due, the revision leaves the line as it reads,
and the count runs on over the changes it did not look at.

## 2. Survey the modules

For each module touched, look for what a single change cannot see and a check does not:

- a concept spread over several files that belongs in one;
- logic written a second time;
- an interface that widened, one more parameter or one more flag, where a second function or a
  narrower one was due;
- a module with little behaviour behind its surface;
- a dependency that points the wrong way against the structural rule an ADR decided;
- a shape the code has grown into that no ADR decides, or one that the *Does not fit* of its
  entry in the catalogue of `propose-adr` describes.

One candidate at a time, each with the ADR or convention it offends, or the name of the smell
where none does, and with its evidence: a review finding, a commit that was red alone, a rework,
or a cost of step 6. A candidate without evidence goes into the hand-off, not into a ticket
([NUC-design-is-revised §3](../../../docs/adr/NUC-design-is-revised.md)).

## 3. Check the overview against the tree

Read [`docs/ARCHITECTURE.md`](../../../docs/ARCHITECTURE.md) with the tree open. A part it names
that no longer exists, a part that exists and it does not name, a flow that no longer runs as
described: each is a finding, because a stale overview is believed.

## 4. Read the record and the constitution against the system

Take the ADRs in force whose *Applies to* names a touched module:
[`scripts/sensor-binding-adrs.sh`](../../../scripts/sensor-binding-adrs.sh) `--sections`, with a
`--path` for each, prints their header, *Decision*, and *Enforcement*, and the *Applies to* written
in prose to match. Read in full each whose *Decision* the survey of step 2 or the system
questions, and each whose *Context* names what the work touched, because the revision weighs its
forces against the system; one the system still bears out, its forces untouched, stands on those
three parts. Decisions that have grown into one, each ADR true on its own
and the set said in one sentence, are a candidate for one superseding ADR that consolidates
them ([mechanics](../../../docs/adr/README.md#mechanics)).

Then the criteria of [`docs/SPECIFICATION.md`](../../../docs/SPECIFICATION.md) the feature spec
named as served; without a spec, the criteria a `Verifies:` marker cites from the directories
in scope. One whose text no longer says what the tests decide, one the work moved past: each is
a candidate for a specification change. The specification is the human's
([NUC-documents-are-fixed §2](../../../docs/adr/NUC-documents-are-fixed.md)): draft the change and
the accepted ADRs it would move; never land it.

## 5. Go through the mistakes

From the review findings and the history of the merges in scope, those of the spec's
tickets or those of the counted changes, list what went wrong on the way. For each: could a
check have caught it? Then it becomes a check. Could a convention have prevented it?
Then it is an entry in [`docs/CONVENTIONS.md`](../../../docs/CONVENTIONS.md).

Then the reverse: a convention or a check that prevented nothing during the work in scope is
dropped, unless an ADR or a criterion cites it
([NUC-decisions-cite-tests §2](../../../docs/adr/NUC-decisions-cite-tests.md)).

Then the rules added since the last revision: every rule written into
[`AGENTS.md`](../../../AGENTS.md), the mechanics of [`CONTRIBUTING.md`](../../../CONTRIBUTING.md),
or the [record's mechanics](../../../docs/adr/README.md#mechanics), and every step written into a
skill, read from the diff of those files since the commit that set the date of the `Last design
revision` line, where [`scripts/sensor-revision-due.sh`](../../../scripts/sensor-revision-due.sh)
starts its count, or from all history while the line reads `none yet`. A rule in prose is the
fallback for what no check can decide
([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)), so each names the
mistake it prevents, or it is a candidate to drop. Each that stays is set against the older ones
beside it, and an older one it makes needless is a candidate too. One an ADR decides is dropped
only as that ADR may change
([NUC-decisions-are-proposed-then-accepted §6](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)).
The hand-off of the revision lists what was added and what it proposes to drop, with the lines each
file gained and lost (`git diff --stat`), so the growth of the rule set is seen once per revision.

## 6. What the work cost and what slowed it

Read what the merges in scope cost. Four sources hold it:

- the `Measures:` line of each merge's hand-off, in the form of
  [`.github/PULL_REQUEST_TEMPLATE.md`](../../../.github/PULL_REQUEST_TEMPLATE.md): the tokens,
  the seconds of the checks, and the rounds of review;
- [`scripts/sensor-work-measures.sh`](../../../scripts/sensor-work-measures.sh): the hours from
  each branch's first commit to its merge, and the rounds of review. It reads GitHub, so it runs
  on the human's instruction ([NUC-main-only-via-pr §2](../../../docs/adr/NUC-main-only-via-pr.md));
- [`scripts/sensor-red-commits.sh`](../../../scripts/sensor-red-commits.sh): the commits that were
  red alone. It runs the checks once per commit, so it runs when the human asks;
- the times [`scripts/check-all.sh`](../../../scripts/check-all.sh) prints: each check's, the
  slowest, and the whole run's.

Look for what cost much and gave little: a merge with several rounds of review, a branch open
long, a slow check that found nothing in scope, tokens that stand out, a commit red alone. Name
the cause of each. A cause in the project's own code, checks, or conventions is a candidate for
a ticket of step 7. A cause in what the template ships is a finding about the template. What
the template ships is a section of [`AGENTS.md`](../../../AGENTS.md), a `NUC-` ADR, a script, a
skill, or another file the template ships and the project carries unchanged. Keep the findings
about the template apart from the project's tickets
([NUC-template-learns-from-projects §1](../../../docs/adr/NUC-template-learns-from-projects.md)).
A backlog entry about the template, recorded by another session, is a source of a finding too.
In the template's own repository, such a cause is a candidate for a ticket of step 7, like any
other.

In a derived project, write each finding about the template as a draft under
`.scratch/template-findings/`, one file each
([NUC-template-learns-from-projects §3](../../../docs/adr/NUC-template-learns-from-projects.md)).
The draft has the fields of the issue template `template-finding.md` of `hivevm/nuc`, which the
setup deletes from a project
([NUC-template-learns-from-projects §2](../../../docs/adr/NUC-template-learns-from-projects.md)):

```markdown
## Template release

## Concerns

## Finding

## Measures

## Proposal
```

- *Template release* is the release the `Template release` line of the README names.
- *Concerns* names the rule, file, or skill of the template.
- *Finding* says what the work met.
- *Measures* names the measures that show it, or reads "none".
- *Proposal* says what the template changes.

A draft carries no code, names, paths, or data of the project
([NUC-template-learns-from-projects §2](../../../docs/adr/NUC-template-learns-from-projects.md)).
List the drafts in the results of the revision, so that the human sees them before the session
ends. The human decides whether to
send each, as an issue of `hivevm/nuc`; the session opens none
([NUC-template-learns-from-projects §4](../../../docs/adr/NUC-template-learns-from-projects.md)).
Mark the backlog entry a draft came from as drafted, with a line `Drafted: <YYYY-MM-DD>` or a
comment on its issue, and leave it
([NUC-template-learns-from-projects §1](../../../docs/adr/NUC-template-learns-from-projects.md)).
Draft no finding from an entry marked so. A revision that found nothing about the template drafts
none.

In the template's own repository, the open issues of that form are a second input
([NUC-template-learns-from-projects §5](../../../docs/adr/NUC-template-learns-from-projects.md)).
Their titles start with `[Finding]`. The human hands them over, or instructs the session to read
them with `gh`. List each as a candidate for a ticket in the results. The human decides which
becomes a ticket, and closes each issue listed, with a link to its ticket or the reason it was
declined.

## 7. File, do not fix

The conventions and the overview are corrected inline, as their documents ask. Everything else,
except the drafts of step 6, which stay under `.scratch/`, is filed before the revision ends
rather than implemented here: a ticket per candidate the survey found with its evidence, a ticket
per rule to drop, a ticket per check to write, a `proposed` ADR through `propose-adr` where a
finding is a decision, a consolidating one included, and a drafted specification change as a
ticket whose *Mode* is "with the human". The tickets go into
`docs/plan/revision-<YYYY-MM-DD>/`, in the shape of the `plan-feature` skill's ticket template,
without a spec; a second revision on one day numbers on in the same directory. List the backlog
entries that touch the modules in scope, from
[`scripts/sensor-backlog.sh`](../../../scripts/sensor-backlog.sh) or the host's issues, as
candidates for the human
([NUC-design-is-revised §3](../../../docs/adr/NUC-design-is-revised.md)); a ticket made from one
deletes the entry in the same commit. The hand-off proposes to the human the deletion of each
stale entry ([NUC-design-is-revised §6](../../../docs/adr/NUC-design-is-revised.md)): one
added (`git log --diff-filter=A --format=%as -- docs/backlog/<entry>`, or the date the host
shows for an issue) before two dates of the `Last design revision` line
(`git log -L '/Last design revision/,+1:docs/ARCHITECTURE.md'`), with no ticket made from it.
A date with an ordinal counts on its own; while the line has read fewer than two dates, no entry
is stale. Where the
revision took the count, move the `Last design revision` line of
[`docs/ARCHITECTURE.md`](../../../docs/ARCHITECTURE.md) to today's date; where it reads today's
date already, add this revision's ordinal of the day after it, `(2)` for the second. Then delete
the `spec.md` of a spec in scope: it is closed.
