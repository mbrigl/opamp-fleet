<!-- The hand-off of a change: the description of its pull request. The pull request of a feat/
     branch hands off the whole branch and gathers the hand-offs of the branches it integrates
     ([NUC-main-only-via-pr §5](../docs/adr/NUC-main-only-via-pr.md)). Keep changes small and
     reviewable. Write everything in English. State what a reviewer can verify against the diff,
     a test, or an output; a claim nothing shows is one the reviewer has to redo. -->

## Summary

<!-- What does this change do, and why? -->

## Related

- Ticket:  <!-- the file under docs/plan/ this deletes (NUC-large-work-is-planned §5), or the
            request it answers -->
- Implements:  <!-- the ADR this implements or relates to, by its identifier,
     or none -->
- ADRs reviewed at:  <!-- the commit of ADRs only the human reviewed before the implementation,
     or none
     ([NUC-decisions-are-proposed-then-accepted §4](../docs/adr/NUC-decisions-are-proposed-then-accepted.md))
     -->

## Proof

<!-- What shows that this does what the ticket asks and meets the bar of AGENTS.md
     [§5](../AGENTS.md#5-quality-bar--definition-of-done). The tests that decide the acceptance
     criteria, by name. Every existing test the diff changes or removes, each with the reason the
     behaviour changed; "none" is the common answer and the reviewer checks it against the diff.
     The result of scripts/check-all.sh, the project's test run, and the sensors the toolchain
     ADR names. A demo, such as a command and its output or a screenshot, where a test cannot
     show it. -->

- Tests:
- Tests changed:
- Checks:
- Demo:
- Measures:  <!-- what the work cost, in this fixed form, so a reader can collect it:
     'tokens <n> | checks <n> s | review rounds <n>'. Tokens are those the sessions read and
     wrote, as the harness reports them; checks are the seconds of the last full run of
     scripts/check-all.sh; review rounds count the reviews of the change, the first and each that
     saw the fixes of another, as the Review section records them. A value nothing measured
     stands as '-'. -->

## Doubts

<!-- Disagreed: what in the task, its framing, or the existing code the implementer disagreed
     with, each with its evidence. Unsure: what the implementer chose or read without certainty,
     for the human or the reviewer to settle. Could not confirm: what could not be confirmed
     against the specification, the ADRs, the code, or a source
     (AGENTS.md [§1](../AGENTS.md#1-principles)). A derived project acts on: what a project
     must do when it takes the change up with a release, such as a changed interface of a
     script, a check that newly reaches a project's own files, a moved key, or a removed file,
     so that the notes of a release can name it; in a project that ships no releases it is
     always "none". "None" is a claim the reviewer checks, in every field. -->

- Disagreed:
- Unsure:
- Could not confirm:
- A derived project acts on:

## Documents and decisions

<!-- One statement each, verifiable against the diff. ADR: new, revised, or none, and why none
     ([NUC-decisions-are-proposed-then-accepted §2](../docs/adr/NUC-decisions-are-proposed-then-accepted.md));
     a new or revised `proposed` ADR ships in commits of ADRs only
     ([NUC-decisions-are-proposed-then-accepted §3](../docs/adr/NUC-decisions-are-proposed-then-accepted.md)).
     ARCHITECTURE.md: updated, or unchanged because the structure is unchanged
     ([§5](../AGENTS.md#5-quality-bar--definition-of-done)). CHANGELOG.md: the entry this change
     adds, or none because no running deployment sees the change, which scripts/sensor-changelog.sh
     asks of a fix or feat to the shipped code. Glossary: the terms this change settled, or none.
     Specification: the accepted ADRs a change to it moves
     ([NUC-documents-are-fixed §2](../docs/adr/NUC-documents-are-fixed.md)), or untouched. -->

- ADR:
- ARCHITECTURE.md:
- CHANGELOG.md:
- Glossary:
- Specification:

## Review

<!-- The fresh-context review that ran before this hand-off, the review skill under
     .agents/skills/ ([§5](../AGENTS.md#5-quality-bar--definition-of-done)): who or what reviewed,
     its findings on the two axes (does it do what was asked, does it meet the bar), and what
     was fixed. The commit the review saw, by hash: scripts/sensor-review-binding.sh lists what
     landed after it. In the description of a feat/ branch's pull request, the review of the whole
     branch at its head, which covers every commit it carries. -->

- Reviewed at:
