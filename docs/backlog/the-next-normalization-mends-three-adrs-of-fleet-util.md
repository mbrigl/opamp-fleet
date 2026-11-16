# The next normalization mends three ADRs of fleet-util

**Kind:** record; for the next normalization.

**Observed.** Three ADRs accepted with PR #193 carry blemishes no decision depends on, and an
accepted ADR changes only by a normalization or by superseding it
([`AGENTS.md` §3](../../AGENTS.md#3-adr-rules), rule 5):

- ADR-0041 counts the crates the operator tool links, 282 before and 226 after (its Context and
  Consequences), but does not say how: they are distinct crate names,
  `cargo tree -p fleet-tools -e normal --prefix none | awk '{print $1}' | sort -u | wc -l`;
  without `awk` the same command prints 571 lines, and 300 after `sort -u`.
- ADR-0040 keeps lines a reflow broke mid-sentence: "which changes / ADR-0041 clause 3, supersedes
  ADR-0041," in its *Out of scope* (near line 87) and "which changes / ADR-0041 clause 3 and so
  supersedes / ADR-0041" in its Follow-ups (near line 134).
- ADR-0039 names `crates/fleet-util/src/archive.rs` as plain code near its line 1556, where its
  predecessor, ADR-0033, linked the file (its line 1553); near line 1209 both write it as code.

**Idea.** The next normalization restates the three with the count method stated, the lines
rewrapped, and the file linked; until then they stand as they are.

**From.** The review of PR #193 (2026-11-15/16), lesser findings the human asked to act on on
2026-11-16.
