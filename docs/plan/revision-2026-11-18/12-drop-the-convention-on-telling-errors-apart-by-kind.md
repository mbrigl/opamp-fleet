# Ticket: Drop the convention on telling errors apart by kind, or keep it

## What to build

The design revision drops a convention that prevented nothing during the work in scope
([`AGENTS.md` §5](../../../AGENTS.md#5-quality-bar--definition-of-done)).
"A caller tells errors apart by their kind, never by their text" in `docs/CONVENTIONS.md`
came with the revision of 2026-11-15, from a CSR whose two proofs were told apart by the text
of an error. No change since then wrote a caller that matches on an error, and no review cited
it; no ADR or criterion cites it.

The revision doubts the drop: one revision's range is three days old, and the entry's situation
did not arise, which is not the same as having been tried and failed. The human decides; on a
drop, this ticket's change deletes the entry.

## Acceptance criteria

- [ ] Given the human's decision, when this ticket closes, then the entry is deleted, or the
  closing commit says why it stays.

## Read first

- Touches: `docs/CONVENTIONS.md`

## Blocked by

- None (can start immediately)

## Mode

with the human: the human decides which rule is dropped.
