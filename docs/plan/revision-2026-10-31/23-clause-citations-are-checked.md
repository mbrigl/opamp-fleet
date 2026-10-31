# Ticket: A citation of an ADR clause in code is checked, and its convention entry goes

## What to build

`docs/CONVENTIONS.md` asks that code cite an ADR by its clause (`ADR-0037 clause 25`), never by a
place in a document. The tree holds 1604 such citations and none that cites a place, and every
clause number cited lies within its ADR's numbered clauses. Verified with a scan of every `ADR-NNNN
clause N` under `crates/`, `scripts/`, `packaging/`, `fuzz/` and `.github/` against the numbered
clauses of each ADR's Decision. A convention a check can decide is the check, not an entry; and a
number beyond an ADR's clauses, or a clause of a superseded ADR, is a stale citation nobody sees
today.

Extend `check-docs.sh` (or the traceability check) so every `ADR-NNNN clause N` in a tracked file
names a numbered clause of the Decision of an ADR in force, and so a citation by "point", paragraph
or line fails; then delete the convention entry in the same change.

## Acceptance criteria

- [ ] Given a citation of a clause number beyond an ADR's last clause, when the check runs, then it
  fails and names the file and the line.
- [ ] Given a citation of a clause of a superseded ADR, when the check runs, then it fails.
- [ ] Given the current tree, when the check runs, then it passes, and the convention entry is gone.

## Read first

- `scripts/check-docs.sh` checks 5 and 6, `scripts/check-traceability.sh`.
- `docs/CONVENTIONS.md`, the entry "Code cites a clause, never a place in a document".

## Blocked by

- None (can start immediately)

## Mode

with the human — ADR-0007 clause 5: nothing is worked unattended, so every part of the system is
kept with the human.
