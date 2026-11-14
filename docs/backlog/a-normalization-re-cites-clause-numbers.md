# A normalization re-cites the clause numbers it moves

**Kind:** defect, process.

**Observed.** `docs/CONVENTIONS.md` asks a comment that relies on a decision to name its clause,
so that a normalization's map can move it, and check 18 of `scripts/check-docs.sh` gives the
same reason. The map does not move a clause number. `.agents/skills/normalize/remap.sh` rewrites
`ADR-NNNN`, an ADR's file name and a criterion identifier, never the number after `clause`. The
`normalize` skill's clause table (its section 5, step 2) records each old clause with the new ADR
and the section that carry it, not the new clause number, and no step re-cites the citations. So
after a normalization `ADR-0021 clause 25` reads `ADR-000X clause 25`, with the old clause number
under the new ADR. Check 18 catches it only where that number lies beyond the new Decision's
clauses; elsewhere it names a clause that says something else.

**Idea.** The clause table names the new clause number of each old clause. The normalization
re-cites every `ADR-OLD clause N` by it, by a second map that `remap.sh` applies, so a citation
moves with its clause as the ADR identifier does. Check 18 then holds the result to the new
record. Without that step, the convention and check 18's header stop claiming that the map moves
clause citations, and name the clause table instead.

**From.** The third review of check 18 on `feat/clause-citations-are-checked` (2026-11-14).
