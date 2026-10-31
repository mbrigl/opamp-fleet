#!/usr/bin/env bash
#
# Self-test of scripts/check-docs.sh. Verifies: ADR-0001 ADR-0003 ADR-0005
#
# Each case builds a throwaway git repository under a temporary directory — a rule file with
# numbered sections, a README with a Project Layout block, the required-checks markers, and the
# Template release line, a workflow and a local runner listing the same check, an ADR record with
# its index, and one skill with its pointer — runs the check against it, and asserts the exit
# code and, for a failing case, the message that names the violation. The baseline passes; every
# other case breaks one thing. The skill pointer check (check 11 of check-docs.sh) is covered in
# every direction; of the older checks, one case each pins the message a maintainer would meet
# first — check 9 gets two more for the toolchain job a derived project adds, checks 12 and 13
# (inherited ADRs decided and the template's identity replaced once the setup file is gone) have
# a case for each of their conditions, and check 14 (the template release named) one for each
# form the line may take.
#
# Section references and ADR numbers inside fixtures are assembled, never written literally, so
# that check-docs.sh does not read this script's own fixtures as references into this repository.
#
# Inputs: scripts/check-docs.sh scripts/lib/
#
# Usage:
#     scripts/test-check-docs.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
CHECK="$ROOT/scripts/check-docs.sh"

# sed_in <file> <sed script> — edit a fixture file in place, the same way on GNU and BSD sed.
sed_in() {
  sed "$2" "$1" > "$1.tmp" && mv "$1.tmp" "$1"
}

# layout <dir> <path...> — name each path in the Project Layout block of the fixture's README,
# for a case that adds a top-level file for another reason (check 10).
layout() {
  local dir="$1"; shift
  awk -v add="$(printf '%s\n' "$@")" '
    { print }
    $1 == "AGENTS.md" && !done { n = split(add, p, "\n"); for (i = 1; i <= n; i++) if (p[i] != "") print p[i] "  # added"; done = 1 }
  ' "$dir/README.md" > "$dir/README.md.tmp" && mv "$dir/README.md.tmp" "$dir/README.md"
}

# insert_before <file> <line prefix> <text> — insert text, where "\n" starts a new line, before
# the first line that starts with the prefix; awk reads "\n" the same way on every platform.
insert_before() {
  awk -v prefix="$2" -v text="$3" 'index($0, prefix) == 1 && !done { print text; done = 1 } { print }' \
    "$1" > "$1.tmp" && mv "$1.tmp" "$1"
}

# sect <n> — the section sign followed by n, assembled so that this file carries no literal one.
sect() { printf '\302\247%s' "$1"; }

# adr_id <n> — the identifier ADR-000n, assembled for the same reason.
adr_id() { printf 'ADR-%04d' "$1"; }

# adr <dir> <number> <status emoji + word> — write one ADR with the header the index mirrors.
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
  } > "$dir/docs/adr/$(printf '%04d' "$n")-decision.md"
}

# index_row <number> <status emoji + word> — one row of the ADR index for the given ADR.
index_row() {
  local n
  n="$(printf '%04d' "$1")"
  echo "| [$n]($n-decision.md) | Decision $n | everything | $2 |"
}

# repo <name> — a fixture repository that passes every check, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr" "$dir/scripts" "$dir/.github/workflows" \
           "$dir/.agents/skills/review" "$dir/.claude/skills"
  git -C "${dir:?}" init -q

  {
    echo "# Agent Guide"
    echo
    echo "## 1. Principles"
    echo
    echo "Be simple."
    echo
    echo "## 2. Rules"
    echo
    echo "Follow them."
  } > "$dir/AGENTS.md"

  {
    echo "# Fixture"
    echo
    echo "## Project Layout"
    echo
    echo '```'
    echo "README.md             # this file"
    echo "AGENTS.md             # the rule file"
    echo "docs/adr/             # decisions"
    echo "scripts/              # the checks"
    echo ".github/              # the workflows"
    echo ".agents/skills/       # the skills"
    echo ".claude/skills/       # their pointers"
    echo '```'
    echo
    echo "## Repository settings"
    echo
    echo "<!-- required-checks begin -->"
    # shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
    echo '`docs`'
    echo "<!-- required-checks end -->"
    echo
    echo "## Template"
    echo
    echo "- **Template release:** unreleased"
  } > "$dir/README.md"

  {
    echo "jobs:"
    echo "  docs:"
    echo "    steps:"
    echo "      - run: bash scripts/check-docs.sh"
  } > "$dir/.github/workflows/checks.yml"

  echo 'run "Documentation consistency" scripts/check-docs.sh' > "$dir/scripts/check-all.sh"

  {
    echo "# $(printf 'ADR-%s' NNNN): title"
    echo
    echo "- **Status:** 🟡 proposed"
    echo
    echo "*Complete the header and delete this paragraph.*"
  } > "$dir/docs/adr/template.md"

  adr "$dir" 1 "🟡 proposed"
  {
    echo "# Architecture Decision Records"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    index_row 1 "🟡 proposed"
  } > "$dir/docs/adr/README.md"

  printf -- '---\nname: review\ndescription: Review a change.\n---\n\n# Review\n' \
    > "$dir/.agents/skills/review/SKILL.md"
  ln -s ../../.agents/skills/review "$dir/.claude/skills/review"

  echo "$dir"
}

# expect <pass|fail> <name> <dir> [needle] — run the check; when a needle is given, its output
# must contain it.
expect() {
  local want="$1" name="$2" dir="$3" needle="${4:-}"
  local out rc
  git -C "${dir:?}" add -A
  out="$(bash "$CHECK" "$dir" 2>&1)"; rc=$?
  if [[ "$want" == "pass" && $rc -eq 0 && ( -z "$needle" || "$out" == *"$needle"* ) ]] \
     || [[ "$want" == "fail" && $rc -ne 0 && ( -z "$needle" || "$out" == *"$needle"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected $want${needle:+ with needle \"$needle\"}, exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases: the baseline -----------------------------------------------------------------

d="$(repo baseline)"
expect pass "a consistent repository passes" "$d"

# --- cases: skill pointers (check 11) ----------------------------------------------------

d="$(repo pointer-missing)"
rm "$d/.claude/skills/review"
expect fail "a skill without its pointer" "$d" ".claude/skills/review: no pointer"

d="$(repo pointer-dangling)"
rm -r "$d/.agents/skills/review"
expect fail "a pointer whose skill is gone" "$d" ".claude/skills/review: dangles"

d="$(repo pointer-copy)"
rm "$d/.claude/skills/review"
cp -r "$d/.agents/skills/review" "$d/.claude/skills/review"
expect fail "a copy where a symlink belongs" "$d" ".claude/skills/review: is a copy"

d="$(repo pointer-wrong-target)"
mkdir -p "$d/.agents/skills/other"
printf -- '---\nname: other\ndescription: Other.\n---\n' > "$d/.agents/skills/other/SKILL.md"
ln -s ../../.agents/skills/review "$d/.claude/skills/other"
expect fail "a pointer at a different skill" "$d" ".claude/skills/other: points at"

d="$(repo pointer-only-skill)"
mkdir -p "$d/.claude/skills/local"
printf -- '---\nname: local\ndescription: Local.\n---\n' > "$d/.claude/skills/local/SKILL.md"
expect fail "a skill that lives only in the pointer directory" "$d" ".claude/skills/local: is not a symlink"

d="$(repo pointer-directory-only)"
rm -r "$d/.agents" "$d/.claude/skills/review"
mkdir -p "$d/.claude/skills/review"
printf -- '---\nname: review\ndescription: Review.\n---\n' > "$d/.claude/skills/review/SKILL.md"
expect fail "skills only in the pointer directory, none for the other agents" "$d" ".claude/skills/review: is not a symlink"

d="$(repo no-skills)"
rm -r "$d/.agents" "$d/.claude"
sed_in "$d/README.md" '/^\.agents\/skills\/ /d; /^\.claude\/skills\/ /d'
expect pass "a repository without skills has nothing to check" "$d"

# --- cases: inherited ADRs after setup (check 12) ---------------------------------------

# inherited <dir> <status emoji + word> <deciders> — ADR 1 as an ADR inherited from the template,
# with the given status in the file and the index and the given Deciders line.
inherited() {
  local dir="$1" status="$2" deciders="$3"
  adr "$dir" 1 "$status"
  insert_before "$dir/docs/adr/0001-decision.md" "- **Applies to:**" "- **Deciders:** $deciders"
  {
    echo "# Architecture Decision Records"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    index_row 1 "$status"
  } > "$dir/docs/adr/README.md"
}

d="$(repo inherited-undecided)"
inherited "$d" "🟡 proposed" "NUC maintainer"
expect fail "an inherited ADR nobody here decided, with the setup file gone" "$d" "0001-decision.md: inherited from the template and not decided by this project"

d="$(repo inherited-pending)"
inherited "$d" "🟡 proposed" "NUC maintainer"
echo "# Template setup" > "$d/TEMPLATE-SETUP.md"
layout "$d" TEMPLATE-SETUP.md
expect pass "an inherited ADR still proposed while the setup file exists" "$d"

d="$(repo inherited-accepted)"
inherited "$d" "🟢 accepted" "NUC maintainer, project maintainer"
expect pass "an inherited ADR accepted by the project's maintainer" "$d"

d="$(repo inherited-accepted-undecided)"
inherited "$d" "🟢 accepted" "NUC maintainer"
expect fail "an inherited ADR shipped accepted, nobody here added to Deciders" "$d" "0001-decision.md: inherited from the template and not decided by this project"

d="$(repo inherited-accepted-and)"
inherited "$d" "🟢 accepted" "NUC maintainer and project maintainer"
expect pass "an inherited ADR accepted, the project's maintainer joined with 'and'" "$d"

d="$(repo inherited-rejected)"
inherited "$d" "🔴 rejected" "NUC maintainer"
{
  echo "# Architecture Decision Records"
  echo
  echo "### Superseded and rejected"
  echo
  echo "| ADR | Title | Applies to | Status |"
  echo "|-----|-------|------------|--------|"
  index_row 1 "🔴 rejected"
} > "$d/docs/adr/README.md"
expect pass "an inherited ADR the project rejected is history, not read" "$d"

d="$(repo own-proposed)"
inherited "$d" "🟡 proposed" "project maintainer"
expect pass "the project's own proposed ADR after setup" "$d"

d="$(repo inherited-wrapped)"
inherited "$d" "🟡 proposed" "project maintainer (reviewing),\n  NUC maintainer"
expect fail "an inherited ADR whose Deciders field wraps, the template's name on the second line" "$d" "0001-decision.md: inherited from the template and still proposed after setup"

# --- cases: template identity after setup (check 13) ------------------------------------

d="$(repo identity-pending)"
sed_in "$d/README.md" 's/^# Fixture$/# NUC — an Agentic Fixture/'
echo "# Template setup" > "$d/TEMPLATE-SETUP.md"
layout "$d" TEMPLATE-SETUP.md
expect pass "the template's title in the README while the setup file exists" "$d"

d="$(repo identity-title)"
sed_in "$d/README.md" 's/^# Fixture$/# NUC — an Agentic Fixture/'
expect fail "the template's title in the README after setup" "$d" "README.md: the title is still the template's"

d="$(repo identity-intro)"
echo "**NUC** is named after the small *nucleus colony*." >> "$d/README.md"
expect fail "the template's intro in the README after setup" "$d" "README.md: the intro still explains the template's name"

d="$(repo identity-overview)"
echo "Describe what this project does, who it is for, and its main goals." >> "$d/README.md"
expect fail "the scaffold's Overview sentence after setup" "$d" "README.md: the Overview is still the scaffold's sentence"

d="$(repo identity-usage)"
echo "TODO — show a minimal example of using the project." >> "$d/README.md"
expect fail "the Usage TODO after setup" "$d" "README.md: the Usage section is still TODO"

d="$(repo identity-devcontainer)"
mkdir -p "$d/.devcontainer"
echo '{ "name": "NUC DevContainer" }' > "$d/.devcontainer/devcontainer.json"
expect fail "the template's Dev Container name after setup" "$d" "devcontainer.json: the Dev Container still carries the template's name"

d="$(repo identity-badge)"
echo "[![Checks](https://github.com/hivevm/nuc/actions/workflows/checks.yml/badge.svg)](https://github.com/hivevm/nuc/actions/workflows/checks.yml)" >> "$d/README.md"
expect fail "the template's repository in the README badge after setup" "$d" "README.md: a badge still points at the template"

d="$(repo identity-license-badge)"
echo "[![License](https://img.shields.io/github/license/hivevm/nuc)](LICENSE)" >> "$d/README.md"
expect fail "the template's repository in a shields.io badge after setup" "$d" "README.md: a badge still points at the template"

d="$(repo identity-template-link)"
echo "Created from [NUC](https://github.com/hivevm/nuc)." >> "$d/README.md"
expect pass "a link to the template's repository outside the badge after setup" "$d"

d="$(repo identity-license)"
printf 'MIT License\n\nCopyright (c) 2026 Maintainer\n' > "$d/LICENSE"
expect fail "the placeholder copyright holder after setup" "$d" "LICENSE: the copyright holder is still the placeholder"

d="$(repo identity-license-apache)"
printf 'Apache License\nVersion 2.0, January 2004\n\nCopyright 2026 Maintainer\n' > "$d/LICENSE"
expect fail "the placeholder copyright holder under another license after setup" "$d" "LICENSE: the copyright holder is still the placeholder"

d="$(repo identity-contact)"
echo "- email the maintainer: <!-- TODO: add a security contact address -->." > "$d/SECURITY.md"
expect fail "the placeholder security contact after setup" "$d" "SECURITY.md: the security contact is still the placeholder"

d="$(repo identity-conduct)"
echo "- Reporting contact: <!-- TODO: add a contact address (email or private channel) -->" > "$d/CODE_OF_CONDUCT.md"
expect fail "the placeholder reporting contact after setup" "$d" "CODE_OF_CONDUCT.md: the reporting contact is still the placeholder"

d="$(repo identity-codeowners)"
printf '# /docs/SPECIFICATION.md @owner\n# /docs/adr/ @owner\n' > "$d/.github/CODEOWNERS"
expect fail "a CODEOWNERS with its rules still commented out after setup" "$d" ".github/CODEOWNERS: no active rule"

d="$(repo identity-codeowners-todo)"
printf '# TODO: add a code owner and uncomment the rules.\n/docs/adr/ @someone\n' > "$d/.github/CODEOWNERS"
expect fail "the code owner TODO still there after setup" "$d" ".github/CODEOWNERS: the instruction to add a code owner is still there"

d="$(repo identity-spec-title)"
printf '# Specification — <Project Name>\n' > "$d/docs/SPECIFICATION.md"
expect fail "the scaffold's title in the specification after setup" "$d" "docs/SPECIFICATION.md: the title still carries '<Project Name>'"

d="$(repo identity-glossary-title)"
printf '# Glossary — <Project Name>\n' > "$d/docs/GLOSSARY.md"
expect fail "the scaffold's title in the glossary after setup" "$d" "docs/GLOSSARY.md: the title still carries '<Project Name>'"

d="$(repo identity-goal)"
printf '# Specification — Fixture\n\n## Goals / Success Criteria\n\n1. **G-1** — …\n' > "$d/docs/SPECIFICATION.md"
expect fail "a goal still the scaffold's ellipsis after setup" "$d" "docs/SPECIFICATION.md: a criterion is still the scaffold's placeholder"

d="$(repo identity-quality)"
printf '# Specification — Fixture\n\n## Quality Goals\n\n- **Q-1** — <Quality, e.g. Performance>: why.\n' > "$d/docs/SPECIFICATION.md"
expect fail "a quality goal still the scaffold's placeholder after setup" "$d" "docs/SPECIFICATION.md: a criterion is still the scaffold's placeholder"

for entry in ARCHITECTURE:Part GLOSSARY:Term CONVENTIONS:Convention; do
  d="$(repo "identity-entry-${entry#*:}")"
  printf '# Fixture\n\n- **<%s>** — what it is.\n' "${entry#*:}" > "$d/docs/${entry%%:*}.md"
  expect fail "the scaffold's <${entry#*:}> entry after setup" "$d" "docs/${entry%%:*}.md: the scaffold's"
done

d="$(repo identity-build)"
echo "- **Build:** TODO <!-- e.g. \`make build\` -->" >> "$d/README.md"
expect fail "a Build command still TODO after setup" "$d" "README.md: a Build, Test, Lint, or Run command is still TODO"

d="$(repo identity-lint)"
echo "- **Lint:** TODO" >> "$d/README.md"
expect fail "a Lint command still TODO after setup" "$d" "README.md: a Build, Test, Lint, or Run command is still TODO"

d="$(repo identity-build-pending)"
echo "- **Build:** TODO" >> "$d/README.md"
echo "# Template setup" > "$d/TEMPLATE-SETUP.md"
layout "$d" TEMPLATE-SETUP.md
expect pass "a Build command still TODO while the setup file exists" "$d"

d="$(repo identity-done)"
printf '# owners\n\n* @someone\n' > "$d/.github/CODEOWNERS"
printf 'MIT License\n\nCopyright (c) 2026 Someone\n' > "$d/LICENSE"
printf '# Specification — Fixture\n\n## Goals / Success Criteria\n\n1. **G-1** — When asked, the system shall answer.\n' > "$d/docs/SPECIFICATION.md"
echo "- **Build:** \`make build\`" >> "$d/README.md"
layout "$d" LICENSE
expect pass "identity replaced after setup" "$d"

# --- cases: prose width (check 15) ---------------------------------------------------------

# words_of <n> — n characters of prose, words of four letters and a space.
words_of() { local s=""; while ((${#s} < $1)); do s+="word "; done; printf '%s' "${s:0:$1}"; }

d="$(repo width-long)"
words_of 101 >> "$d/README.md"; echo >> "$d/README.md"
expect fail "a prose line over 100 columns" "$d" "101 columns — wrap the prose at 100"

d="$(repo width-limit)"
words_of 100 >> "$d/README.md"; echo >> "$d/README.md"
expect pass "a prose line of exactly 100 columns" "$d"

d="$(repo width-dashes)"
printf '%s — %s\n' "$(words_of 48)" "$(words_of 49)" >> "$d/README.md"
expect pass "an em dash counts as one column" "$d"

d="$(repo width-link)"
printf '  [a long link text](https://example.invalid/%s)\n' "$(words_of 100 | tr ' ' x)" >> "$d/README.md"
expect pass "a link that cannot break stands on a line of its own" "$d"

d="$(repo width-exempt)"
# shellcheck disable=SC2016  # the backticks are a Markdown code fence, not a command substitution
printf '| %s |\n\n## %s\n\n```\n%s\n```\n' "$(words_of 120)" "$(words_of 120)" "$(words_of 120)" >> "$d/README.md"
expect pass "a table row, a heading, and a code block are exempt" "$d"

d="$(repo width-inner-fence)"
# shellcheck disable=SC2016  # the backticks are Markdown code fences, not a command substitution
printf '````\n```\n%s\n```\n````\n' "$(words_of 120)" >> "$d/README.md"
expect pass "a fence inside a longer fence does not close it" "$d"

d="$(repo width-generated)"
words_of 120 > "$d/CHANGELOG.md"; echo >> "$d/CHANGELOG.md"
echo "CHANGELOG.md linguist-generated" > "$d/.gitattributes"
layout "$d" CHANGELOG.md .gitattributes
expect pass "a file git marks as generated is not read" "$d"

d="$(repo width-generated-unset)"
words_of 120 > "$d/CHANGELOG.md"; echo >> "$d/CHANGELOG.md"
echo "CHANGELOG.md -linguist-generated" > "$d/.gitattributes"
expect fail "a file marked not generated is read" "$d" "CHANGELOG.md:1: 120 columns"

d="$(repo width-link-then-prose)"
printf '[a link](https://example.invalid/%s) and prose that could wrap\n' "$(words_of 100 | tr ' ' x)" >> "$d/README.md"
expect fail "prose after a long link can wrap" "$d" "columns — wrap the prose at 100"

d="$(repo width-label)"
printf -- '- **Label:** %s\n' "$(words_of 110)" >> "$d/README.md"
expect fail "a bold label outside an ADR header is prose" "$d" "columns — wrap the prose at 100"

# --- cases: the template release named (check 14) ---------------------------------------

# release_line <dir> <text> — replace the baseline's Template release line with the given text.
release_line() { sed_in "$1/README.md" "s/^- \*\*Template release:\*\* unreleased$/$2/"; }

d="$(repo release-tag)"
release_line "$d" '- **Template release:** v1.4.0 of the template ([tags](https:\/\/example.invalid\/tags)).'
expect pass "a release tag followed by a sentence" "$d"

d="$(repo release-plain-line)"
release_line "$d" '**Template release:** v0.2.0'
expect pass "the line without a list marker" "$d"

d="$(repo release-linked)"
release_line "$d" '* **Template release:** [v1.4.0](https:\/\/example.invalid\/releases\/tag\/v1.4.0), taken up in full.'
expect pass "the tag as link text after a star marker" "$d"

d="$(repo release-backticked)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
release_line "$d" '- **Template release:** `v1.4.0`'
expect pass "the tag in backticks" "$d"

d="$(repo release-leading-zero)"
release_line "$d" '- **Template release:** v01.4.0'
expect fail "a leading zero, which SemVer forbids" "$d" "names neither a 'vX.Y.Z' tag nor 'unreleased'"

d="$(repo release-missing)"
release_line "$d" 'Created from the template.'
expect fail "no Template release line" "$d" "no '**Template release:**' line"

d="$(repo release-malformed)"
release_line "$d" '- **Template release:** 1.4'
expect fail "a version that is not a vX.Y.Z tag" "$d" "names neither a 'vX.Y.Z' tag nor 'unreleased'"

d="$(repo release-prerelease)"
release_line "$d" '- **Template release:** v1.4.0-rc1'
expect fail "a pre-release suffix on the tag" "$d" "names neither a 'vX.Y.Z' tag nor 'unreleased'"

d="$(repo release-word)"
release_line "$d" '- **Template release:** latest'
expect fail "a word that is not 'unreleased'" "$d" "names neither a 'vX.Y.Z' tag nor 'unreleased'"

# --- cases: one each for the older checks ------------------------------------------------

d="$(repo broken-link)"
echo "See [the missing file](missing.md)." >> "$d/README.md"
expect fail "a relative link to a file that does not exist" "$d" "broken relative link"

d="$(repo anchor-missing)"
echo "See [the rules](AGENTS.md#3-rules)." >> "$d/README.md"
expect fail "a link to an anchor no heading carries" "$d" "has no matching anchor '#3-rules'"

d="$(repo anchor-slug)"
printf '\n## Bar & Done ##\n' >> "$d/AGENTS.md"
echo "See [the bar](AGENTS.md#bar--done)." >> "$d/README.md"
expect pass "an anchor slugged the way GitHub does, closing hashes dropped" "$d"

d="$(repo anchor-last-line)"
printf '\n## Last' >> "$d/AGENTS.md"
echo "See [the last section](AGENTS.md#last)." >> "$d/README.md"
expect pass "a heading on a last line without a newline anchors" "$d"

d="$(repo index-status)"
adr "$d" 1 "🟢 accepted"
expect fail "an index status that disagrees with the ADR" "$d" "status for '0001-decision.md'"

# Verifies: ADR-0001
d="$(repo header-not-a-list-item)"
sed 's/^- \*\*Status:\*\*/**Status:**/' "$d/docs/adr/0001-decision.md" > "$d/adr.tmp" \
  && mv "$d/adr.tmp" "$d/docs/adr/0001-decision.md"
expect fail "a header field that is no list item" "$d" \
  "0001-decision.md:3: a header field not written as the list item"
d="$(repo header-field-quoted-in-body)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
echo 'A later ADR names this one in its `**Supersedes:**` field.' >> "$d/docs/adr/0001-decision.md"
expect pass "a header field quoted in the body of an ADR" "$d"

d="$(repo numbering-gap)"
adr "$d" 3 "🟡 proposed"
index_row 3 "🟡 proposed" >> "$d/docs/adr/README.md"
expect fail "a gap in the ADR numbering" "$d" "numbers must run 0001..N without gaps"

# consolidation <dir> <flip second: yes|no> [wrap] — ADR 3 supersedes ADRs 1 and 2; the second's
# status is flipped only when asked, so that a check reading the first name alone passes; with
# 'wrap', the second name sits on a continuation line of the field.
consolidation() {
  local dir="$1" flip="$2" wrap="${3:-}" n3 sep=", "
  n3="$(adr_id 3)"
  [[ "$wrap" == "wrap" ]] && sep=",\n  "
  adr "$dir" 1 "⚪ superseded by $n3"
  if [[ "$flip" == "yes" ]]; then adr "$dir" 2 "⚪ superseded by $n3"; else adr "$dir" 2 "🟡 proposed"; fi
  adr "$dir" 3 "🟢 accepted"
  insert_before "$dir/docs/adr/0003-decision.md" "- **Applies to:**" \
    "- **Supersedes:** [$(adr_id 1)](0001-decision.md)${sep}[$(adr_id 2)](0002-decision.md)"
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

# --- cases: the two index tables (check 1) ------------------------------------------------

d="$(repo archive-row-above)"
adr "$d" 1 "🔴 rejected"
{
  echo "# Architecture Decision Records"
  echo
  echo "| ADR | Title | Applies to | Status |"
  echo "|-----|-------|------------|--------|"
  index_row 1 "🔴 rejected"
} > "$d/docs/adr/README.md"
expect fail "a rejected row with no archive heading above it" "$d" "sits above the 'Superseded and rejected' heading"

d="$(repo index-row-twice)"
index_row 1 "🟡 proposed" >> "$d/docs/adr/README.md"
expect fail "a row both sides of a union merge kept" "$d" "lists '0001-decision.md' twice"

d="$(repo binding-row-below)"
{
  echo "# Architecture Decision Records"
  echo
  echo "## Superseded and rejected"
  echo
  echo "| ADR | Title | Applies to | Status |"
  echo "|-----|-------|------------|--------|"
  index_row 1 "🟡 proposed"
} > "$d/docs/adr/README.md"
expect fail "a proposed row under the archive heading" "$d" "sits below the 'Superseded and rejected' heading"

d="$(repo consolidation)"
consolidation "$d" yes
expect pass "one ADR superseding two, both flipped" "$d"

d="$(repo consolidation-half)"
consolidation "$d" no
expect fail "one ADR superseding two, the second not flipped" "$d" "0002-decision.md: 0003-decision.md supersedes it"

d="$(repo consolidation-wrapped)"
consolidation "$d" yes wrap
expect pass "a Supersedes field wrapped onto a second line, both flipped" "$d"

d="$(repo consolidation-wrapped-half)"
consolidation "$d" no wrap
expect fail "a wrapped Supersedes field, the name on the second line not flipped" "$d" "0002-decision.md: 0003-decision.md supersedes it"

# proposed_supersession <dir> <old status> — ADR 2, proposed, supersedes ADR 1, whose status line
# and index row read as given.
proposed_supersession() {
  local dir="$1" old="$2"
  adr "$dir" 1 "$old"
  adr "$dir" 2 "🟡 proposed"
  insert_before "$dir/docs/adr/0002-decision.md" "- **Applies to:**" \
    "- **Supersedes:** [$(adr_id 1)](0001-decision.md)"
  {
    echo "# Architecture Decision Records"
    echo
    echo "### Binding"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    [[ "$old" == ⚪* ]] || index_row 1 "$old"
    index_row 2 "🟡 proposed"
    echo
    echo "### Superseded and rejected"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    [[ "$old" == ⚪* ]] && index_row 1 "$old"
  } > "$dir/docs/adr/README.md"
}

# Verifies: ADR-0001
d="$(repo supersession-proposed)"
proposed_supersession "$d" "🟢 accepted"
expect pass "a proposed supersession, the old ADR still accepted" "$d"
d="$(repo supersession-flipped-early)"
proposed_supersession "$d" "⚪ superseded by $(adr_id 2)"
expect fail "an old ADR flipped while its successor is only proposed" "$d" \
  "0001-decision.md: superseded by $(adr_id 2), which is not accepted"

# A chain: ADR 1 superseded by ADR 2, which ADR 3, accepted, supersedes in its turn.
d="$(repo supersession-chain)"
adr "$d" 1 "⚪ superseded by $(adr_id 2)"
adr "$d" 2 "⚪ superseded by $(adr_id 3)"
adr "$d" 3 "🟢 accepted"
insert_before "$d/docs/adr/0002-decision.md" "- **Applies to:**" \
  "- **Supersedes:** [$(adr_id 1)](0001-decision.md)"
insert_before "$d/docs/adr/0003-decision.md" "- **Applies to:**" \
  "- **Supersedes:** [$(adr_id 2)](0002-decision.md)"
{
  echo "# Architecture Decision Records"
  echo
  echo "### Binding"
  echo
  echo "| ADR | Title | Applies to | Status |"
  echo "|-----|-------|------------|--------|"
  index_row 3 "🟢 accepted"
  echo
  echo "### Superseded and rejected"
  echo
  echo "| ADR | Title | Applies to | Status |"
  echo "|-----|-------|------------|--------|"
  index_row 1 "⚪ superseded by $(adr_id 2)"
  index_row 2 "⚪ superseded by $(adr_id 3)"
} > "$d/docs/adr/README.md"
expect pass "a chain of supersessions, the last successor accepted" "$d"

d="$(repo section-ref)"
echo "Cited in [\`AGENTS.md\` $(sect 9)](AGENTS.md#9-nowhere)." >> "$d/README.md"
expect fail "a section reference that names no section" "$d" "matches no numbered section"

# words <n> — the word form of a section reference, assembled for the same reason as sect.
words() { printf 'AGENTS.md, section %s' "$1"; }

d="$(repo section-words)"
mkdir -p "$d/scripts/lib" && echo "# Required by $(words 9)." > "$d/scripts/lib/run.sh"
expect fail "a reference in words that names no section" "$d" "reference '$(words 9)' matches no numbered section"

d="$(repo section-words-valid)"
mkdir -p "$d/scripts/lib" && echo "# Required by $(words 2)." > "$d/scripts/lib/run.sh"
expect pass "a reference in words to a section that exists" "$d"

d="$(repo section-hook)"
mkdir -p "$d/.githooks"
echo "# See AGENTS.md $(sect 9)." > "$d/.githooks/pre-commit"
expect fail "a section reference in a git hook" "$d" ".githooks/pre-commit:1: reference"

d="$(repo section-json)"
echo "{ \"de\": \"Siehe $(sect 9) BGB\" }" > "$d/messages.json"
layout "$d" messages.json
expect pass "the section sign in JSON is an ordinary character" "$d"

d="$(repo section-json-words)"
echo "{ \"note\": \"$(words 9)\" }" > "$d/settings.json"
expect fail "a reference in words in a JSON file" "$d" "settings.json:1: reference"

d="$(repo unknown-adr-ref)"
echo "Decided in $(adr_id 42)." >> "$d/README.md"
expect fail "a reference to an ADR that does not exist" "$d" "matches no ADR file"

d="$(repo check-lists)"
echo 'run "Extra" scripts/check-extra.sh' >> "$d/scripts/check-all.sh"
expect fail "a check the local runner has and CI does not" "$d" "CI and the local gate disagree"

# Verifies: ADR-0002
d="$(repo script-in-no-list)"
echo 'echo new' > "$d/scripts/check-new.sh"
expect fail "a script that neither list runs" "$d" "scripts/check-new.sh: no workflow and not scripts/check-all.sh runs it"
d="$(repo script-on-instruction)"
echo 'echo by hand' > "$d/scripts/sensor-by-hand.sh"
echo '# On instruction: scripts/sensor-by-hand.sh' >> "$d/scripts/check-all.sh"
expect pass "a script the local runner names as run on instruction" "$d"

d="$(repo other-workflow)"
printf 'on: [pull_request]\njobs:\n  again:\n    steps:\n      - run: bash scripts/check-docs.sh --strict\n' \
  > "$d/.github/workflows/again.yml"
expect pass "another workflow that runs a script the local runner runs, its job not required" "$d"
d="$(repo other-workflow-required)"
printf 'on: [pull_request]\njobs:\n  again:\n    steps:\n      - run: bash scripts/check-docs.sh\n' \
  > "$d/.github/workflows/again.yml"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
sed_in "$d/README.md" 's|^`docs`$|`docs`, `again`|'
expect fail "a required check that is a job of another workflow than checks.yml" "$d" \
  "required check 'again' is not a job of .github/workflows/checks.yml"
d="$(repo other-workflow-inline)"
printf 'on: [pull_request]\njobs:\n  again:\n    steps:\n      - run: echo inline\n' \
  > "$d/.github/workflows/again.yml"
expect fail "a job of another workflow that runs no script" "$d" \
  "again.yml: job 'again' runs no script under scripts/"

# build_job <dir> <run line> [local] — a toolchain job in the workflow, with a setup step before
# its run line, listed among the required checks; with 'local', the local runner invokes the
# build script too.
build_job() {
  local dir="$1" run="$2" local_gate="${3:-}"
  {
    echo "  build:"
    echo "    steps:"
    echo "      - uses: actions/setup-something@v1"
    echo "      - run: $run"
  } >> "$dir/.github/workflows/checks.yml"
  [[ "$local_gate" == "local" ]] \
    && echo 'run "Build, test, lint" scripts/check-build.sh' >> "$dir/scripts/check-all.sh"
  # shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
  sed_in "$dir/README.md" 's|^`docs`$|`docs`, `build`|'
}

d="$(repo build-job)"
build_job "$d" "bash scripts/check-build.sh" local
expect pass "a toolchain job that runs the build script after a setup step" "$d"

# The commands inline in the job, the local runner untouched: every list agrees, and the pre-push
# hook runs neither the build nor the tests.
d="$(repo build-job-inline)"
build_job "$d" "make test"
expect fail "a toolchain job with its commands inline and nothing in the local runner" "$d" "job 'build' runs no script under scripts/"

# release <dir> [script] — a workflow that publishes, its steps inline, and with a script, a
# step that runs it.
release() {
  printf 'on: [push]\njobs:\n  publish:\n    steps:\n      - run: make dist\n' \
    > "$1/.github/workflows/release.yml"
  [[ -z "${2:-}" ]] || echo "      - run: bash $2" >> "$1/.github/workflows/release.yml"
}

d="$(repo not-a-gate)"
release "$d"
echo '# Not a gate: .github/workflows/release.yml' >> "$d/scripts/check-all.sh"
expect pass "a publishing workflow the local runner names as no gate, its steps inline" "$d"
d="$(repo not-a-gate-unnamed)"
release "$d"
expect fail "a publishing workflow not named as no gate" "$d" \
  "release.yml: job 'publish' runs no script under scripts/"
d="$(repo not-a-gate-missing)"
echo '# Not a gate: .github/workflows/gone.yml' >> "$d/scripts/check-all.sh"
expect fail "a '# Not a gate:' line that names no workflow" "$d" \
  "'# Not a gate: .github/workflows/gone.yml' names no workflow"
d="$(repo not-a-gate-other-file)"
echo '# Not a gate: README.md' >> "$d/scripts/check-all.sh"
expect fail "a '# Not a gate:' line that names a file outside .github/workflows/" "$d" \
  "'# Not a gate: README.md' names no workflow"
d="$(repo not-a-gate-checks)"
echo '# Not a gate: .github/workflows/checks.yml' >> "$d/scripts/check-all.sh"
expect fail "a '# Not a gate:' line that names the required checks' workflow" "$d" \
  "names the workflow of the required checks"
d="$(repo not-a-gate-script)"
echo 'echo sign' > "$d/scripts/sign.sh"
release "$d" scripts/sign.sh
echo '# Not a gate: .github/workflows/release.yml' >> "$d/scripts/check-all.sh"
expect fail "a script a workflow that is no gate runs, and the local runner does not" "$d" \
  "does not run 'scripts/sign.sh', which a workflow runs"

# matrix_job <dir> <required names> — a toolchain job with a matrix in the workflow and the
# local runner, and the README's required checks extended by the names given.
# With 'flow', the strategy is written in flow style; with 'outputs', the job computes a matrix
# as an output and has none of its own.
matrix_job() {
  local dir="$1" names="$2" style="${3:-}"
  {
    echo "  build:"
    case "$style" in
      flow) echo "    strategy: { fail-fast: false, matrix: { os: [ubuntu-latest, macos-latest] } }" ;;
      outputs) echo "    outputs:"; echo "      matrix: \${{ steps.m.outputs.matrix }}" ;;
      *) echo "    strategy:"; echo "      matrix:"; echo "        os: [ubuntu-latest, macos-latest]" ;;
    esac
    echo "    steps:"
    echo "      - run: bash scripts/check-build.sh"
  } >> "$dir/.github/workflows/checks.yml"
  echo 'echo build' > "$dir/scripts/check-build.sh"
  echo 'run "Build, test, lint" scripts/check-build.sh' >> "$dir/scripts/check-all.sh"
  sed_in "$dir/README.md" "s|^\`docs\`\$|\`docs\`, $names|"
}

# Verifies: ADR-0002
d="$(repo matrix-runs)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
matrix_job "$d" '`build (ubuntu-latest)`, `build (macos-latest)`'
expect pass "a job with a matrix named by its check runs" "$d"
d="$(repo matrix-bare)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
matrix_job "$d" '`build`'
expect fail "a job with a matrix named by its bare name" "$d" \
  "required check 'build' is a job with a matrix"
d="$(repo matrix-flow)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
matrix_job "$d" '`build (ubuntu-latest)`, `build (macos-latest)`' flow
expect pass "a strategy in flow style named by its check runs" "$d"
d="$(repo matrix-output)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
matrix_job "$d" '`build`' outputs
expect pass "a job that computes a matrix as an output, named by its name" "$d"
d="$(repo matrix-none)"
build_job "$d" "bash scripts/check-build.sh" local
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
sed_in "$d/README.md" 's|`build`|`build (ubuntu-latest)`|'
expect fail "a check run named for a job without a matrix" "$d" \
  "names a check run of 'build', which has no matrix"

d="$(repo layout-stale)"
sed_in "$d/README.md" 's|^docs/adr/ |docs/gone/ |'
expect fail "a Project Layout entry that no longer exists" "$d" "is not a directory of this repository"

# Verifies: ADR-0001
d="$(repo layout-unnamed-top)"
echo "notes" > "$d/NOTES.md"
expect fail "a top-level file the Project Layout does not name" "$d" \
  "Project Layout does not name 'NOTES.md', a top-level part"
d="$(repo layout-glob)"
echo "echo" > "$d/scripts/check-one.sh"
echo 'run "One" scripts/check-one.sh' >> "$d/scripts/check-all.sh"
echo "      - run: bash scripts/check-one.sh" >> "$d/.github/workflows/checks.yml"
layout "$d" "scripts/check-*.sh" "scripts/sensor-*.sh"
expect fail "a Project Layout pattern that matches nothing" "$d" "lists 'scripts/sensor-*.sh', which matches nothing"
d="$(repo layout-plan-absent)"
layout "$d" "docs/plan/"
expect pass "docs/plan/ named while there is no open work" "$d"

# --- cases: drafts (checks 1, 2, 5, 6, 8, and 16) ---------------------------------------

# Verifies: ADR-0002
DRAFT="draft"
# draft_id <slug> — the draft identifier of a slug, assembled for the same reason as adr_id.
draft_id() { printf 'ADR-%s-%s' "$DRAFT" "$1"; }

# drafted <dir> — the draft 'next', proposed to supersede ADR 1, which stays accepted until the
# draft is, with both index rows and a link to the draft from the README.
drafted() {
  local dir="$1"
  adr "$dir" 1 "🟢 accepted"
  {
    echo "# $(draft_id next): the next decision"
    echo
    echo "- **Status:** 🟡 proposed"
    echo "- **Applies to:** everything"
    echo "- **Supersedes:** [$(adr_id 1)](0001-decision.md)"
    echo
    echo "## Decision"
    echo
    echo "We will."
  } > "$dir/docs/adr/$DRAFT-next.md"
  {
    echo "# Architecture Decision Records"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    index_row 1 "🟢 accepted"
    echo "| [$DRAFT-next]($DRAFT-next.md) | Next | everything | 🟡 proposed |"
  } > "$dir/docs/adr/README.md"
  echo "Decided in [$(draft_id next)](docs/adr/$DRAFT-next.md)." >> "$dir/README.md"
}

d="$(repo draft)"
drafted "$d"
GITHUB_BASE_REF=feat/topic expect pass "a draft, its index row, a link, and a supersession, landing on a feat/ branch" "$d"
GITHUB_BASE_REF="" GITHUB_ACTIONS="" expect pass "a draft outside CI, where nothing names the branch" "$d"
GITHUB_BASE_REF=main PULL_REQUEST_DRAFT=false \
  expect fail "a draft in a pull request into main ready for review" "$d" "$DRAFT-next.md: a draft lands on main"
GITHUB_BASE_REF=main PULL_REQUEST_DRAFT=true \
  expect pass "a draft in a draft pull request into main, a feat/ branch under rework" "$d"
GITHUB_BASE_REF="" GITHUB_ACTIONS=true GITHUB_REF_NAME=main PULL_REQUEST_DRAFT="" \
  expect fail "a draft in a push to main" "$d" "$DRAFT-next.md: a draft lands on main"

d="$(repo draft-ref-missing)"
drafted "$d"
echo "Also $(draft_id gone)." >> "$d/README.md"
GITHUB_BASE_REF="" expect fail "a draft identifier with no draft file" "$d" "reference '$(draft_id gone)' matches no ADR file"

d="$(repo draft-heading)"
drafted "$d"
sed_in "$d/docs/adr/$DRAFT-next.md" "1s/$(draft_id next)/$(draft_id other)/"
GITHUB_BASE_REF="" expect fail "a draft whose heading names another draft" "$d" "$DRAFT-next.md: heading says '# $(draft_id other)'"

d="$(repo draft-slug)"
drafted "$d"
adr "$d" 2 "🟡 proposed"
mv "$d/docs/adr/0002-decision.md" "$d/docs/adr/$DRAFT-Next_one.md"
GITHUB_BASE_REF="" expect fail "a draft whose slug is not lower case and hyphens" "$d" "$DRAFT-Next_one.md: a draft's slug"

d="$(repo draft-link)"
drafted "$d"
echo "See [$(draft_id next)](docs/adr/0001-decision.md)." >> "$d/README.md"
GITHUB_BASE_REF="" expect fail "a link that cites a draft and points at another ADR" "$d" "cites $(draft_id next) but points at '0001-decision.md'"

# --- summary -------------------------------------------------------------------------------

summary "Documentation self-test"
