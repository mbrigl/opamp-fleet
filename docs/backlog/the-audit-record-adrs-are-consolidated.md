# The audit record's ADRs are consolidated

**Kind:** ADR text.

**Observed.** Four accepted ADRs decide the audit record together: ADR-0028 (an append-only record
whose failed writes on PostgreSQL are kept and tried again), ADR-0029 (one hash chain per lease,
naming the lease each decision was taken under), ADR-0041 (the heads of the chains signed with a
key on the token) and ADR-0043 clause 9 (what the export drops and counts). Each is true on its
own, and the set reads as one decision: what the record holds, how it is chained, signed and
exported. A reader has to assemble it from four records.

**Idea.** One superseding ADR that restates the four as one decision, through `propose-adr`, or
the next normalization merging them into one new ADR. Not drafted during the freeze of
2026-11-12.

**From.** The design revision of 2026-11-12 (step 4) and the review of its branch.
