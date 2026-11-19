---
name: review
description: Review a change in a fresh context before its hand-off. Use when a change is ready to hand off, when the human asks for a review, or when you are the session or subagent given a diff to judge — never on your own work.
---

# Review a change

The rule is [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done). This skill is
what the reviewer is given and the order in which it reports; it states no rule of its own
([NUC-procedures-are-skills §3](../../../docs/adr/NUC-procedures-are-skills.md)).

## 1. Inputs — and nothing else

The reviewer runs in a context that did not write the change and is handed only:

- the diff, its commits in the shape they are to merge in;
- the task: the ticket or the request, with its acceptance criteria;
- [`AGENTS.md` §1](../../../AGENTS.md#1-principles) and
  [§5](../../../AGENTS.md#5-quality-bar--definition-of-done);
- the *Doubts* section of the hand-off draft under `.scratch/`, the pull request description that
  fills the sections of the pull request template
  ([NUC-main-only-via-pr §5](../../../docs/adr/NUC-main-only-via-pr.md)): what the implementer
  disagreed with, what it was unsure of, what it could not confirm, and what a derived project
  acts on;
- [`docs/CONVENTIONS.md`](../../../docs/CONVENTIONS.md);
- the golden path named in [`docs/ARCHITECTURE.md`](../../../docs/ARCHITECTURE.md);
- the ADRs in force whose *Applies to* the diff touches: those that name a path of the diff, as
  [`scripts/sensor-binding-adrs.sh`](../../../scripts/sensor-binding-adrs.sh) `--sections`
  lists them with the parts a session reads, and those whose *Applies to* is prose, which it
  prints from the ADR files, the `NUC-` ones included;
- for each of those ADRs that chose a concept of the catalogue in
  [`propose-adr`](../propose-adr/concepts/README.md), that entry's *Does not fit* and *The rule a
  test decides*: what the tests decide is checked there, and the rest is checked here;
- the output of the sensors the toolchain ADR names, and of
  [`scripts/check-all.sh`](../../../scripts/check-all.sh) `--verbose`, whose tests-kept sensor
  lists the test files deleted and the skip markers added since the base;
- for a normalization, its map and its clause table, and the record at the base commit
  ([NUC-decisions-are-proposed-then-accepted §8](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)).

A light review ([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)) is
handed the diff, the task, the doubts, and that output, and nothing else. It judges whether every
path of the diff is one that section names, and whether the diff does what the task says, and
reads nothing beyond it; a path outside them is its one finding, and the change goes to a full
review.

The reviewer is the harness's inferential sensor: it judges what the computational ones, the
tests and checks, cannot decide.

If the implementer's reasoning, plan, or conversation is offered, decline it. The review holds
the diff against the standards, not against the story of how it came to be.

Beyond these inputs the reviewer opens a file only where a finding needs it: a file the diff
touches, or one that calls or reads what it changed. It reads the rest of an ADR only where the
diff questions its *Decision*, and it runs no check whose output it was handed. A second review
that sees the fixes is handed the commits after the one the first review saw and the findings
they answer, and judges those; where the agent can continue a session, it continues the first
review's, which has read the change already and wrote none of the fixes. A later change gets a
review session of its own. After a rework, the review is of the whole branch at its head
([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)).

## 2. Report on two axes, kept apart

**Does it do what was asked?** Each acceptance criterion against the diff and its tests: met,
not met, or met by a test that does not decide it. Scope the task did not ask for is a finding.

**Does it meet the bar?** Simplicity first ([`AGENTS.md` §1](../../../AGENTS.md#1-principles)):
is there a smaller change that meets the same criteria; what did the diff add that nothing asked
for, such as an abstraction, a dependency, a parameter, a flag, a fallback, or a comment restating
the code; what could be deleted. Then each bullet of
[`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done) that applies, in its order;
of the tests, also whether they take their expected values from the criterion rather than from
the code under test. Then the conventions, the ADRs, and the sensor output; a sensor finding the
implementer suppressed is a finding here. Of each statement the change adds to the rules, whether
it is one sentence ([`AGENTS.md` §4](../../../AGENTS.md#4-working-style)). Last, the
*Doubts*, in their four fields: each point under *Disagreed* checked against its evidence, each
under *Unsure* settled or left to the human by name, each under *Could not confirm* confirmed or
left open by name, and *A derived project acts on* checked against the diff: a changed interface
of a script, a check that newly reaches a project's own files, a moved key, or a removed file that
it does not name is a finding. A "none" is checked like any other claim.

## 3. Tests that changed

List every existing test the diff changes or removes. Each needs a reason the behaviour changed,
verified against the diff, not against the hand-off's claim. One without a reason weakened
the suite and is the first finding.

## 4. Hand back

Findings, concrete and ranked, with file and line, and what the reviewer itself could not verify:
each finding that must be fixed before the hand-off with its scenario, a lesser one a line, and no
account of what is in order. No fixes applied. The implementer fixes the first kind before the
hand-off, and a lesser one it fixes or names in the hand-off's *Doubts*. On a `feat/` branch and
on a branch that integrates on one, a fix that corrects one commit is a fixup of it
(`git commit --fixup=<commit>`, its body saying what the finding was). The rework folds it in by
`git rebase -i --autosquash`, at the latest when the `feat/` branch is rebuilt by topic
([Integration branches](../../../CONTRIBUTING.md#integration-branches), steps 4 and 7), instead
of by hand. A fix that spans commits is a commit of its own, and so is every fix on any other
branch, since `main` keeps what it merges. The hand-off records in its *Review* section who
reviewed, the findings on both axes, what was fixed, and the commit the reviewer was handed, on its
`Reviewed at:` line. The fixes come after that commit;
[`scripts/sensor-review-binding.sh`](../../../scripts/sensor-review-binding.sh) lists them for the
human who decides the merge, unless a second review sees them first and the line moves. After a
rework, the pull request's description carries that line.
