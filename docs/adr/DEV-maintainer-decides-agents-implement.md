# DEV-maintainer-decides-agents-implement: One maintainer decides, any agent that reads AGENTS.md implements, a fresh context and a CODEOWNERS reviewer review, and nothing runs unattended

- **Status:** 🟢 accepted
- **Date:** 2026-10-26
- **Deciders:** Markus Brigl
- **Applies to:** every session that works in the repository and every pull request, `AGENTS.md`, `.claude/`, the `review` skill under `.agents/skills/review/`, `.github/CODEOWNERS`, and the rulesets on `main`
- **Role:** harness

## Context

Every session works inside the harness: which agents work in the project, who reviews what, who
decides, and what may run unattended. [`AGENTS.md` §4](../../AGENTS.md#4-working-style) names the
harness ADR as the place for the parts kept with the human and for the cap on open agent work. A
task runs unattended only when its ticket fits under both.

The project is complex. Its work spans sessions, and the specification's *Threats & Forbidden
Actions* name what is worth protecting, among them every host the fleet reaches and the
operator's signing key. Yet it has one maintainer: the specification's *Constraints* say that one
maintainer decides and that coding agents do most of the work under the rules of
[`AGENTS.md`](../../AGENTS.md). The harness fits both. It leaves room for more than one agent and
for parallel sessions, and it keeps review and merge at a pace one human can follow.

The agents work under the maintainer's own GitHub account, and GitHub refuses an approval from the
author of a pull request. A required approval alone would block every pull request the maintainer
opens. A second account to open them would be one more identity with write access to keep and to
protect.

What the repository holds today:

- `AGENTS.md` is the rule file, written for no vendor. The only agent configuration in the tree is
  Claude Code's under `.claude/`. Its `CLAUDE.md` points to `AGENTS.md`, and its `skills/` holds
  one pointer per skill into `.agents/skills/`.
- The `Deciders` lines of the project's own ADRs name Markus Brigl. The template's ADRs are the
  `NUC-` base, which the project carries as the release its `README.md` names
  ([NUC-base-is-fixed §1](NUC-base-is-fixed.md)).
- [`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done) requires a review in a fresh
  context before a change is handed off, through the `review` skill, and never by the session that
  wrote the change.
- Changes reach `main` only through a pull request
  ([`AGENTS.md` §6](../../AGENTS.md#6-project-rules)).
  [`.github/CODEOWNERS`](../../.github/CODEOWNERS) names the maintainer for
  `docs/SPECIFICATION.md` and `docs/adr/`.
- No ticket is dispatched.

## Decision

We will keep implementer and reviewer apart in a harness where one maintainer decides and nothing
runs unattended.

1. **One maintainer decides.**
   - The maintainer, Markus Brigl, decides every ADR and lands every change to the specification.
   - `.github/CODEOWNERS` names the maintainer for `docs/SPECIFICATION.md` and `docs/adr/`.
   - `.github/CODEOWNERS` may name further reviewers. The maintainer decides who they are.
2. **Any agent that reads `AGENTS.md` may implement.**
   - `AGENTS.md` is the one rule file. This decision prescribes no agent and no model.
   - An agent's own configuration, such as Claude Code's under `.claude/`, points to `AGENTS.md`
     and the shared skills.
   - That configuration must state no rule of its own.
3. **A fresh context reviews every change before its pull request opens.** The review runs through
   the `review` skill. A session other than the one that wrote the change must run it.
4. **A reviewer that `.github/CODEOWNERS` names reviews every pull request before it merges.** The
   maintainer may review their own pull request. Two rulesets on `main` hold this:
   - The ruleset `main` requires a pull request, the merge method merge commit, the required
     checks, and a branch up to date with `main`. It blocks force pushes and the deletion of the
     branch. Nobody bypasses it, the maintainer included. So only a pull request that passed the
     checks reaches `main`.
   - The ruleset `main-review` requires one approval from a reviewer that `.github/CODEOWNERS`
     names. The admin role bypasses it, for pull requests only. The maintainer reviews their own
     pull request, records the review on it, and merges it without an approval. Every other pull
     request waits for such a reviewer who did not open it.
5. **Nothing runs unattended.**
   - No session works without a human.
   - Nobody dispatches a ticket to a session without a human.
   - The cap on open unattended agent work is zero. So every part of the system is kept with the
     human.
6. **Parallel sessions work in git worktrees of their own.**

**Out of scope:**

- which model implements or reviews;
- who besides the maintainer `.github/CODEOWNERS` names, and whether the parts the specification's
  threats protect need a second reviewer;
- how many attended sessions may be open at once;
- the command that starts a session without a human, and the remote on which a ticket is claimed.
  Both matter only once work runs unattended.

## Alternatives considered

- **One agent, the human reviews every change.** The smallest form: one agent and model, the human
  reading every pull request, nothing unattended, no parallel sessions. It leaves no room for a
  second agent or for sessions that run in parallel.
- **Parallel tickets under a cap.** The form a complex project starts from. Tickets whose *Mode*
  allows it run without the human, several at once. A dispatching session claims each in a
  worktree of its own and works it through the `work-tickets` skill. The open agent work is capped
  at what the humans can review. Not taken: no work runs unattended here, and taking it up changes
  this decision.
- **A second account for the agents.** A machine user or a GitHub App would open the agents' pull
  requests. The maintainer could then approve them, and every pull request could require an
  approval. Not taken: it is one more identity with write access, and its token one more secret to
  keep and to rotate. The maintainer reviews their own work either way.
- **No required approval at all.** The checks alone guard `main`, and the review is a habit. Not
  taken: a pull request of anyone but the maintainer would merge without a review by a reviewer
  that `.github/CODEOWNERS` names.
- **A framework that brings agents, artifacts and pipeline in one piece and runs the process
  itself.** Declined by [NUC-procedures-are-skills](NUC-procedures-are-skills.md) and
  [NUC-large-work-is-planned](NUC-large-work-is-planned.md).
- **A gate in one agent's own configuration as the enforcement of a rule.** A gate one agent
  honours and the next does not looks like a rule and is not one
  ([`AGENTS.md` §6](../../AGENTS.md#6-project-rules)).
- **Superseding named clauses of an ADR, the rest staying accepted.** A short successor would
  change a detail of a long ADR, and the old ADR's `Status` line would name the clauses it
  replaces. Not taken: the template supersedes an ADR whole
  ([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules), rule 5). A mechanic beside the template's would
  have to be carried through every release of the template taken up. A detail of a long ADR
  changes by a successor that restates it whole.

## Sources / Prior art

- Birgitta Böckeler, *Harness engineering*:
  <https://martinfowler.com/articles/harness-engineering.html>. Guides that feed forward and
  sensors that feed back, and committing early to what the sensors can hold.
- Addy Osmani, *Agentic autonomy levels*:
  <https://addyosmani.com/blog/agentic-autonomy-levels/>. How quickly a wrong result is noticed,
  how cleanly it is undone, and what proves it right.
- The harness entry of the `propose-adr` skill's concept catalogue,
  [`.agents/skills/propose-adr/concepts/harness.md`](../../.agents/skills/propose-adr/concepts/harness.md):
  the three forms and the questions a harness ADR answers.

## Consequences

- Positive: a session knows who decides, who reviews, and that it works with a human. Any agent
  that reads `AGENTS.md` can take up the work. Parallel sessions do not share a working tree.
- Negative / trade-offs:
  - Every change waits on one human's review and merge, and so does every proposed ADR.
  - The maintainer's own pull requests carry no approval that GitHub records, only the review the
    maintainer records on them.
  - With the bypass, the maintainer's account can merge a pull request nobody reviewed.
  - No ticket can run unattended under [`AGENTS.md` §4](../../AGENTS.md#4-working-style) until this
    decision changes.
- Follow-ups: the model that implements and the one that reviews; further reviewers in
  `.github/CODEOWNERS`, and a second reviewer for the parts the threats protect; a cap on attended
  parallel sessions; unattended tickets under a cap, once the work outgrows one maintainer's
  attended sessions.

## Enforcement

**Not mechanically decidable:** the tree cannot decide who reviews, who decides which ADRs, and
what runs unattended. The parts the harness relies on mechanically are checked elsewhere:

- The git hooks under [`.githooks/`](../../.githooks/) run the checks.
- [Repository settings](../../CONTRIBUTING.md#repository-settings) lists the two rulesets of clause
  4 with their required checks and the review that `.github/CODEOWNERS` drives.
- [`scripts/sensor-repo-settings.sh`](../../scripts/sensor-repo-settings.sh) compares that list
  with the repository on a human's instruction.
- The sensor does not read who may bypass a ruleset. That stands in the repository's settings.
