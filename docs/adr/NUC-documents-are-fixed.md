# NUC-documents-are-fixed: Six documents with one role each carry what a repository knows, and one rule file carries its working rules

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** `docs/SPECIFICATION.md`, `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/GLOSSARY.md`, `docs/CONVENTIONS.md`, `README.md`, `CONTRIBUTING.md`, `SECURITY.md`, `.github/CODEOWNERS`, `.github/ISSUE_TEMPLATE/`, `.github/PULL_REQUEST_TEMPLATE.md`, every per-agent pointer file, the rules `docs/links`, `docs/section-refs`, and `docs/project-layout` of `scripts/check-docs.sh`, `scripts/sensor-plain-rules.sh`, and `scripts/lib/rules.sh`

## Context

Several coding agents may work in the same repository, and each looks for its instructions in a
different place. Without one written governance their behaviour drifts, rule sets are duplicated
and diverge, and structural decisions are made silently.

Three document roles follow from that: what the project is *for*, what has been *decided*, and
what the working *rules* are. Three more are easy to overlook.

- The record is a log. Nothing in it says what the system looks like **now**. Reconstructing that
  from the log costs more as it grows, and an agent starting without memory pays it every session.
- The vocabulary changes more often than any other part of the specification. A term is sharpened
  in almost every design conversation, which sits badly in the document meant to change least.
- An older project has conventions on how it writes code and tests where no check decides it. The
  rule file is the template's, the specification must not churn, and an ADR is for decisions. So a
  convention became an ADR it did not deserve, or stayed in someone's head, and a reviewer held
  the diff against nothing.

Two forces bound the answer. Every further document is a candidate second source of truth that
can go stale. And the specification outranks every ADR while nothing protects it: editing a goal
would be the cheapest route around an accepted decision, and the ADRs resting on the old clause
would keep their force.

A rule restated in a second document drifts unseen. A link to the rule's section breaks visibly
when the section moves, and a check catches the broken link.

## Decision

We will keep what a repository knows in six fixed documents with one role each, and its working
rules in one rule file.

1. **The set.** Every repository must keep these six documents, each in its one role:
   - [`docs/SPECIFICATION.md`](../SPECIFICATION.md), the specification: what the project is for;
   - [`docs/adr/`](.), the record: the decisions, kept as
     [NUC-decisions-are-proposed-then-accepted](NUC-decisions-are-proposed-then-accepted.md),
     [NUC-adrs-are-named-by-category](NUC-adrs-are-named-by-category.md), and
     [NUC-base-is-fixed](NUC-base-is-fixed.md) decide;
   - [`AGENTS.md`](../../AGENTS.md), the rule file: the working rules;
   - [`docs/ARCHITECTURE.md`](../ARCHITECTURE.md), the overview: the system as it stands;
   - [`docs/GLOSSARY.md`](../GLOSSARY.md), the glossary: the vocabulary;
   - [`docs/CONVENTIONS.md`](../CONVENTIONS.md), the conventions: the judgement calls on code and
     tests.

   No other document may carry a rule. A project may drop only the further documents of clause 7.
2. **Specification.** A change to the specification is a decision, not an edit.
   - Only a human may land the change. An agent may draft it but must not judge alone that no
     ADR is affected.
   - The change must name every accepted ADR whose basis it moves. A passage no ADR derives from
     names none.
   - The human reconsiders each named ADR in the same review and supersedes each it invalidates.
   - A project must narrow a `NUC-` clause the change invalidates by an ADR of its own, or fork
     the template ([NUC-base-is-fixed §1](NUC-base-is-fixed.md)).
   - The template repository corrects its own `NUC-` ADR in place instead
     ([NUC-decisions-are-proposed-then-accepted §6](NUC-decisions-are-proposed-then-accepted.md)).
3. **Rule file.** A repository must have exactly one rule file. It is vendor-neutral and the one
   source of working rules.
   - Another document must cite a working rule by a link to its section anchor and must not
     restate it.
   - A working rule that the rule file neither states nor links is not a rule of the project.
4. **Overview.** The overview must hold no rule and no decision. It must cite the ADR behind each
   structural choice. A change to the structure must update it in the same change. Where it and an
   accepted ADR disagree, the ADR is right.
5. **Glossary.** The glossary defines the domain's terms and decides nothing.
   - An entry names one term in bold, says in one or two sentences what it is, and lists under
     _Avoid:_ the synonyms it displaces.
   - Whoever settles a term, an agent included, must add it in the same change.
   - Code, comments, and documents must use its terms. A missing entry is a gap to close there,
     not a word to invent.
   - The specification must define no term of its own.
6. **Conventions.** An entry of the conventions says in a sentence or two how the project writes
   code or tests where no check decides it, and why.
   - Whoever settles a convention, an agent included, must add it in the same change.
   - A convention that a check is able to decide must become the check.
   - A convention that constrains future choices must become an ADR.
   - The reviewer must receive the conventions. The implementer reads them when the work needs
     them.
7. **Further documents.** Every other document must point at the rule file and hold no rule.
   Examples are `README.md`, `CONTRIBUTING.md`, the per-agent pointer files, and the catalogue of
   `propose-adr`.
8. **Mechanics.** A document may hold the mechanics that the rule file or a `NUC-` ADR delegates
   to it. Examples are [`docs/adr/README.md`](README.md), [`template.md`](template.md), and
   sections of [`CONTRIBUTING.md`](../../CONTRIBUTING.md). Mechanics say how a rule is carried
   out and must add no rule. `README.md` must hold no mechanics.

**Out of scope:**

- what the rule file says, beyond clause 3;
- what enforces a rule, and what a per-agent configuration may do, which
  [NUC-main-only-via-pr §7](NUC-main-only-via-pr.md) decides;
- the order of work of a setup, which the setup file and its skill carry.

## Latitude

A project may choose:

- which agents work in it and which of them need a pointer file, from the entry `harness` of the
  [catalogue](../../.agents/skills/propose-adr/concepts/README.md);
- the notation and depth of the overview; the catalogue has no entry, and the choice needs no ADR;
- a context map and one vocabulary per bounded context, for a repository with more than one; the
  catalogue has no entry, and the choice needs no ADR;
- which further documents of clause 7 it keeps, while it keeps all six of clause 1; the catalogue
  has no entry, and the choice needs no ADR.

## Alternatives considered

- **Per-tool instruction files.** They guarantee drift between agents and multiply maintenance.
- **Rules embedded in `README.md`.** They mix the onboarding of users with agent governance.
- **No overview; the record is the current state.** It holds while the log is short and degrades
  silently as it grows.
- **Vocabulary inside the specification.** The right authority for a goal is the wrong authority
  for a word. Every renamed term became a specification change only a human lands, so the
  vocabulary in the tree lagged the vocabulary in use.
- **The full ADR lifecycle for the specification.** Immutable clauses would demand a supersession
  for a wording fix. The specification is living intent, not a record.
- **A CI check requiring an ADR reference on every specification change.** It cannot tell a typo
  fix from a rewritten goal. So it blocks ordinary edits or is satisfied by a token reference.
- **Conventions in the rule file.** The file is the template's. Project conventions in it collide
  with every template release and reach every session, whether it writes code or not.
- **Conventions as ADRs.** The wrong lifecycle: a convention is refined every few weeks, an ADR is
  superseded. The record fills with decisions nobody would take for one.
- **Conventions only as checks.** That is the mechanical half. Naming, what a module may know, and
  what a test asserts are judgement calls no linter decides, and a reviewer needs them written
  down most.

## Sources / Prior art

- The `AGENTS.md` convention — <https://agents.md>: one open file for coding agents, the format
  the rule file takes.
- Michael Nygard, *Documenting Architecture Decisions* —
  <https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions>: the ADR form, and
  that a record is immutable and superseded.
- GitHub Spec Kit — <https://github.com/github/spec-kit>: a "constitution" written once per
  project that every feature derives from; the specification's role here.
- Matt Pocock, *domain-modeling* skill —
  <https://github.com/mattpocock/skills/blob/main/skills/engineering/domain-modeling/SKILL.md>: a
  glossary "and nothing else", devoid of implementation, next to `docs/adr/`; the model for the
  glossary. *A complete guide to AGENTS.md* on the same author's site —
  <https://www.aihero.dev/a-complete-guide-to-agents-md>: "Every token in your AGENTS.md file
  gets loaded on every single request", so "be ruthless about what goes here".
- Addy Osmani, *AGENTS.md* — <https://addyosmani.com/blog/agents-md/>: every line traceable to
  something that went wrong.
- Birgitta Böckeler, *Context engineering for coding agents* —
  <https://martinfowler.com/articles/exploring-gen-ai/context-engineering-coding-agents.html>:
  build rules files up gradually; why conventions live in a document with a retro rather than in
  the rule file.
- Jarosław Wąsowski, *Designing a spec that survives code generation* —
  <https://medium.com/@wasowski.jarek/sdd-designing-a-spec-that-survives-code-generation-spec-first-spec-driven-development-b61fdc234493>:
  specification types with different lifecycles, and that maintaining everything is the wrong
  answer; read in abstract only, the article is paywalled.

## Consequences

- Positive: one place to change a rule, and consistent agent behaviour.
- Positive: one place answers what the system looks like.
- Positive: the session that changes the vocabulary maintains it.
- Positive: the specification stops being a way around an accepted ADR.
- Positive: a convention has a place that is neither an ADR nor a head, and the reviewer holds the
  diff against something written.
- Negative: nothing mechanical keeps another document from stating a rule of its own. The
  documents have to stay small enough for a review to see it.
- Negative: a stale overview is worse than none because it is believed, and no check tells the two
  apart.
- Negative: naming the ADRs a specification change moves is a judgement no check makes.
- Negative: the conventions attract entries a check could decide, and only the design revision
  moves them out.
- Follow-ups: a context map for repositories with more than one bounded context.

## Enforcement

[`scripts/check-docs.sh`](../../scripts/check-docs.sh) holds clause 3, and its self-test cites
this ADR. Its rule `docs/links` fails on a relative link that does not resolve. Its rule
`docs/section-refs` fails on a section reference that names no section of the rule file.

[`scripts/sensor-plain-rules.sh`](../../scripts/sensor-plain-rules.sh) holds clause 5 and the
writing rule of the rule file ([`AGENTS.md` §4](../../AGENTS.md#4-working-style)), and its
self-test cites this ADR. It reads the lines a change adds to the *Decision* and *Enforcement* of
an ADR and to `AGENTS.md`, a renamed file by the lines its rename changes:

- Without a flag, it fails nothing. It reports each soft modal verb, *should*, *could*, *might*,
  or *ideally*, and each word an _Avoid:_ list names. It reports each sentence over 25 words and
  each item of an _Avoid:_ list it cannot read.
- With `--require`, it is the check of this clause and that rule. It fails on a soft modal verb
  and on a word an _Avoid:_ list names, on such a line of a project ADR. On a line of a `NUC-` ADR
  or of `AGENTS.md`, which a take-up replaces as the release has them, it only reports them.
- `scripts/check-all.sh` and the `checks.yml` workflow run it with `--require`.

The rule `docs/project-layout` of `scripts/check-docs.sh` holds the Project Layout of
`CONTRIBUTING.md` against the tree (clause 8).

[`.github/CODEOWNERS`](../../.github/CODEOWNERS) routes a specification change to the owner's
review once the repository requires it (clause 2).

**Not mechanically decidable:** the review judges these:

- whether another document holds a rule (clauses 1, 7, and 8);
- whether a specification change names every ADR it moves (clause 2);
- whether the overview matches the system (clause 4);
- whether the glossary's terms are the ones in use (clause 5);
- whether a convention belongs in a check (clause 6).
