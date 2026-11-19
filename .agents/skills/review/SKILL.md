---
name: review
description: Review a change in a fresh context before its hand-off. Use when a change is ready to hand off, when the human asks for a review, or when you are the session or subagent given a diff to judge — never on your own work.
---

# Review a change

The rule is [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done). This skill is
what the reviewer is given, the order in which it reports, and what the implementer does with
the findings. It states no rule of its own
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
handed the diff, the task, the doubts, and that output, and nothing else. It judges two things
and reads nothing beyond them:

- whether every path of the diff is one that section names;
- whether the diff does what the task says.

A path outside that section is its one finding, and the change goes to a full review.

The reviewer is the harness's inferential sensor: it judges what the computational ones, the
tests and checks, cannot decide.

If the implementer's reasoning, plan, or conversation is offered, decline it. The review holds
the diff against the standards, not against the story of how it came to be.

Beyond these inputs the reviewer opens a file only where a finding needs it: a file the diff
touches, or one that calls or reads what it changed. It reads the rest of an ADR only where the
diff questions its *Decision*. It runs no check whose output it was handed.

A second review that sees the fixes is handed two things: the commits after the one the first
review saw, and the findings they answer. It judges those. Where the agent can continue a
session, it continues the first review's, which has read the change already and wrote none of
the fixes. A later change gets a review session of its own. After a rework, the review is of the
whole branch at its head ([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)).

## 2. Report on two axes, kept apart

**Does it do what was asked?** Each acceptance criterion against the diff and its tests: met,
not met, or met by a test that does not decide it. Scope the task did not ask for is a finding.

**Does it meet the bar?** In this order:

1. Simplicity first ([`AGENTS.md` §1](../../../AGENTS.md#1-principles)):
   - Is there a smaller change that meets the same criteria?
   - What did the diff add that nothing asked for, such as an abstraction, a dependency, a
     parameter, a flag, a fallback, or a comment restating the code?
   - What could be deleted?
2. Each bullet of [`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done) that
   applies, in its order. Of the tests, also whether they take their expected values from the
   criterion rather than from the code under test.
3. The conventions, the ADRs, and the sensor output. A sensor finding the implementer suppressed
   is a finding here.
4. Of each statement the change adds to the rules, whether it is one sentence
   ([`AGENTS.md` §4](../../../AGENTS.md#4-working-style)).
5. Last, the *Doubts*, in their four fields:
   - each point under *Disagreed*, checked against its evidence;
   - each point under *Unsure*, settled or left to the human by name;
   - each point under *Could not confirm*, confirmed or left open by name;
   - *A derived project acts on*, checked against the diff. A changed interface of a script, a
     check that newly reaches a project's own files, a moved key, or a removed file that it does
     not name is a finding.

   A "none" is checked like any other claim.

## 3. Tests that changed

List every existing test the diff changes or removes. Each needs a reason the behaviour changed,
verified against the diff, not against the hand-off's claim. One without a reason weakened
the suite and is the first finding.

## 4. Hand back

The reviewer hands back findings, concrete and ranked, with file and line:

- each finding that must be fixed before the hand-off, with its scenario;
- each lesser finding, in a line;
- what the reviewer itself could not verify.

It gives no account of what is in order, and it applies no fixes. For example:

```markdown
**Does it do what was asked?**
1. Must fix — `scripts/foo.sh:42`: criterion 2 is met by a test that does not decide it; the
   test passes with the new branch removed. Scenario: an empty input still exits 0.

**Does it meet the bar?**
2. Must fix — `scripts/foo.sh:57`: the `--force` flag nothing asked for; delete it.
   Scenario: a caller passes `--force` and skips the check.
3. Lesser — `scripts/test-foo.sh:12`: the expected value is copied from the output, not the
   criterion.

**Could not verify:** the timing claim under *Unsure*; no measure was handed over.
```

## 5. After the review

This step is the implementer's. The implementer fixes each finding that must be fixed before the
hand-off. It fixes a lesser one or names it in the hand-off's *Doubts*.

How a fix is committed depends on the branch:

- On a `feat/` branch and on a branch that integrates on one, a fix that corrects one commit is
  a fixup of it: `git commit --fixup=<commit>`, its body saying what the finding was. The rework
  folds it in by `git rebase -i --autosquash`, instead of by hand. It does so at the latest when
  the `feat/` branch is rebuilt by topic
  ([Integration branches](../../../CONTRIBUTING.md#integration-branches), steps 4 and 7).
- A fix that spans commits is a commit of its own.
- Every fix on any other branch is a commit of its own, since `main` keeps what it merges.

The hand-off records in its *Review* section:

- who reviewed;
- the findings on both axes;
- what was fixed;
- the commit the reviewer was handed, on its `Reviewed at:` line.

The fixes come after that commit.
[`scripts/sensor-review-binding.sh`](../../../scripts/sensor-review-binding.sh) lists them for
the human who decides the merge, unless a second review sees them first and the line moves.
After a rework, the pull request's description carries that line.
