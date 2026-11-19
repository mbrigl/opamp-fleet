# NUC-main-only-via-pr: Work is committed on branches, a human decides what leaves the machine, and `main` changes only through a merged pull request

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** the git hooks under `.githooks/`, the git settings the Dev Container sets for them, every commit, merge, and push, the integration steps on `feat/` branches, the repository settings of the remote, the sections *Git hooks*, *Integration branches*, and *Repository settings* of `CONTRIBUTING.md`, `scripts/ready-for-main.sh`, `scripts/drop-merged-branch.sh`, `scripts/sensor-repo-settings.sh`, and `scripts/sensor-review-binding.sh`

## Context

Agents commit, merge, and push as fast as they type. Without rules on integration, four things go
wrong.

- **Work leaves the machine unseen.** An agent that pushes, opens a pull request, or rewrites
  history on its own acts where a human can no longer take it back quietly. A push is public, and
  a reset or a clean loses work nobody reviewed. An instruction for one push read as leave for the
  next one makes every later push unasked.
- **`main` moves without a review.** A commit on the local `main`, a fast-forward to a local
  branch, or a push to the remote's `main` skips the pull request, the required checks, and the
  Code Owners review. The local `main` then differs from the remote's, and every later branch
  starts from a state nobody else has.
- **The history loses what was reviewed.** A squash merge folds a branch into one commit. A
  rebase merge rewrites its commits with new identifiers. Both discard the merges an integration
  branch carries, and with them each branch's hand-off. A rewrite of `main` breaks every clone and
  every citation of a commit.
- **A gate binds one agent only.** A permission prompt or a deny list in one agent's configuration
  stops that agent. The next agent, or a human, passes it. A rule that rests on such a gate looks
  enforced and is not.

Several forces bound the answer. Git does not version hooks, and `--no-verify` skips most of them,
so a hook holds only for those who keep it on. The remote's settings hold for everyone who pushes,
but they see only what reaches the remote. An administrator may bypass a ruleset. Several branches
reviewed apart collide only when they meet, so some work has to meet before it reaches `main`.

The binding sensor routes a change only to ADRs. Rules on the hooks and the integration steps
that stand in no ADR reach no session that changes them.

## Decision

We will commit only on branches, leave pushes and rewrites to a human, and change `main` only
through merged pull requests.

1. **Branches.** An agent may commit on a branch without an instruction. Nobody may commit on
   `main`.
2. **Instruction.** A human decides what leaves the machine.
   - An agent must wait for a human's instruction before a push, a `gh` command, or anything that
     rewrites or discards history, such as `reset`, `rebase`, `merge`, `restore`, or `clean`.
   - An instruction covers the action it names and no next one.
   - A standing instruction may cover only the claim of a ticket, where the harness ADR grants it.
     The claim is the push that creates its branch across clones, or the assignment of its issue.
3. **Credentials.** A human authenticates `gh` through its web flow and enters its one-time code.
   An agent must not request, store, or hard-code a personal access token.
4. **`main`.** A change must reach `main` through a pull request that merges as a merge commit.
   The local `main` must only move to a commit that the remote's `main` has. Nobody may rewrite
   what has reached `main`.
5. **Integration branch.** On a human's instruction, a branch reviewed per
   [`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done) may integrate first on a
   `feat/` branch.
   - It integrates linearly: its commits are rebased onto the `feat/` branch, which moves forward
     to them. A `feat/` branch carries no merge commit.
   - Before its pull request into `main`, the `feat/` branch is rebuilt on `main` as one commit per
     topic. A topic that changes ADRs carries them in a commit of ADRs only, before its code. Each
     topic's message names the work it folds.
   - The description of that pull request is the hand-off of the whole branch, with a review of
     the whole branch at its head.
   - Until the `feat/` branch merges into `main`, the human may rework and push all of it again.
6. **Hooks.** The git hooks under `.githooks/` must enforce each rule of this ADR that a file
   decides.
   - An agent must not bypass a hook: no `--no-verify`, no git without the hooks, no hand-moved
     `origin/main`, and no hand-written record of a green tree.
   - Where the harness ADR grants the push of a claim, that push may skip the pre-push hook.
   - A human may bypass a hook deliberately.
7. **What enforces a rule.** Only what every agent hits enforces a rule: the git hooks, the
   checks, and the repository settings.
   - The repository settings must enforce on the remote what no file enforces.
   - A tool's own configuration enforces no rule, and no rule may rest on it. It may close a path
     for its own agent as defence in depth.

**Out of scope:**

- making and releasing a claim: [NUC-large-work-is-planned](NUC-large-work-is-planned.md) and the
  `work-tickets` skill;
- the review and the checks before a push:
  [`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done);
- the commands, refusals, and settings, which `CONTRIBUTING.md` lists.

## Latitude

A project may choose:

- whether its harness grants the standing instruction for the push of a claim, which the harness
  ADR records ([catalogue entry](../../.agents/skills/propose-adr/concepts/harness.md));
- whether branches integrate on a `feat/` branch first, and whether a pull request against it
  tests a branch before the fast-forward;
- further hooks, checks, and repository settings that hold its own rules.

The catalogue has no entry for the last two choices, and neither needs an ADR.

## Alternatives considered

- **Push to `main` directly, trunk-based.** It is fast and needs no pull request. Every push
  skips the required checks and the Code Owners review, and a red tree reaches every clone.
- **Squash merges.** The history of `main` gets one commit per change. The topics of a `feat/`
  branch and its commits of ADRs only fold into one, and the merge of its pull request is lost.
- **Rebase merges.** The history stays linear. GitHub writes new commits with new identifiers, so
  the commits a review saw no longer exist.
- **One merge per integration on a `feat/` branch, its hand-off as the message.** Each reviewed
  branch keeps its own merge. The history of `main` grows a merge per ticket, nested under the
  merge of the pull request, and reads by the tickets, not by the topics. The hand-offs repeat what
  the pull request's description says for the whole branch.
- **Gates in each tool's configuration.** A permission prompt or a deny list is quick to write.
  It holds for one agent only, and a prompt asks about a command, not the branch it lands on.
- **Repository settings alone.** They hold for everyone on the remote. A red push still leaves the
  machine, a local `main` still drifts, and an administrator may bypass them.
- **Hooks alone.** They stop the mistake before it leaves the machine. Git does not version them,
  `--no-verify` skips most of them, and a fresh clone runs none until it enables them.
- **A standing instruction for every push.** It saves the human a step per push. It lets work
  leave the machine unseen, which is the first failure above.
- **A personal access token for `gh`.** It lets an agent act on the remote without a human. A
  stored token leaks with the file or the log that holds it.

## Sources / Prior art

- GitHub, *About merge methods on GitHub* —
  <https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/about-merge-methods-on-github>:
  a squash merge combines the commits into one, and a rebase merge writes new commits with new
  identifiers.
- GitHub, *Available rules for rulesets* —
  <https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets>:
  a required pull request, allowed merge methods, required status checks, an up-to-date branch,
  blocked force pushes and deletions, the Code Owners review, and the bypass list.
- GitHub, *About protected branches* —
  <https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches>:
  the older form of the same protection.
- Git, *githooks* — <https://git-scm.com/docs/githooks>: `--no-verify` skips `pre-commit` and
  `pre-merge-commit`, and a non-zero exit of `reference-transaction` in its prepared state aborts
  the update.
- GitHub CLI, *gh auth login* — <https://cli.github.com/manual/gh_auth_login>: the web flow is the
  default, and the token goes to the system's credential store.

## Consequences

- Positive: nothing reaches `main` or the remote without a human and the required checks.
- Positive: the history of `main` reads one commit per topic, and the merge of each pull request
  carries the hand-off of its branch.
- Positive: the binding sensor routes a change to the hooks or the integration steps here.
- Negative: every push waits for a human, and an agent cannot finish an integration alone.
- Negative: hooks are enabled per clone, so a clone without them relies on the remote alone.

## Enforcement

Four self-tests cite this ADR:

- `scripts/test-git-hooks.sh`: no commit or merge on `main` (clause 1), no push to it or move of
  it past `origin/main` (clause 4). It also refuses a merge on a `feat/` branch (clause 5);
- `scripts/test-ready-for-main.sh` and `scripts/test-drop-merged-branch.sh`: no fixup reaches
  `main`, and the local `main` only takes up the remote's (clause 4). The readiness script also
  refuses a branch that carries a merge (clause 5);
- `scripts/test-sensor-repo-settings.sh`: the sensor reports each repository setting that differs
  (clause 7).

The ruleset on `main` holds clause 4 on the remote. Given the pull request's description with
`--hand-off`, `scripts/sensor-review-binding.sh` reports whether the review stands at the head.
It also lists a merge on the branch (clause 5), and it never fails.

**Not mechanically decidable:** the review holds clauses 2 and 3, and whether a `feat/` branch is
folded by topic before its pull request (clause 5). It also holds a bypass under clause 6, and
clause 7 beyond the repository settings.
