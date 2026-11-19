# <CAT>-<slug>: <short title of the decision>

- **Status:** 🟡 proposed
- **Date:** YYYY-MM-DD
- **Deciders:** <names / roles of the humans who decide>
- **Applies to:** <the paths, components, or kinds of change this decision constrains>
- **Supersedes:** [<CAT>-<slug>](<CAT>-<slug>.md)
- **Extends:** NUC-<slug> §n
- **Narrows:** NUC-<slug> §n
- **Role:** harness | toolchain

*Complete the header and delete what does not apply. `Status` ships as `🟡 proposed`; a human flips
it to `🟢 accepted`, `🔴 rejected`, or `⚪ superseded by <CAT>-<slug>`, keeping exactly one emoji with
the word that belongs to it. `Date` is the date of the last status change: the date the ADR was
proposed until the first flip. Revising a `proposed` ADR does not move it and needs no note; git
carries the history. `Deciders` names the humans who accept or reject; an agent is never a
decider. `Applies to` names what the decision constrains, concrete paths where it has them,
otherwise the kind of change (every commit, every public interface), in one line without a pipe
character: the index mirrors it verbatim, and that is how a reader finds the ADRs that bind a
change ([mechanics](README.md#mechanics)). `Supersedes` only when this ADR replaces one existing
ADR or several whose decisions have grown into one, each linked; their status lines flip when a
human accepts this one ([mechanics](README.md#mechanics)). `Extends` or `Narrows` only when this
ADR extends or narrows a clause of a `NUC-` ADR, which no project ADR supersedes
([NUC-base-is-fixed §1](NUC-base-is-fixed.md)); `Role` only when it fills a
required role ([NUC-base-is-fixed §3](NUC-base-is-fixed.md)).
For a decision recorded only because a
reader would take it for a mistake, cheap to reverse and constraining nothing
([NUC-decisions-are-proposed-then-accepted §2](NUC-decisions-are-proposed-then-accepted.md)), a
sentence each under Context and Decision and `None — cheap to reverse` under Alternatives, Sources,
and Consequences is the whole record; Enforcement stays. Then delete this paragraph.*

## Context

What is the issue, and which forces are at play: requirements from
[`docs/SPECIFICATION.md`](../SPECIFICATION.md), constraints from other ADRs, technical forces?

Interrogate the request with the human before writing this section
([NUC-decisions-are-proposed-then-accepted §3](NUC-decisions-are-proposed-then-accepted.md)); the
`propose-adr` skill under [`.agents/skills/`](../../.agents/skills/) lists the questions.

## Decision

What we decided, in one clear, active sentence ("We will …"). A second "We will …" sentence is a
sign of two ADRs
([NUC-decisions-are-proposed-then-accepted §1](NUC-decisions-are-proposed-then-accepted.md)).
Everything that binds stands here or under *Enforcement*. A session whose change only follows the
ADR reads these two and the header ([`AGENTS.md` §2](../../AGENTS.md#2-start-here)).

**Out of scope:** what this decision deliberately leaves open, so a later ADR can take it up
without contradicting this one. Write "nothing beyond the sentence above" when the decision is
already narrow.

## Latitude

The choices this decision leaves to a project, one per item. A choice links its entry of the
[catalogue](../../.agents/skills/propose-adr/concepts/README.md) where one exists. A choice with no
entry says that it needs no ADR. Name no tool and no project value. Write "None." when the
decision leaves no choice.

## Alternatives considered

- **<Alternative A>** — why it was not chosen.
- **<Alternative B>** — why it was not chosen.

## Sources / Prior art

State of the art and established solutions consulted before deciding: links, docs, papers,
comparable systems. [`AGENTS.md` §4](../../AGENTS.md#4-working-style) requires citing what
informed the decision. Write "None — trivial/reversible" only when that is genuinely true.

## Consequences

- Positive: what becomes easier or possible.
- Negative / trade-offs: what becomes harder, what we accept.
- Follow-ups: the questions this decision raises, described by topic, never by an identifier that
  does not exist yet.

## Enforcement

What keeps this decision true and where it lives: the check, test, CI job, or review step that
fails when it is violated. Name the test that cites this ADR, or write a paragraph starting
`**Not mechanically decidable:**` with the reason no test can decide it.
Accepting an ADR that has neither fails CI
([NUC-decisions-cite-tests §3](NUC-decisions-cite-tests.md)).
