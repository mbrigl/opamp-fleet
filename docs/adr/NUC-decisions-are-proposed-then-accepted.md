# NUC-decisions-are-proposed-then-accepted: A decision is proposed as an ADR before it is implemented, a human accepts it, and only a supersession or a normalization changes it

- **Status:** 🟢 accepted
- **Date:** 2026-11-08
- **Deciders:** NUC maintainer
- **Applies to:** every ADR under `docs/adr/`, `scripts/sensor-review-binding.sh`, `scripts/sensor-binding-adrs.sh`, `scripts/test-normalize-remap.sh`, the map script `.agents/skills/normalize/remap.sh`, and the skills `propose-adr` and `normalize`

## Context

An agent decides quickly and in passing. A decision taken inside an implementation is visible
only in the code, without its reasons, and the next session takes it for an accident or repeats
the debate. A human who first sees the decision in a finished change can no longer weigh it apart
from the work built on it.

The rules of the record stood in [`AGENTS.md` §3](../../AGENTS.md#3-adr-rules) and in the
mechanics of the index. The binding sensor routes a change only to ADRs, so a change to the record
reached these rules only through the rule file, and a project could not narrow them in its record.

Several forces bound the answer. An ADR that nobody may change goes stale, and one that anybody
may edit decides nothing. Every agent reads the same files, so the steps have to show in commits
and their messages, not in the state of a tool.

## Decision

We will propose each architecture decision as an ADR before implementing it, and only a human
accepts it.

1. **When.** An agent must create an ADR before an architecture-relevant decision.
   - Such a decision picks a dependency or framework, a public interface, a persistence or
     synchronization strategy, or a protocol or data format. It is anything that constrains
     future technology choices.
   - One ADR records one decision, or an integration whose decision is the interplay.
   - When the catalogue of `propose-adr` has an entry for a decision no ADR in force decides, the
     agent must propose the entry's fitting concepts first. The proposal must include the
     simplest of them.
2. **Calibration.** An ADR records a decision that is costly to reverse or constrains future
   choices. It also records one a reader without the conversation takes for a mistake, however
   cheap to reverse.
   - Implementing within an ADR in force needs no ADR. Nor do a bug fix, an interface-preserving
     refactoring, a test, documentation, formatting, or development-only tooling.
   - An agent that is unsure must write a short proposed ADR instead of deciding silently.
3. **Proposal.** An agent must submit an ADR as proposed, in commits of ADRs only, and hand them
   to the human before anything is implemented against them.
   - The commits go on the work's branch or on a branch of their own.
   - ADRs that one human decides may travel together.
   - An ADR the human sends back must move to a branch of its own. Nothing may be implemented
     against it until it is settled.
4. **Implementation.** After the human's review, the agent may implement while the ADR is
   proposed, after its commits, on the same branch or a stacked one.
   - A commit that changes the text of an ADR must change nothing outside `docs/adr/`.
   - A normalization may carry the restated ADRs together with the text that cites them.
   - The template repository's correction of its own `NUC-` ADRs may carry the corrected ADRs
     together with the text that cites them.
   - Findings return as revisions, each a commit of ADRs only.
   - The hand-off must name the commit of ADRs that the human reviewed.
5. **Status.** Only a human may change the status of an ADR. The flip to accepted changes the
   header line only and may land with the implementation. An accepted ADR binds everyone, and
   nobody may violate or work around it.
6. **Immutability.** Outside a normalization, nobody may rewrite or delete an accepted project
   ADR. A project changes one only by superseding it
   ([NUC-adrs-are-named-by-category §4](NUC-adrs-are-named-by-category.md)). The template
   repository corrects its own `NUC-` ADRs in place, and a release ships the correction.
   - A take-up of a release may move each citation of a `NUC-` clause in an accepted project ADR,
     as the map of the release says.
   - When the cited clause changes its meaning, the project must supersede its ADR instead.
7. **Authority.** Authority runs from the specification to the accepted ADRs to the task. When the
   specification and an ADR conflict, the specification wins. An agent must raise the conflict
   instead of choosing silently.
8. **Normalization.** A normalization restates the record, only on a human's instruction.
   - It restates the specification, then supersedes the ADRs that invalidates, then rewrites the
     project ADRs in force as a new set.
   - It changes no decision, and it deletes superseded and rejected ADRs.
   - A merged or split ADR takes a new identifier. The others keep theirs.
   - A map moves the references to the new identifiers. It stands in the hand-off and the commit
     body, in no document.
   - It leaves the `NUC-` files as they are.

**Out of scope:**

- how an ADR is named, cited, superseded, and indexed
  ([NUC-adrs-are-named-by-category](NUC-adrs-are-named-by-category.md));
- the fixed base ([NUC-base-is-fixed](NUC-base-is-fixed.md));
- the glossary and the change of the specification
  ([NUC-documents-are-fixed §5](NUC-documents-are-fixed.md) and
  [NUC-documents-are-fixed §2](NUC-documents-are-fixed.md));
- the order of work of a proposal or a normalization (the skills `propose-adr` and `normalize`).

## Latitude

None.

## Alternatives considered

- **The decision recorded after the implementation.** It costs the human no early review. The
  human then weighs a decision against the work already built on it, and a rejection discards
  that work.
- **Accepted ADRs edited in place.** The record stays short and current. A reader cannot tell
  what was decided when, and a change of a decision passes without a human's acceptance.
- **The agent accepts its own ADR.** It saves a round trip. Nobody outside the conversation ever
  weighed the decision.

## Sources / Prior art

- Michael Nygard, "Documenting Architecture Decisions" (2011): the short ADR with context,
  decision, status, and consequences, and supersession instead of an edit.
- MADR, the Markdown Architectural Decision Records template: the status field and its values.

## Consequences

- Positive: the human weighs each decision before work is built on it.
- Positive: the binding sensor routes a change to the rules of the record like to any other ADR.
- Negative: an implementation waits for the review of its ADR.
- Negative: every derived project migrates its record once, with a map the human reviews.

## Enforcement

`scripts/test-normalize-remap.sh` cites this ADR. It holds that a map moves every reference
exactly once (clause 8). The map script refuses a chain or a cycle among ADR identifiers, so a
second run changes nothing ([NUC-adrs-are-named-by-category §1](NUC-adrs-are-named-by-category.md)).
A map of criteria may swap them in one pass.

These scripts fail nothing:

- `scripts/sensor-review-binding.sh` lists each commit that changes the text of an ADR and a path
  outside `docs/adr/` (clause 4).
- `scripts/sensor-binding-adrs.sh` lists the ADRs in force whose *Applies to* names a path the
  change touches or the session names (clause 5). Where an *Applies to* names checks of a file by
  number, it lists that ADR only for a change inside one of those checks. A change it cannot
  attribute to a check lists every ADR that names the file. With `--sections` it prints the parts a
  session reads. Its self-test cites this ADR.

**Not mechanically decidable:** the review judges these (clauses 1 to 4, 8):

- whether a decision needs an ADR;
- whether an ADR holds one decision;
- whether the human reviewed the proposal before the implementation;
- whether a normalization changed no decision.
