# NUC-decisions-cite-tests: Every accepted decision and every criterion of the specification is verified by a test that cites it

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** every accepted ADR and its *Enforcement*, the Goals and Quality Goals of `docs/SPECIFICATION.md`, every `Verifies:` marker, `scripts/check-traceability.sh`, and `scripts/test-check-traceability.sh`

## Context

This template drives development from a specification. Authority runs from the specification to
the accepted ADRs to the task
([NUC-decisions-are-proposed-then-accepted §7](NUC-decisions-are-proposed-then-accepted.md)).
Neither end of that chain is connected to anything executable. The Definition of Done requires tests
for new behaviour ([`AGENTS.md` §5](../../AGENTS.md#5-quality-bar--definition-of-done)). Nothing
says which decision or goal a test protects, and nothing notices a decision that no test protects.
The cheapest *Enforcement* an ADR can carry is "review only", and it protects nothing.

Two forces bound the answer. Some decisions cannot be decided by a program: only a human flips an
ADR's status, and only a reviewer sees that a large effort was planned before it was built. Tests
demanded for them assert a constant. And the template ships no language toolchain, so the rule
can prescribe only what the tests of every language can carry.

## Decision

We will fail CI on an accepted ADR that no test cites, and report each criterion of the
specification that no test cites.

1. **Criterion identifiers.** Every item under **Goals / Success Criteria** must start with a bold
   `G-n`, and every item under **Quality Goals** with a bold `Q-n`. Nobody may reuse one. Only a
   normalization renumbers them, and it must move every marker
   ([NUC-decisions-are-proposed-then-accepted §8](NUC-decisions-are-proposed-then-accepted.md)).
2. **Marker.** A test cites what it verifies by `Verifies:` and one or more identifiers of ADRs or
   criteria. The marker is plain text in any file git does not ignore, except Markdown.
3. **Coverage.** The check fails on an accepted ADR that no marker cites. It ignores proposed ADRs,
   whose tests land before the acceptance
   ([NUC-decisions-are-proposed-then-accepted §4](NUC-decisions-are-proposed-then-accepted.md)).
4. **Exception.** An ADR whose decision no test can decide must say so in its *Enforcement*, in a
   paragraph `**Not mechanically decidable:**` with the reason. It qualifies only when a human
   decides it, or when it governs how the project works. The paragraph exempts it from clause 3.
5. **Criteria.** A criterion that no marker cites is pending work, which the check lists without
   failing. The specification marks no state of a criterion. A criterion has no exception: one
   that no test can decide must be rewritten until a test can.
6. **Invalid citation.** A marker fails when it names an unknown identifier, or a superseded or
   rejected ADR.

**Out of scope:**

- the adequacy of a citing test, which stays with review;
- whether the tests pass, and the test framework, layout, and kinds, which the `toolchain` role
  ADR decides ([NUC-base-is-fixed §3](NUC-base-is-fixed.md));
- the form of an identifier ([NUC-adrs-are-named-by-category §1](NUC-adrs-are-named-by-category.md))
  and the language of the checks ([NUC-checks-are-bash](NUC-checks-are-bash.md)).

## Latitude

A project may choose:

- its test framework, layout, and kinds of test, from the entry `toolchain` of the
  [catalogue](../../.agents/skills/propose-adr/concepts/toolchain.md);
- a language-specific mechanism on top of the marker, such as annotations or tags;
- a narrower place for the marker, such as test files only. That choice narrows clause 2 in an
  ADR of its own ([NUC-base-is-fixed §1](NUC-base-is-fixed.md)).

The catalogue has no entry for the second choice, and it needs no ADR.

## Alternatives considered

- **Review-only enforcement as a legitimate default.** This is the state the ADR ends. A coverage
  report that fails nothing is the same thing in a new format.
- **Tests without any exception.** It does not remove the undecidable decisions. It hides them
  behind tests that assert a constant. A check that counts the headings of an issue template
  protects the template, not the decision.
- **A pending mark on each criterion in the specification.** The check could then fail on an
  unmarked criterion. Every pull request that meets a goal would change the specification, which
  only a human may land. The report says the same without touching the document.
- **A marker counts only in test files.** It is stronger, but "test file" has no
  language-neutral definition. A project that has one narrows clause 2 in its own ADR.
- **A language-specific mechanism,** such as annotations, tags, or ArchUnit-style rule classes.
  It is stronger where it exists, but a template without a toolchain cannot prescribe one. A
  project may add one on top of the marker.

## Sources / Prior art

- Birgitta Böckeler, *Harness engineering* —
  <https://martinfowler.com/articles/harness-engineering.html>: a harness is guides that feed
  forward and sensors that feed back, and the behaviour harness is the least mature. This ADR
  wires a decision to a sensor.
- Birgitta Böckeler, *Sensors for coding agents* —
  <https://martinfowler.com/articles/sensors-for-coding-agents.html>: coverage says a line ran,
  not that its effect was verified. That is why a marker proves a claim, not adequacy.
- Addy Osmani, *Agentic code review* — <https://addyosmani.com/blog/agentic-code-review/>: a
  green check over edited tests means nothing until the edits are confirmed. That is why every
  changed test needs a reason in review.

## Consequences

- Positive: "which test protects this decision?" and "is this goal met?" become lookups.
- Positive: a decision nobody verifies fails CI when it is accepted, not when it is found broken.
- Positive: *Enforcement* stops being free text that can say "none".
- Negative: accepting an ADR requires its tests in the same change.
- Negative: a marker proves a claim, not adequacy. A test that cites and asserts nothing passes,
  and so does a marker in a file that is no test.
- Negative: a criterion that loses its test shows only in the check's report and in the diff
  that deleted the test.
- Negative: quality goals must be phrased so that a test can decide them.

## Enforcement

`scripts/check-traceability.sh` holds these clauses. `scripts/check-all.sh` runs it, and so does
its own job in [`checks.yml`](../../.github/workflows/checks.yml). Its self-test
`scripts/test-check-traceability.sh` cites this ADR.

**Not mechanically decidable:** the review holds three judgements:

- whether the identifier of a deleted criterion is used again (clause 1);
- whether the reason of an exception holds (clause 4);
- whether a test can decide a criterion (clause 5).
