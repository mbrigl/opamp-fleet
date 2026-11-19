# NUC-adrs-are-named-by-category: An ADR is named by a category key and a slug, cited by its clauses, and indexed by category

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** every ADR under `docs/adr/`, the ADR index `docs/adr/README.md`, the ADR template `docs/adr/template.md`, checks 1, 2, 5, 6, 7, and 8 of `scripts/check-docs.sh`, and the ADR readers of `scripts/lib/adr.sh`

## Context

An ADR was cited by a number. A number names no topic, so a reader of a commit, an issue, or a
code comment opens the file to learn what was meant. A draft took its number only on the way to
`main`, through a numbering script, a check against drafts on `main`, and a rule for two branches
that took the same number. A normalization renumbered the whole record, and every citation outside
the tree went stale.

Several forces bound the answer. Every agent reads the same files, so the identifier has to work
in plain text, file names, and Markdown links. The checks are bash and must keep working on
macOS, Linux, and Windows under Git Bash. A derived project needs room for its own categories.

## Decision

We will name each ADR by a category key and a slug, cite it by its numbered clauses, and index it
by category.

1. **Identifier.** An identifier is a category key, a hyphen, and a slug, such as
   `SEC-host-socket-closed`.
   - The key is two to five capital letters. The slug is two to five lower-case words or numbers
     joined by hyphens, and it states the decision.
   - The file is `docs/adr/<identifier>.md`, headed `# <identifier>:` and the title.
   - The author picks the identifier when creating the file. No ADR has a draft state, and no step
     numbers ADRs.
   - The identifier must not change once the ADR is on `main`, even when its title changes.
   - No identifier may be used twice, deleted ADRs included. A successor takes a new slug.
   - The date in the header orders the ADRs.
2. **Categories.** The fixed categories are these:
   - `NUC`: the template's own ADRs, reserved to the template repository;
   - `SEC`: security;
   - `OPS`: build, release, deployment, and operation;
   - `API`: interfaces the system offers and consumes, adapters among them.

   A project must define every further category, with one line on what it holds, in the category
   list of the ADR index.
3. **Clauses.** The *Decision* of an ADR states its rules as numbered clauses. A citation of a
   clause adds a section sign and the number, such as `SEC-host-socket-closed §3`. A clause keeps
   its number, and a removed clause leaves its number unused.
4. **Supersession.** A project ADR that replaces others names each in its `Supersedes:` field
   from its proposal on. One ADR may supersede several whose decisions have grown into one. When
   a human accepts it, each old ADR's status flips to superseded by the new identifier, in the
   same change. A superseded ADR keeps its file and body until a normalization.
5. **Index.** The ADR index lists the category list and the project's ADRs by category. It links
   the `NUC-` base in one line and lists no `NUC-` ADR.
   - Each row shows the status by the legend's emoji and mirrors the ADR's `Applies to` verbatim.
   - Accepted and proposed ADRs stand under **Binding**, superseded and rejected ones under
     **Superseded and rejected**.
   - A row changes in the same commit as its ADR.
6. **Citations.** Every citation of an ADR must name a file in `docs/adr/`, and in Markdown it
   links that file. An anticipated follow-up is described by its topic. An identifier in a
   Markdown code span is an example or a plan, not a citation.

**Out of scope:**

- when an ADR is due and how it is accepted
  ([NUC-decisions-are-proposed-then-accepted](NUC-decisions-are-proposed-then-accepted.md));
- the fixed base ([NUC-base-is-fixed](NUC-base-is-fixed.md));
- which test cites a decision ([NUC-decisions-cite-tests](NUC-decisions-cite-tests.md)).

## Latitude

A project may choose further categories, defined in the category list of its index.

## Alternatives considered

- **Sequential numbers, as before.** They are short and sort by age. They name no topic, need a
  draft state and a numbering step, collide between parallel branches, and change at every
  normalization.
- **A category with a number, such as `SEC-0003`.** It names the area. Parallel branches still
  collide on the counter, and the number still says nothing within the category.
- **A date or a random identifier.** It never collides. Nobody can read or remember it, and a
  citation in prose means nothing without the file.
- **The slug alone, without a category.** It reads well. The index loses its grouping, and
  nothing marks the template's ADRs apart from the project's.

## Sources / Prior art

- Michael Nygard, "Documenting Architecture Decisions" (2011): records numbered in sequence.
- MADR, the Markdown Architectural Decision Records template: file names of a number and a title.
- adr-tools by Nat Pryce: a numbering script and supersession links between numbered records.

## Consequences

- Positive: a citation names its topic, in a commit, an issue, or a code comment.
- Positive: no numbering step, no draft check, and no collision between parallel branches.
- Negative: an author has to name the decision in five words when creating the file.

## Enforcement

`scripts/check-docs.sh` holds these clauses, and its self-test cites this ADR:

- each identifier's form, file, heading, category, and uniqueness across the history (clauses 1
  and 2);
- the supersessions, the index rows, and the citations against the ADRs (clauses 4 to 6).

**Not mechanically decidable:** the review judges whether a slug states the decision (clause 1).
