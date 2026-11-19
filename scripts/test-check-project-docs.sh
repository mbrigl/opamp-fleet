#!/usr/bin/env bash
#
# Self-test of scripts/check-project-docs.sh. Verifies: NUC-adrs-are-named-by-category
#
# Each case builds a throwaway git repository under a temporary directory — a README with a
# Template section, a CONTRIBUTING.md with a Project Layout and a Repository settings section, a
# workflow, and an ADR record with its index — runs the check against it, and asserts the exit
# code and, for a failing case, the line of the violation. The baseline passes; every other case
# names a section or cites a clause, rightly or not. The cases stood in scripts/test-check-docs.sh
# while the rules were items of scripts/check-docs.sh. Section references and ADR identifiers
# inside these fixtures are assembled, never written literally, so that neither check reads this
# script's own fixtures as references into this repository. Last, the rule keys in the script's
# header and functions, and the two lines of scripts/check-all.sh that run the check.
#
# Inputs: scripts/check-project-docs.sh scripts/lib/ scripts/check-all.sh
#
# Usage:
#     scripts/test-check-project-docs.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
CHECK="$ROOT/scripts/check-project-docs.sh"

# insert_before <file> <line prefix> <text> — insert text, where "\n" starts a new line, before
# the first line that starts with the prefix; awk reads "\n" the same way on every platform.
insert_before() {
  awk -v prefix="$2" -v text="$3" 'index($0, prefix) == 1 && !done { print text; done = 1 } { print }' \
    "$1" > "$1.tmp" && mv "$1.tmp" "$1"
}

# adr_id <n> — the identifier SEC-decision-<n>, assembled so that the checks do not read it.
adr_id() { printf '%s-decision-%s' SEC "$1"; }

# adr <dir> <number> <status emoji + word> — write one ADR with its header.
adr() {
  local dir="$1" n="$2" status="$3"
  {
    echo "# $(adr_id "$n"): decision $n"
    echo
    echo "- **Status:** $status"
    echo "- **Applies to:** everything"
    echo
    echo "## Decision"
    echo
    echo "We will."
  } > "$dir/docs/adr/$(adr_id "$n").md"
}

# index_row <number> <status emoji + word> — one row of the ADR index for the given ADR.
index_row() { echo "| [$(adr_id "$1")]($(adr_id "$1").md) | Decision $1 | everything | $2 |"; }

# repo <name> — a fixture repository that passes the check, printed as its path. The README has
# five lines and the workflow four, so that a line a case appends is the sixth or the fifth.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr" "$dir/.github/workflows"
  git -C "${dir:?}" init -q -b main

  {
    echo "# Fixture"
    echo
    echo "## Template"
    echo
    echo "- **Template release:** unreleased"
  } > "$dir/README.md"

  {
    echo "# Contributing"
    echo
    echo "## Project Layout"
    echo
    echo "## Repository settings"
  } > "$dir/CONTRIBUTING.md"

  {
    echo "jobs:"
    echo "  docs:"
    echo "    steps:"
    echo "      - run: bash scripts/check-docs.sh"
  } > "$dir/.github/workflows/checks.yml"

  {
    echo "# <CAT>-<slug>: title"
    echo
    echo "- **Status:** 🟡 proposed"
  } > "$dir/docs/adr/template.md"

  adr "$dir" 1 "🟡 proposed"
  {
    echo "# Architecture Decision Records"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    index_row 1 "🟡 proposed"
  } > "$dir/docs/adr/README.md"

  echo "$dir"
}

# expect <pass|fail> <name> <dir> [needle] — run the check; when a needle is given, its output
# must contain it.
expect() {
  local want="$1" name="$2" dir="$3" needle="${4:-}"
  local out rc
  git -C "${dir:?}" add -A
  out="$(bash "$CHECK" --root "$dir" 2>&1)"; rc=$?
  if output_holds "$out" "$needle" && [[ ( "$want" == "pass" && $rc -eq 0 ) || ( "$want" == "fail" && $rc -ne 0 ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected $want${needle:+ with needle \"$needle\"}, exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# consolidation <dir> <flip> [wrap] — ADR 3 supersedes ADRs 1 and 2, the index to match; with
# flip yes both old ones are flipped, else ADR 2 is still proposed.
consolidation() {
  local dir="$1" flip="$2" wrap="${3:-}" n3 sep=", "
  n3="$(adr_id 3)"
  [[ "$wrap" == "wrap" ]] && sep=",\n  "
  adr "$dir" 1 "⚪ superseded by $n3"
  if [[ "$flip" == "yes" ]]; then adr "$dir" 2 "⚪ superseded by $n3"; else adr "$dir" 2 "🟡 proposed"; fi
  adr "$dir" 3 "🟢 accepted"
  insert_before "$dir/docs/adr/$(adr_id 3).md" "- **Applies to:**" \
    "- **Supersedes:** [$(adr_id 1)]($(adr_id 1).md)${sep}[$(adr_id 2)]($(adr_id 2).md)"
  {
    echo "# Architecture Decision Records"
    echo
    echo "### Binding"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    [[ "$flip" == "yes" ]] || index_row 2 "🟡 proposed"
    index_row 3 "🟢 accepted"
    echo
    echo "### Superseded and rejected"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    index_row 1 "⚪ superseded by $n3"
    [[ "$flip" == "yes" ]] && index_row 2 "⚪ superseded by $n3"
  } > "$dir/docs/adr/README.md"
}

# --- cases: the baseline -------------------------------------------------------------------

d="$(repo baseline)"
expect pass "the baseline names no section and cites no clause" "$d"

# --- cases: a section of a README or CONTRIBUTING.md named in prose (docs/named-sections) -

# named <file> <heading> — a reference to a heading by name, its arrow assembled for the same
# reason as adr_id.
named() { printf '%s \342\206\222 "%s"' "$1" "$2"; }

d="$(repo named-missing)"
echo "# Runs the docs ($(named README.md Nowhere))." >> "$d/.github/workflows/checks.yml"
expect fail "a named section no heading of the README carries" "$d" \
  '.github/workflows/checks.yml:5: "Nowhere" names no heading of README.md'

d="$(repo named-present)"
{
  echo "# Runs the docs ($(named README Template), $(named CONTRIBUTING.md 'Repository settings'))."
  echo "# Listed as $(named CONTRIBUTING.md 'Project Layout') says."
} >> "$d/.github/workflows/checks.yml"
expect pass "a named section of the README and of CONTRIBUTING.md that exists" "$d"

d="$(repo named-wrapped)"
printf '# Gated (CONTRIBUTING.md \342\206\222 "Repository\n# settings").\n' \
  >> "$d/.github/workflows/checks.yml"
expect pass "a named section wrapped inside its heading is joined" "$d"

d="$(repo named-wrapped-missing)"
printf '# Runs the docs (README.md \342\206\222\n# "Nowhere").\n' >> "$d/.github/workflows/checks.yml"
expect fail "a named section wrapped after its arrow is joined" "$d" \
  '.github/workflows/checks.yml:5: "Nowhere" names no heading of README.md'

d="$(repo named-quoted-word)"
echo '# The release sets "unreleased" in README.md to the tag.' >> "$d/.github/workflows/checks.yml"
expect pass "a quoted word before 'in README.md' is not read as a heading" "$d"

d="$(repo named-comment-wrap)"
mkdir -p "$d/src"
printf '//! Built as README.md \342\206\222\n//! "Nowhere" says.\n' > "$d/src/lib.rs"
expect fail "a reference wrapped under a '//!' comment is joined" "$d" \
  'src/lib.rs:1: "Nowhere" names no heading of README.md'

d="$(repo named-fenced)"
# shellcheck disable=SC2016  # the backticks are a Markdown fence, not a command substitution
printf '\n```\n## Fenced\n```\n' >> "$d/README.md"
echo "# Runs the docs ($(named README.md Fenced))." >> "$d/.github/workflows/checks.yml"
expect fail "a heading inside a fenced code block is no heading" "$d" \
  '.github/workflows/checks.yml:5: "Fenced" names no heading of README.md'

d="$(repo named-beside)"
mkdir -p "$d/sub"
printf '# Sub\n\n## Local\n' > "$d/sub/README.md"
echo "See $(named README.md Local)." > "$d/sub/notes.md"
expect pass "the README beside the file is the one named" "$d"

d="$(repo named-beside-first)"
mkdir -p "$d/sub"
printf '# Sub\n\n## Local\n' > "$d/sub/README.md"
echo "See $(named README Template)." > "$d/sub/notes.md"
expect fail "the README beside the file is chosen before the root one" "$d" \
  'sub/notes.md:1: "Template" names no heading of sub/README.md'

d="$(repo named-up)"
mkdir -p "$d/sub"
printf '# Sub\n\n## Local\n' > "$d/sub/README.md"
echo "See $(named ../README.md Template)." > "$d/sub/notes.md"
expect pass "a file name after '../' is a path, past the README beside the file" "$d"

d="$(repo named-up-missing)"
echo "# Runs the docs ($(named ../README.md Template))." >> "$d/.github/workflows/checks.yml"
expect fail "a path to a document that does not exist" "$d" \
  '.github/workflows/checks.yml:5: "Template" names a section of ../README.md, which does not exist'

d="$(repo named-root)"
mkdir -p "$d/sub"
echo "See $(named README.md Template)." > "$d/sub/notes.md"
expect pass "with no README beside the file, the root one is named" "$d"

d="$(repo named-placeholder)"
echo "Every $(named README '<Heading>') is checked." >> "$d/README.md"
expect pass "a placeholder heading in angle brackets names nothing" "$d"

# --- cases: clause citations (docs/clause-citations) --------------------------------------

# S — the section sign, assembled so that docs/section-refs does not read the fixtures as sections of
# AGENTS.md.
S="$(printf '\302\247')"

# clauses <dir> <number> <count> — give ADR <number> a Decision of <count> numbered clauses.
clauses() {
  local i
  for ((i = 1; i <= $3; i++)); do
    printf '\n%d. **Clause %d.** We will.\n' "$i" "$i"
  done >> "$1/docs/adr/$(adr_id "$2").md"
}

d="$(repo clause-forms)"
clauses "$d" 1 4
mkdir -p "$d/src"
{
  echo "// $(adr_id 1) ${S}4 and $(adr_id 1) ${S}1; $(adr_id 1) ${S}2, ${S}3."
  echo "// $(adr_id 1) ${S}1, ${S}2 and ${S}3, $(adr_id 1) ${S}1, ${S}2, and ${S}4, $(adr_id 1) ${S}1–${S}4."
  echo "// [$(adr_id 1) ${S}2](docs/adr/$(adr_id 1).md), and wrapped: $(adr_id 1)"
  echo "// ${S}3."
} > "$d/src/lib.rs"
expect pass "a clause cited in every form a citation takes, within the Decision" "$d"

d="$(repo clause-beyond)"
clauses "$d" 1 3
echo "Decided by $(adr_id 1) ${S}4." >> "$d/README.md"
expect fail "a clause beyond the Decision's numbered clauses" "$d" \
  "README.md:6: $(adr_id 1) ${S}4 is cited, and its Decision has 3"

d="$(repo clause-range-beyond)"
clauses "$d" 1 3
echo "Decided by $(adr_id 1) ${S}1, ${S}2, and ${S}2–${S}5." >> "$d/README.md"
expect fail "a range whose end lies beyond the Decision" "$d" \
  "README.md:6: $(adr_id 1) ${S}5 is cited, and its Decision has 3"

d="$(repo clause-of-beyond)"
clauses "$d" 1 3
echo "As clause 7 of [$(adr_id 1)](docs/adr/$(adr_id 1).md) says." >> "$d/README.md"
expect fail "a clause named before 'of' and the identifier, the older form" "$d" \
  "README.md:6: $(adr_id 1) is cited by clause N of, a place a normalization cannot follow"

d="$(repo clause-wrapped)"
clauses "$d" 1 3
mkdir -p "$d/src"
printf '/// Held as %s\n/// %s9 says.\n' "$(adr_id 1)" "$S" > "$d/src/lib.rs"
expect fail "a citation wrapped under a '///' comment is joined" "$d" \
  "src/lib.rs:1: $(adr_id 1) ${S}9 is cited, and its Decision has 3"

d="$(repo clause-none)"
echo "Decided by $(adr_id 1) ${S}1." >> "$d/README.md"
expect pass "a Decision that numbers no clauses is not held, as the template's fixtures are not" "$d"

d="$(repo clause-place)"
clauses "$d" 1 3
echo "Decided by $(adr_id 1), point 2." >> "$d/README.md"
expect fail "a place cited instead of a clause" "$d" \
  "README.md:6: $(adr_id 1) is cited by point, a place a normalization cannot follow"

d="$(repo clause-superseded)"
consolidation "$d" yes
clauses "$d" 1 3
clauses "$d" 3 3
echo "Decided by $(adr_id 1) ${S}2." >> "$d/README.md"
expect fail "a clause of a superseded ADR, outside the record" "$d" \
  "README.md:6: $(adr_id 1) is no longer in force"

d="$(repo clause-superseded-history)"
consolidation "$d" yes
clauses "$d" 1 3
clauses "$d" 3 3
echo "It restates $(adr_id 1) ${S}2." >> "$d/docs/adr/$(adr_id 3).md"
printf '# Changelog\n\n- Changed as %s %s3 and %s, point 1 said.\n' "$(adr_id 1)" "$S" "$(adr_id 1)" \
  > "$d/CHANGELOG.md"
echo "The successor's $(adr_id 3) ${S}9." >> "$d/docs/adr/$(adr_id 1).md"
expect pass "a superseded ADR cited by the record and the changelog; a superseded ADR not read" "$d"

d="$(repo clause-forms-beyond)"
clauses "$d" 1 2
mkdir -p "$d/src"
{
  echo "// $(adr_id 1) ${S}3."
  echo "// [$(adr_id 1) ${S}4](docs/adr/$(adr_id 1).md)."
  echo "// $(adr_id 1) ${S}1 and ${S}5."
  echo "// $(adr_id 1) ${S}1, ${S}2, and ${S}6."
  echo "// $(adr_id 1)"
  echo "// ${S}7."
  echo "let s = \"per $(adr_id 1) \\"
  echo "    ${S}8\";"
} > "$d/src/lib.rs"
for want in "src/lib.rs:1: $(adr_id 1) ${S}3 " "src/lib.rs:2: $(adr_id 1) ${S}4 " \
  "src/lib.rs:3: $(adr_id 1) ${S}5 " "src/lib.rs:4: $(adr_id 1) ${S}6 " \
  "src/lib.rs:5: $(adr_id 1) ${S}7 " "src/lib.rs:7: $(adr_id 1) ${S}8 "; do
  expect fail "each form a citation takes is read: ${want%% *}" "$d" "$want"
done

d="$(repo clause-zero)"
clauses "$d" 1 3
echo "Decided by $(adr_id 1) ${S}0." >> "$d/README.md"
expect fail "clause 0, below the Decision's first" "$d" \
  "README.md:6: $(adr_id 1) ${S}0 is cited, and its Decision has 3"

# A fence inside the Decision holds a '## ' line and numbered lines: neither ends the section nor
# counts as a clause, so the clause after the fence is the third. Read the fence as text and the
# count is 5; let its heading end the section and it is 2.
d="$(repo clause-fenced)"
clauses "$d" 1 2
printf '\n~~~markdown\n## Example\n\n3. x\n4. x\n~~~\n\n3. **Clause 3.** We will.\n' \
  >> "$d/docs/adr/$(adr_id 1).md"
echo "Decided by $(adr_id 1) ${S}4." >> "$d/README.md"
expect fail "a numbered line or a heading inside a fence in the Decision is no clause" "$d" \
  "README.md:6: $(adr_id 1) ${S}4 is cited, and its Decision has 3"

d="$(repo clause-separators)"
clauses "$d" 1 3
mkdir -p "$d/src"
{
  echo "// $(adr_id 1) ${S}2"
  echo "// 404 answers it."
  echo "// $(adr_id 1) ${S}3, 2026-11-14 was the day."
} > "$d/src/lib.rs"
expect pass "a number after bare space or one a date goes on from is no clause" "$d"

d="$(repo clause-older-word)"
clauses "$d" 1 3
echo "Decided by $(adr_id 1) clause 2." >> "$d/README.md"
expect fail "the older word clause instead of the section sign" "$d" \
  "README.md:6: $(adr_id 1) is cited by the older word clause, a place"

d="$(repo clause-after-link)"
clauses "$d" 1 3
mkdir -p "$d/src"
echo "// [$(adr_id 1)](docs/adr/$(adr_id 1).md) ${S}2." > "$d/src/lib.rs"
expect fail "the section sign after a link instead of in its text" "$d" \
  "src/lib.rs:1: $(adr_id 1) is cited by the section sign after its link, a place"

for place in "paragraph 2" "line 2" "rule 2" "item 2" "step 2" "section 2"; do
  d="$(repo "clause-place-${place%% *}")"
  clauses "$d" 1 3
  echo "Decided by $(adr_id 1) $place." >> "$d/README.md"
  word="${place% 2}"
  expect fail "a place cited instead of a clause: $word" "$d" \
    "README.md:6: $(adr_id 1) is cited by $word, a place"
done

d="$(repo clause-rejected)"
adr "$d" 1 "🔴 rejected"
clauses "$d" 1 3
{
  echo "# Architecture Decision Records"
  echo
  echo "| ADR | Title | Applies to | Status |"
  echo "|-----|-------|------------|--------|"
  echo
  echo "### Superseded and rejected"
  echo
  echo "| ADR | Title | Applies to | Status |"
  echo "|-----|-------|------------|--------|"
  index_row 1 "🔴 rejected"
} > "$d/docs/adr/README.md"
echo "Decided by $(adr_id 1) ${S}2." >> "$d/README.md"
expect fail "a clause of a rejected ADR" "$d" "README.md:6: $(adr_id 1) is no longer in force"

d="$(repo clause-template)"
echo "As $(adr_id 1) ${S}9 says." >> "$d/docs/adr/template.md"
expect pass "the template is not read" "$d"

# --- cases: the rule keys and the runner ---------------------------------------------------

for key in docs/named-sections docs/clause-citations; do
  check "the header lists $key, and one check carries it" \
    test "$(grep -cxE "#   $key|check_[a-z_]+\(\) \{  # $key" "$CHECK")" = 2
done
check "check-all.sh runs the check" \
  grep -qx -- 'run "Project documentation" scripts/check-project-docs.sh' "$ROOT/scripts/check-all.sh"
check "check-all.sh runs the self-test" \
  grep -qx -- 'run "Project documentation self-test" scripts/test-check-project-docs.sh' \
  "$ROOT/scripts/check-all.sh"

# --- summary -------------------------------------------------------------------------------

summary "Project documentation self-test"
