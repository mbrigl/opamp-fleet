# ADR-0002: Every accepted decision and every success criterion is verified by a test that cites it, and the checks that hold the decisions are bash scripts under `scripts/`

- **Status:** 🟢 accepted
- **Date:** 2026-09-16
- **Deciders:** NUC maintainer
- **Applies to:** every accepted ADR, the identifiers by which ADRs are cited, the Goals and Quality Goals of `docs/SPECIFICATION.md`, the tests that verify them, the checks and sensors under `scripts/`, and the git hooks under `.githooks/`

## Context

This template drives development from a specification: authority runs **specification → accepted
ADRs → task** ([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules)). Neither end of that chain is
connected to anything executable. The Definition of Done requires tests for new behaviour
([`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done)), but nothing says which
decision or goal a test protects, and nothing notices a decision that no test protects. The
cheapest *Enforcement* entry an ADR can carry is "review only", and it is the one that protects
nothing.

Two forces limit the answer. Some decisions cannot be decided by a program — that only a human
flips an ADR's status, that an effort too large for one session was planned before it was built —
and demanding tests for them produces tests that assert a constant. And this template ships no
language toolchain, so the rule can prescribe only something every language's tests can carry.

A marker cites an ADR by an identifier, and so do links, commits, and other ADRs. Work runs on
several branches at once and meets on `feat/` branches before it reaches `main`
([`AGENTS.md` §6](../../AGENTS.md#6-project-rules)); an ADR numbered when it is written takes
the number a parallel branch takes too, and every merge that meets one renumbers what its branch
wrote. The number is needed only once the ADR is on `main`, where it is permanent.

## Decision

We will fail CI on any accepted ADR that no test cites with a `Verifies: <id>` marker, and report
every criterion under **Goals / Success Criteria** or **Quality Goals** in the specification that
no test cites.

- **Identifiers.** Every list item in those two sections starts with a bold `G-n` (Goals) or `Q-n`
  (Quality Goals), as in `- **G-n** — …`; an item without one fails. Identifiers are never
  reused. Only a normalization renumbers them, and it moves every marker with them
  ([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules), rule 5).
- **Marker.** `Verifies:` followed by one or more identifiers (`ADR-NNNN`, `ADR-draft-<slug>`,
  `G-n`), in any file of the repository that git does not ignore and that is not Markdown. It is
  plain text so that every language can carry it.
- **Draft identifiers.** An ADR that has not reached `main` is a draft: the file
  `docs/adr/draft-<slug>.md`, its slug lower case letters and digits joined by hyphens, headed
  and cited as `ADR-draft-<slug>` wherever a numbered ADR is cited by its number. On the branch
  whose pull request brings it to `main`, once that branch has taken up the current `main`,
  `scripts/number-adrs.sh` gives each draft the next free number, in the order the drafts were
  added, in two commits: the first changes only `docs/adr/`, the second only what lies outside
  it, and its body carries the map from each draft identifier to its number. Neither commit
  changes ADR text together with another path
  ([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules), rule 4); the checks fail on the first and pass
  on the second. Commits before the numbering keep the draft identifiers, and the map resolves
  them. No draft reaches `main`: a pull request into it that is ready for review fails on one,
  and so does a push to it; a draft pull request, which tests a `feat/` branch still under
  rework, passes.
- **Exception.** An ADR whose decision no test can decide — because only a human can, or because
  it is about how the project works rather than what the system does — says so in its
  *Enforcement* section, in a paragraph starting `**Not mechanically decidable:**` with the reason.
  Criteria have no exception: a criterion no test can decide is not an acceptance criterion, and is
  rewritten until one can.
- **Criteria.** A criterion no test cites is pending work, and the check lists it; it does not
  fail. Nothing in the specification marks the state, so meeting a goal touches the tests, not the
  constitution.
- **Invalid citation.** A marker citing an identifier that does not exist, or an ADR that is
  superseded or rejected, fails.
- **The checks.** Every check and sensor the template ships is a bash script under `scripts/`
  that needs nothing beyond bash 4.4 or later, git, and the standard tools of the base image
  (coreutils, grep, sed, awk, find, tar), the shell lint's ShellCheck aside, and runs after its
  own self-test on fixture repositories. A git hook under `.githooks/` needs no more than bash
  3.2, because it runs before any check can say which bash it found. A sensor fails on nothing it
  counts until a decision turns it into a check. `scripts/check-all.sh`, the `checks.yml`
  workflow, and the README's required checks list the same ones.
- **The local gate.** The git hooks under `.githooks/` run the checks before a commit, a merge,
  and a push, and refuse what [`AGENTS.md` §6](../../AGENTS.md#6-project-rules) keeps from
  `main`. With `--changed`, `scripts/check-all.sh` runs a self-test only where its `# Inputs:`
  line names a path the branch changed; CI runs every one.
- **The sensors the template ships** are the scripts `scripts/sensor-*.sh`, each saying in its
  header what it counts. Each self-test cites this ADR.
- **Sensors of the GitHub repository.** A sensor that reads what only the GitHub repository
  holds, such as its rulesets and its security settings, may need the GitHub CLI `gh` as well,
  authenticated through its web flow. It runs on a human's instruction, not in
  `scripts/check-all.sh` or `checks.yml`: the workflow token cannot read every setting, and a
  pre-push hook that depends on the network refuses work for a reason the work does not have.
  Its self-test runs like any other, on fixture responses in place of the API, so it needs no
  `gh` and stays in the three lists.

`proposed` ADRs are out of the check's scope: their implementation and tests land while they are
still proposed, and the acceptance lands with them ([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules)).

**Out of scope:** whether a citing test is adequate, which stays with review; whether the tests
pass, which is the project's own build-test job; and which test framework, layout, or kind of test a
project uses.

## Alternatives considered

- **Keep review-only enforcement as a legitimate default.** The state this ADR ends. A coverage
  report that fails nothing is the same thing in a new format.
- **Tests without any exception.** Does not remove the undecidable decisions, it hides them behind
  tests that assert a constant — a check that counts the headings of an issue template protects
  the template, not the decision.
- **A pending mark on each criterion in the specification.** Lets the check fail on an unmarked
  criterion, at the price that every pull request meeting a goal becomes a specification change,
  which a human alone may land. The report says the same without touching the document.
- **Count a marker only in test files.** Stronger, but "test file" has no language-neutral
  definition. A project that has one narrows the check through its own ADR.
- **A language-specific mechanism** (annotations, tags, ArchUnit-style rule classes). Stronger where
  it exists, but a template without a toolchain cannot prescribe one; a project can add one on top
  of the marker.
- **Every check on bash 3.2, the one macOS ships.** Spares a contributor on macOS the install of
  a current bash, at the price of rewriting the `mapfile` calls, the associative arrays, and the
  case conversions of the checks. The Dev Container and CI carry a current bash, and a check that
  refuses an old one says so at once.
- **Number an ADR when it is written,** the practice of Nygard's ADRs and `adr-tools`. Two
  branches that propose at once collide, and a number names two ADRs until one is renumbered.
  A placeholder in the form of a number (`XXXX-<slug>`) looks like one to every check that
  matches four characters; slugs alone drop the short name and the reading order; the pull
  request's number is unknown when the ADR is written and leaves gaps in `0001..N`.
- **Number in one commit,** excepted from rule 4, or with a temporary alias in the ADR's header
  until the references have moved. Every commit stays green, at the price of an exception to
  the rule that keeps a reviewed ADR apart from its implementation, or of a third commit.
- **Run the settings sensor in CI.** The workflow token reads the rules of a branch, but the
  security settings only with admin rights, so CI would need a stored token
  ([`AGENTS.md` §6](../../AGENTS.md#6-project-rules) forbids one) or report half of them.

## Sources / Prior art

- Birgitta Böckeler, *Harness engineering* —
  <https://martinfowler.com/articles/harness-engineering.html>: a harness is guides that feed
  forward and sensors that feed back, and the behaviour harness is the least mature; this ADR
  wires a decision to a sensor. *Sensors for coding agents* —
  <https://martinfowler.com/articles/sensors-for-coding-agents.html>: coverage says a line ran,
  not that its effect was verified — why a marker proves a claim, not adequacy.
- Addy Osmani, *Agentic code review* — <https://addyosmani.com/blog/agentic-code-review/>: a
  green check over edited tests means nothing until the edits are confirmed; the reason every
  changed test needs a reason in review.
- Python's PEP 1 — <https://peps.python.org/pep-0001/>: the author names a draft by the next
  number no published or open PEP uses, and an editor confirms it or assigns one; two drafts in
  flight still meet on a number, the reason a draft here takes none until its merge.

## Consequences

- Positive: "which test protects this decision?" and "is this goal met?" become lookups; a decision
  nobody verifies fails CI when it is accepted, not when it is found broken later; *Enforcement*
  stops being free text that can say "none".
- Negative / trade-offs: accepting an ADR requires its tests in the same change. A marker
  proves a claim, not adequacy — a test that cites and asserts nothing passes, and so does a marker
  in a file that is no test. A criterion that loses its test is visible only in the check's report
  and in the diff that deleted the test. Quality goals must be phrased so a test can decide them.
  An ADR has two names in the history, the draft identifier and the number, joined by the map
  in one commit's body, and the first numbering commit is red on its own.

## Enforcement

`scripts/check-traceability.sh`, run by `scripts/check-all.sh` and as its own job in
[`checks.yml`](../../.github/workflows/checks.yml), fails on every violation listed in the decision
above and lists the criteria without a test. It ships with a self-test that cites this ADR.
Check 9 of [`scripts/check-docs.sh`](../../scripts/check-docs.sh) holds the three lists of checks
together and fails a workflow job that runs no script, and
[`scripts/check-shell.sh`](../../scripts/check-shell.sh) lints every script. That a check stays
free of further dependencies and runs after its self-test is review. That a sensor of the GitHub
repository runs on instruction only, and its self-test without `gh`, is review.
`scripts/check-all.sh` refuses a bash below 4.4 with a message that names the one it found.
[`scripts/test-git-hooks.sh`](../../scripts/test-git-hooks.sh) exercises every refusal of the
hooks, and the self-test of each sensor cites this ADR. Checks 2 and 16 of
`scripts/check-docs.sh` hold a draft's name against its file and keep drafts from `main`, and
the self-test of `scripts/number-adrs.sh` holds the two commits, the map, and that the review
binding sensor lists neither; both self-tests cite this ADR.
