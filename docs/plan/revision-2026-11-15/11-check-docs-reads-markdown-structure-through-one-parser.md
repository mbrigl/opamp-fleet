# Ticket: `check-docs.sh` reads an ADR's sections and a heading through one parser each

## What to build

`scripts/lib/adr.sh` provides `ADR_FENCE_AWK` and `adr_section`, so that every reader of an
ADR's sections ends a section at the same line. `sensor-binding-adrs.sh` and `lib/rules.sh` use
them. A sensor that read sections its own way stopped at a `####` heading and never listed
ADR-0046 (#174, 26d9d74b3). `scripts/check-docs.sh` still parses the same structures several
ways:

- **Check 18** counts the clauses of a Decision with a private awk (near line 1422) that ignores
  fences: an `N. ` line inside fenced code counts as a clause, and a `## ` line there ends the
  section.
- **Headings.** `heading_texts` (check 17, near line 1325) and `extract_heading_slugs` (the
  anchor check, near line 605) parse ATX headings with the same code, and only the first skips
  fenced code. `scan_markdown` (near line 672) holds two more fence models. None uses
  `ADR_FENCE_AWK`.
- **Line joining.** Checks 17 and 18 each carry the same `scan`/`refs`/`cites` driver (near lines
  1373 and 1505) with the same comment-marker regex and the same `prev`/`FNR` logic, so a fix
  to one (the known three-line wrap of check 18) leaves the other behind.

Check 18 counts through `adr_section Decision` (or prepends `ADR_FENCE_AWK`). The heading parsers
share one function that skips fences as `ADR_FENCE_AWK` does. Checks 17 and 18 share one
line-joining prelude.

## Acceptance criteria

- [ ] Given an ADR whose Decision holds a fenced block with a line `1. x`, when check 18 counts
  its clauses, then the fenced line is not counted (a new self-test case).
- [ ] Given a Markdown file with a heading inside fenced code, when a link names that heading's
  anchor, then the anchor check and check 17 both refuse it.
- [ ] Given `scripts/test-check-docs.sh`, when the change lands, then it passes, and
  `check-docs.sh` passes on the tree.

## Read first

- Touches: `scripts/check-docs.sh`, `scripts/test-check-docs.sh`, `scripts/lib/adr.sh`
- CONVENTIONS: "A self-test asserts what it builds".
- ADR-0001 (the checks of the record), ADR-0002.
- Pull request #180 edits the header of check 18; work this after it lands.

## Blocked by

- None (can start immediately)

## Mode

with the human: ADR-0007 clause 5 keeps every part of the system with the human and sets the cap
on open unattended work at zero.
