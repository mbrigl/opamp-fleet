# Agent Guide

> **The single rule file of this project**
> ([NUC-documents-are-fixed §3](docs/adr/NUC-documents-are-fixed.md)).
> It states each working rule or links the `NUC-` clause that carries it. Where another document
> repeats a rule, the wording here governs. The file belongs to the template, and a derived
> project must not edit it. A project extends or narrows a `NUC-` clause only by an ADR of its own
> ([NUC-base-is-fixed §1](docs/adr/NUC-base-is-fixed.md)). A project writes its
> conventions into [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md), into checks, and into ADRs. A
> procedure is a skill under [`.agents/skills/`](.agents/skills/) and states no rule of its own
> ([NUC-procedures-are-skills §3](docs/adr/NUC-procedures-are-skills.md)).

## 1. Principles

The human decides *why* and *what*. The agent works out *how*. The human also decides a *how*
that binds future choices. An ADR records it with its *why*
([NUC-decisions-are-proposed-then-accepted §1](docs/adr/NUC-decisions-are-proposed-then-accepted.md)).
Five principles win over speed and cleverness:

- **Simplicity.** The agent must build the simplest thing that satisfies the specification. It
  may add a dependency or an abstraction only for a present need. Removing code is progress. An
  ADR must justify complexity that is warranted.
- **Evolvability.** The project decides the essential architecture at the start. That
  architecture must stay open to change. To follow a technology, the project supersedes the ADR
  that chose it. Nobody may work around that ADR.
- **Proportionality.** Ceremony scales with how hard a change is to reverse. An agent that skips
  ceremony a reader expects must say so.
- **Reflection.** The agent must weigh alternatives and consequences. It must write the reasoning
  down.
- **Critical stance.** The agent must take nothing at face value: not the human's framing, not its
  own earlier output, not the existing code. It must verify against the specification, the ADRs,
  the code, and authoritative sources. It must put disagreement, with its evidence, into the
  hand-off of the change. It must put there what it was unable to confirm, too. "None" is a claim
  that the reviewer checks.

## 2. Start here

Before any non-trivial work, the agent must read the decisions that bind it. Of the rest it must
read only what the work needs.

1. **The ADRs.** The agent must run
   [`scripts/sensor-binding-adrs.sh`](scripts/sensor-binding-adrs.sh) `--sections`. While nothing
   has changed yet, it must name each path the work will touch with a `--path`.
   - The sensor prints the `NUC-` ADRs like any other. It reads each *Applies to* from the ADR
     file, not from the index [`docs/adr/README.md`](docs/adr/README.md).
   - Accepted ADRs are binding
     ([NUC-decisions-are-proposed-then-accepted §5](docs/adr/NUC-decisions-are-proposed-then-accepted.md)).
   - The agent must match each *Applies to* written in prose against the work by hand. Of each ADR
     that binds the work, it must read the header, *Decision*, and *Enforcement*.
   - The agent must read the rest of an ADR when the change questions, revises, or supersedes it.
     It must read the mechanics of the index when the change writes one.
   - A `proposed` ADR binds the work that implements it. Rejected and superseded ADRs explain
     *why*. They are not rules.
2. **The specification.** [`docs/SPECIFICATION.md`](docs/SPECIFICATION.md) holds the problem,
   the goals, and the success criteria, in the vocabulary of the
   [glossary](docs/GLOSSARY.md). The agent must read it when the work serves or moves a goal
   (`G-n`, `Q-n`), plans a feature, or decides an ADR.
3. **The overview.** [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) holds the system as it
   stands today, below the accepted ADRs
   ([NUC-documents-are-fixed §4](docs/adr/NUC-documents-are-fixed.md)). The agent must read it when
   the work changes the structure or has to find its way in it.

Scaffold text in what the work reads is the first work, such as a placeholder title, an empty
section, or a `TODO`. The agent must say so and elaborate it with the human before implementing.

## 3. ADR rules

The rules of the record stand in `NUC-` ADRs. The mechanics of the record are in
[`docs/adr/README.md`](docs/adr/README.md).

- Authority:
  [NUC-decisions-are-proposed-then-accepted §7](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
- The one rule file and citation by anchor:
  [NUC-documents-are-fixed §3](docs/adr/NUC-documents-are-fixed.md).

1. When an ADR is due:
   [NUC-decisions-are-proposed-then-accepted §1](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
   Skill: `propose-adr`.
2. Calibration:
   [NUC-decisions-are-proposed-then-accepted §2](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
3. Proposal:
   [NUC-decisions-are-proposed-then-accepted §3](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
   The exception for a normalization and for the template's correction:
   [NUC-decisions-are-proposed-then-accepted §4](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
4. Implementation:
   [NUC-decisions-are-proposed-then-accepted §4](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
   Status:
   [NUC-decisions-are-proposed-then-accepted §5](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
5. Immutability:
   [NUC-decisions-are-proposed-then-accepted §6](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
   Fixed identifiers:
   [NUC-adrs-are-named-by-category §1](docs/adr/NUC-adrs-are-named-by-category.md). Conflict with
   the specification:
   [NUC-decisions-are-proposed-then-accepted §7](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
   Normalization:
   [NUC-decisions-are-proposed-then-accepted §8](docs/adr/NUC-decisions-are-proposed-then-accepted.md).
   Skill: `normalize`.
6. Glossary: [NUC-documents-are-fixed §5](docs/adr/NUC-documents-are-fixed.md).
7. Change of the specification: [NUC-documents-are-fixed §2](docs/adr/NUC-documents-are-fixed.md).

## 4. Working style

- **Reply to the human in their own language.** Everything else must be English: code, comments,
  commits, docs, ADRs, pull requests.
- **Prefer small, reviewable changes.** Before a larger change, the agent must plan and explore.
  A commit or a pull request must name the ADR it implements, as `Implements <CAT>-<slug>`.
- **Cut work into vertical slices.** Each slice is a thin, tested path through every layer it
  touches. The first slice through new ground is the tracer bullet. A wide mechanical refactor is
  the one exception. It expands, migrates in batches, and contracts, and every step must stay
  green.
- **Write code and tests to [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md).** Where a new judgement
  call goes: [NUC-documents-are-fixed §6](docs/adr/NUC-documents-are-fixed.md).
- **Write a rule plainly.** This covers the rules of this file and the *Decision* and
  *Enforcement* of an ADR.
  - The author must write one statement per sentence.
  - The author must name who acts, in the active voice.
  - The author must use the terms of the glossary.
  - The author must write several items as a list.
  - The author must state a duty with *must* or *must not*, and a permission with *may*. *Should*,
    *could*, *might*, and *ideally* leave the reader guessing.

  [`scripts/sensor-plain-rules.sh`](scripts/sensor-plain-rules.sh) reports each sentence over 25
  words, each soft modal verb, and each word the glossary lists under _Avoid:_ on the lines a
  change adds. With `--require`, it fails on a soft modal verb or an avoided word in a project ADR.
- **Plan work larger than one session:**
  [NUC-large-work-is-planned §1](docs/adr/NUC-large-work-is-planned.md) to
  [NUC-large-work-is-planned §5](docs/adr/NUC-large-work-is-planned.md). Skill: `plan-feature`.
  The skill `work-tickets` works the tickets, one at a time or in parallel.
- **Record a finding outside your ticket in the backlog:**
  [NUC-large-work-is-planned §6](docs/adr/NUC-large-work-is-planned.md) to
  [NUC-large-work-is-planned §8](docs/adr/NUC-large-work-is-planned.md).
- **Work with the human.** The agent must work in small steps, surface its reasoning, and seek
  feedback early. A task may run unattended only when its ticket answers for all of the following:
  - a test decides its criteria;
  - its scope is isolated, and a wrong result costs one revert;
  - it touches nothing the harness ADR keeps with the human, and it fits under that ADR's cap on
    open agent work;
  - it says how a wrong result is noticed and undone, and what proves it right;
  - it names the stop past which the session hands off instead of trying on.

  The harness ADR is the project's ADR in the role `harness`
  ([NUC-base-is-fixed §3](docs/adr/NUC-base-is-fixed.md)). It decides who
  decides, which agents work, and how much agent work may be open at once.
- **Keep the context lean.**
  - Besides a review, the agent may delegate to a subagent only independent work or a search
    whose answer is a conclusion.
  - A subagent must return conclusions, not file dumps. Its brief must give paths, not the
    content of the files.
  - Alignment, implementation, and review must each start in a fresh session with only what it
    needs. No session may start by compacting the old one.
  - The implementing session loads what the *Read first* of its ticket names.
  - The agent must read a file by the sections the work needs. It must read a file whole where a
    rule or a procedure says so, or where the work judges the file as a whole.
  - The agent must run a command whose output is longer than the work reads through a filter for
    its purpose. It must read the exit status and a failure unfiltered.
  - A degraded session must hand off, and so must one whose topic is done. Every turn resends the
    whole session.
  - A session that hands off must leave a note under `.scratch/`. The note says what the next
    session needs, not what this one did.
  - A review's topic lasts until the fixes it asked for are judged.
- **Session artifacts are not tracked.** Session plans, handoff notes, and scratch files live
  under `.scratch/` (gitignored) or outside the tree. What has to outlive the session becomes a
  ticket, a `proposed` ADR, or a change to one of the six documents of
  [NUC-documents-are-fixed §1](docs/adr/NUC-documents-are-fixed.md). An agent's own memory must
  keep how the human works, not the state of the work. The tree and its history hold that state.
  Every session loads that memory first.
- **Research before a decision that is costly to reverse,** external sources included. The ADR
  must cite them. Below that bar, the agent must research what it is actually unsure of. It must
  not pass off recalled interfaces, versions, or defaults as verified.
- **Settle scope and intent with the human first when they are still open.** The trigger is
  disagreement about *why* and *what*, not size. **When in doubt, the agent must ask the human
  before implementing.**

## 5. Quality bar & Definition of Done

- **A change is done only when it builds, its tests pass, and linters and formatters are clean,**
  locally and in CI. Nobody may hand off a red change or propose merging it.
- **Run [`scripts/check-all.sh`](scripts/check-all.sh) before pushing.** It runs the repository's
  checks and the build, test, and lint commands of the README's **Build, Test & Run** section.
  CI runs the same on every pull request. The git hooks run the checks before a commit, a merge,
  and a push ([NUC-checks-are-bash §6](docs/adr/NUC-checks-are-bash.md)).
- **New behaviour must ship with tests.** A bug fix must ship with a regression test that fails
  before the fix and passes after. For behaviour that is genuinely untestable, the agent must say
  so and why.
- **The suite is the contract an implementation is held to.** So it must test through interfaces
  the implementation can change behind. Before changing what exists, the agent must pin the
  behaviour that must stay, with tests first where none exist. A rewrite or a migration is done
  when the old suite passes against the new implementation.
- **Tests cite the ADRs and criteria they verify:**
  [NUC-decisions-cite-tests §2](docs/adr/NUC-decisions-cite-tests.md) to
  [NUC-decisions-cite-tests §5](docs/adr/NUC-decisions-cite-tests.md). An invalid citation fails:
  [NUC-decisions-cite-tests §6](docs/adr/NUC-decisions-cite-tests.md).
- **Never weaken the suite to make it pass.** A flaky test is a defect, not noise. Nobody may
  retry it or re-run it until green.
- **Review in a fresh context before handing off.** Skill: `review`.
  - The session that wrote a change must not review it.
  - The reviewer must get the diff, the task, the doubts, and the standards. It must not get the
    reasoning.
  - It reports on two axes: whether the change does what was asked, and whether it meets this
    bar, simplicity first.
  - A finding that must be fixed must be fixed before the hand-off. A lesser one must be fixed or
    named in the doubts of the hand-off.
  - Each existing test that the diff changes or removes must have a reason.
  - The hand-off must name the commit the review saw. A sensor lists what landed after it. A
    sensor is a script that reports. It fails only where a decision makes it a check, as
    `--require` makes the plain-rules sensor
    ([NUC-checks-are-bash §4](docs/adr/NUC-checks-are-bash.md)).
  - A rework replaces the commits a review saw. After a rework, only a review of the whole branch
    at its head binds.
  - The review scales with the change ([§1](#1-principles)). A change with green checks gets a
    light review when it touches only these paths:
    - `.github/ISSUE_TEMPLATE/`;
    - documents under `docs/`, except `docs/plan/` and the set of
      [NUC-documents-are-fixed §1](docs/adr/NUC-documents-are-fixed.md).
  - A light review reads the diff, the task, the doubts, and the check output. Every other change
    gets the full review.
- **A feature spec closes only after a design revision:**
  [NUC-design-is-revised §1](docs/adr/NUC-design-is-revised.md) to
  [NUC-design-is-revised §6](docs/adr/NUC-design-is-revised.md). Skill: `design-revision`.
- **A mistake that a check would have caught must become a check,** shipped with the fix. A rule in
  prose is the fallback for what no check can decide. The design revision drops what prevented
  nothing.
- **Keep the diff releasable.** It must hold no commented-out code, no stray debug output, and no
  `TODO` standing in for a decision. Unfinished work must be a ticket or a `proposed` ADR, not
  hidden in the code.
- **A change to the structure updates [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md):**
  [NUC-documents-are-fixed §4](docs/adr/NUC-documents-are-fixed.md).

## 6. Project rules

The rules of integration stand in
[NUC-main-only-via-pr](docs/adr/NUC-main-only-via-pr.md).

- Branches, and the human's instruction before anything leaves the machine:
  [NUC-main-only-via-pr §1](docs/adr/NUC-main-only-via-pr.md) and
  [NUC-main-only-via-pr §2](docs/adr/NUC-main-only-via-pr.md).
- The claim of a ticket: [NUC-large-work-is-planned §4](docs/adr/NUC-large-work-is-planned.md).
  Its release, local or remote, waits for an instruction
  ([NUC-main-only-via-pr §2](docs/adr/NUC-main-only-via-pr.md)). Skill: `work-tickets`.
- The git hooks, and that no agent bypasses them:
  [NUC-main-only-via-pr §6](docs/adr/NUC-main-only-via-pr.md). What each hook refuses is under
  [Git hooks](CONTRIBUTING.md#git-hooks).
- What enforces a rule: [NUC-main-only-via-pr §7](docs/adr/NUC-main-only-via-pr.md).
- Credentials for `gh`: [NUC-main-only-via-pr §3](docs/adr/NUC-main-only-via-pr.md).
- `main` only through a pull request: [NUC-main-only-via-pr §4](docs/adr/NUC-main-only-via-pr.md).
  What no file can enforce is under [Repository settings](CONTRIBUTING.md#repository-settings).
- Integration on a `feat/` branch: [NUC-main-only-via-pr §5](docs/adr/NUC-main-only-via-pr.md).
  The steps are under [Integration branches](CONTRIBUTING.md#integration-branches).

## 7. Secrets

- **Never write a secret into a tracked file, commit message, ADR, log, or CI output.** Secrets
  live in environment variables, gitignored `.env*` files, or GitHub Actions secrets. A
  placeholder in a tracked `.env.example` is not a secret.
- **A secret that reaches git is compromised.** The human must rotate it first. Removing it from
  the branch tip is cleanup, not remediation. The agent must tell the human immediately.
- No check enforces either rule. [`SECURITY.md`](SECURITY.md) says what the repository's
  configuration does about secrets and where it stops.
