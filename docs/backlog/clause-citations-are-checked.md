# A clause citation names a clause of an ADR in force

**Kind:** idea, check.

**Observed.** `docs/CONVENTIONS.md` asks that code cite an ADR by its clause (`ADR-0021 clause
25`), never by a place in a document. A convention a check can decide is the check, not an entry.
A number beyond an ADR's clauses, or a clause of a superseded ADR, is a stale citation nobody sees
today; a normalization remaps them by a map, and nothing verifies the result. `main` has no such
check.

**What the old branch had.** Revision ticket 23 of `feat/revision-2026-10-31`, open, not worked:
extend `scripts/check-docs.sh` or `scripts/check-traceability.sh` so every `ADR-NNNN clause N` in
a tracked file names a numbered clause of the Decision of an ADR in force, and a citation by
"point", paragraph or line fails; then delete the convention entry in the same change.
