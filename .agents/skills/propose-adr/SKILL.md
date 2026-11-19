---
name: propose-adr
description: Develop and submit an Architecture Decision Record, with a catalogue of concepts to propose from. Use when a change is architecture-relevant, costly to reverse, constrains future choices, or would look like a mistake to a reader without this conversation, when it takes an early decision no ADR decides, such as the shape of the code, the environment, or the supply chain, or when the human asks for an ADR.
---

# Propose an ADR

The rules are
[NUC-decisions-are-proposed-then-accepted](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md),
[NUC-adrs-are-named-by-category](../../../docs/adr/NUC-adrs-are-named-by-category.md), and, for
the `Extends`, `Narrows`, and `Role` fields,
[NUC-base-is-fixed](../../../docs/adr/NUC-base-is-fixed.md); the mechanics of the record are in
[`docs/adr/README.md`](../../../docs/adr/README.md). This skill is the order of work. It states no
rule of its own ([NUC-procedures-are-skills §3](../../../docs/adr/NUC-procedures-are-skills.md)).

## 1. Calibrate

Decide with
[NUC-decisions-are-proposed-then-accepted §2](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)
whether this needs an ADR at all. The rule's third case, what a reader without this conversation
would take for a mistake, is the deliberate deviation from the obvious path; record it, or the next
session "fixes" it. When no ADR is needed, say so and stop here.

## 2. Interrogate the request

Before writing a line of the record, ask the human, in rounds until the answers stop changing the
decision, at least:

- Which goal or quality goal of [`docs/SPECIFICATION.md`](../../../docs/SPECIFICATION.md) does
  this serve, by identifier (`G-n`, `Q-n`)?
- What did the human not say that the decision depends on?
- Which alternative has nobody mentioned yet?
- What happens if no decision is made?
- Does this bind future choices, or is it a *how* that needs no ADR?
- Would someone who lacks this conversation see the result and try to fix it?

Question the framing and your own draft. A draft written ahead of the answers is the first
alternative to reject.

When the catalogue has an entry for the decision, read it in
[`concepts/README.md`](concepts/README.md) before the first round and put its candidates to the
human: the two or three whose *Fits when* matches the quality goals, what each one's *Does not
fit* says about this project, and the simplest form as one of them. Between candidates that fit
equally, prefer the one whose rule a test or check can decide: what a computational sensor holds
needs no reviewer to remember it. The human chooses; a candidate nobody chose is an alternative
in the record.

## 3. Write the record

1. Copy [`docs/adr/template.md`](../../../docs/adr/template.md) to `<CAT>-<slug>.md` and head it
   `# <CAT>-<slug>: <title>`. The identifier is picked now and stays
   ([mechanics](../../../docs/adr/README.md#mechanics)). Complete the header and delete the
   instruction paragraph.
2. One decision per ADR: one "We will …" sentence. A second one is a second ADR, unless the
   decision is the interplay of an integration
   ([NUC-decisions-are-proposed-then-accepted §1](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)).
3. **Alternatives:** every one the interrogation surfaced, each with why not. For a concept of
   the catalogue, its entry's sections carry over: *Fits when* and *Does not fit* into the
   Context, *Alternatives* and *Sources* as they stand, checked against this project, and *The
   rule a test decides* into the Enforcement.
4. **Sources:** what was consulted. A decision costly to reverse is researched first, external
   sources included, and nothing recalled is passed off as verified
   ([§4](../../../AGENTS.md#4-working-style)).
5. **Consequences:** weighed against the specification's quality goals. Follow-ups by topic,
   never by an identifier that does not exist yet.
6. **Enforcement:** the test that will cite this ADR with a `Verifies:` marker, or a paragraph
   starting `**Not mechanically decidable:**` with the reason
   ([NUC-decisions-cite-tests §4](../../../docs/adr/NUC-decisions-cite-tests.md)).
7. Add the index row in [`docs/adr/README.md`](../../../docs/adr/README.md), status and *Applies
   to* mirrored verbatim. For a supersession, name the old ADR in `Supersedes` and leave its
   status line: the human flips it when accepting this one
   ([mechanics](../../../docs/adr/README.md#mechanics)).
8. Run [`scripts/check-docs.sh`](../../../scripts/check-docs.sh).

## 4. Submit

Commit on a branch and hand the ADR to the human as
[NUC-decisions-are-proposed-then-accepted §3](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md)
asks. What follows is
[NUC-decisions-are-proposed-then-accepted §4](../../../docs/adr/NUC-decisions-are-proposed-then-accepted.md).
