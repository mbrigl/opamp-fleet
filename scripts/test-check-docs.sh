#!/usr/bin/env bash
#
# Self-test of scripts/check-docs.sh. Verifies: NUC-documents-are-fixed NUC-procedures-are-skills
#
# Each case builds a throwaway git repository under a temporary directory — a rule file with
# numbered sections, a README with the Template release line, a CONTRIBUTING.md with a Project
# Layout block and the required-checks markers, a workflow and a local runner listing the same
# check, an ADR record with its index, and one skill with its pointer — runs the check against it,
# and asserts the exit code and, for a failing case, the key and the data of the violation. The
# baseline passes; every other case breaks one thing. The cases stand in the fixture table
# scripts/fixtures/check-docs-record.txt, which says its format, so that a port of the check is
# held to them. Every fixture has its setup done.
#
# Four cases stay here, since no row of the table can hold them: a reference in words to a
# section AGENTS.md lacks, twice, which the rule docs/section-refs would read in the table itself,
# and a git on PATH that cannot read the history or warns on stderr, twice. Section references
# and ADR identifiers inside these fixtures are assembled, never written literally, so that
# check-docs.sh does not read this script's own fixtures as references into this repository.
#
# Inputs: scripts/check-docs.sh scripts/lib/ scripts/fixtures/
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

# layout <dir> <path...> — name each path in the Project Layout block of the fixture's
# CONTRIBUTING.md, for a case that adds a top-level file for another reason (the rule
# docs/project-layout).
layout() {
  local dir="$1"; shift
  ADD="$(printf '%s\n' "$@")" awk '
    { print }
    $1 == "AGENTS.md" && !done { n = split(ENVIRON["ADD"], p, "\n"); for (i = 1; i <= n; i++) if (p[i] != "") print p[i] "  # added"; done = 1 }
  ' "$dir/CONTRIBUTING.md" > "$dir/CONTRIBUTING.md.tmp" && mv "$dir/CONTRIBUTING.md.tmp" "$dir/CONTRIBUTING.md"
}

# insert_before <file> <line prefix> <text> — insert text, where "\n" starts a new line, before
# the first line that starts with the prefix; awk reads "\n" the same way on every platform.
insert_before() {
  awk -v prefix="$2" -v text="$3" 'index($0, prefix) == 1 && !done { print text; done = 1 } { print }' \
    "$1" > "$1.tmp" && mv "$1.tmp" "$1"
}


# adr_id <n> — the identifier SEC-decision-<n>, assembled for the same reason.
adr_id() { printf '%s-decision-%s' SEC "$1"; }

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
  } > "$dir/docs/adr/$(adr_id "$n").md"
}

# index_row <number> <status emoji + word> — one row of the ADR index for the given ADR.
index_row() { echo "| [$(adr_id "$1")]($(adr_id "$1").md) | Decision $1 | everything | $2 |"; }

# role_adr <dir> <role> — the accepted ADR that fills the role, and role_row <role> its index row.
role_adr() {
  local cat=OPS
  printf '# %s: decision\n\n- **Status:** 🟢 accepted\n- **Role:** %s\n- **Applies to:** everything\n' \
    "$cat-fixture-$2" "$2" > "$1/docs/adr/$cat-fixture-$2.md"
}
role_row() { local cat=OPS; echo "| [$cat-fixture-$1]($cat-fixture-$1.md) | Decision | everything | 🟢 accepted |"; }
# ROLES — the required roles, read from the library.
read -r -a ROLES <<< "$(bash -c 'source "$1" && echo "${ADR_ROLES[*]}"' _ "$ROOT/scripts/lib/adr.sh")"

# repo <name> — a fixture repository that passes every check, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr" "$dir/scripts" "$dir/.github/workflows" \
           "$dir/.agents/skills/review" "$dir/.claude/skills"
  git -C "${dir:?}" init -q -b main

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
    echo "## Template"
    echo
    echo "- **Template release:** unreleased"
  } > "$dir/README.md"

  {
    echo "# Contributing"
    echo
    echo "## Project Layout"
    echo
    echo '```'
    echo "README.md             # for those who use it"
    echo "CONTRIBUTING.md       # this file"
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
  } > "$dir/CONTRIBUTING.md"

  {
    echo "jobs:"
    echo "  docs:"
    echo "    steps:"
    echo "      - run: bash scripts/check-docs.sh"
  } > "$dir/.github/workflows/checks.yml"

  echo 'run "Documentation consistency" scripts/check-docs.sh' > "$dir/scripts/check-all.sh"

  {
    echo "# <CAT>-<slug>: title"
    echo
    echo "- **Status:** 🟡 proposed"
    echo
    echo "*Complete the header and delete this paragraph.*"
  } > "$dir/docs/adr/template.md"

  adr "$dir" 1 "🟡 proposed"
  for r in "${ROLES[@]}"; do role_adr "$dir" "$r"; done
  {
    echo "# Architecture Decision Records"
    echo
    echo "| ADR | Title | Applies to | Status |"
    echo "|-----|-------|------------|--------|"
    index_row 1 "🟡 proposed"
    for r in "${ROLES[@]}"; do role_row "$r"; done
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
  out="$(bash "$CHECK" --root "$dir" 2>&1)"; rc=$?
  if output_holds "$out" "$needle" && [[ ( "$want" == "pass" && $rc -eq 0 ) || ( "$want" == "fail" && $rc -ne 0 ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected $want${needle:+ with needle \"$needle\"}, exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases: the words form of a section reference (docs/section-refs) -------------------------

# The words form is read in every text file but Markdown, the fixture table among them, so a case
# that names a section AGENTS.md lacks stays here, where the reference is assembled.

# words <n> — the word form of a section reference, assembled for the same reason as adr_id.
words() { printf 'AGENTS.md, section %s' "$1"; }

d="$(repo section-words)"
mkdir -p "$d/scripts/lib" && echo "# Required by $(words 9)." > "$d/scripts/lib/run.sh"
expect fail "a reference in words that names no section" "$d" "[docs/section-refs:unmatched] @ scripts/lib/run.sh:1 @ 9"

d="$(repo section-json-words)"
echo "{ \"note\": \"$(words 9)\" }" > "$d/settings.json"
expect fail "a reference in words in a JSON file" "$d" "[docs/section-refs:unmatched] @ settings.json:1 @ 9"

# --- cases read from the fixture table ----------------------------------------------------------

TABLE="$ROOT/scripts/fixtures/check-docs-record.txt"

# Verifies: NUC-adrs-are-named-by-category NUC-base-is-fixed NUC-checks-are-bash
# Verifies: NUC-releases-are-semver-tags NUC-template-learns-from-projects
# adr_file <id> — the file of an identifier; status_of <status> — the status line it reads.
adr_file() { echo "$1.md"; }
status_of() {
  case "$1" in
    accepted) echo "🟢 accepted" ;; proposed) echo "🟡 proposed" ;; rejected) echo "🔴 rejected" ;;
    superseded:*) echo "⚪ superseded by ${1#superseded:}" ;;
  esac
}

# table_text <text> — sets TEXT to the text of a step as the table reads it: each '\n' a line
# break, and each '<prose N>' N characters of 'word ' repeated.
table_text() {
  local re='<prose ([0-9]+)>' s
  TEXT="${1//\\n/$'\n'}"
  while [[ "$TEXT" =~ $re ]]; do
    s=""
    while ((${#s} < BASH_REMATCH[1])); do s+="word "; done
    TEXT="${TEXT/"${BASH_REMATCH[0]}"/"${s:0:BASH_REMATCH[1]}"}"
  done
}

# step <dir> <step> — apply one step of a tree to the fixture.
step() {
  local dir="$1" op id arg rest file key list item sep="" old new how
  local index="$1/docs/adr/README.md"
  read -r op id arg <<< "$2"
  rest="${2#"$op" }" rest="${rest#"$id" }"
  file="$(adr_file "$id")" key="$id"
  case "$op" in
    adr)
      step "$dir" "file $id $arg"
      case "$arg" in superseded:* | rejected) step "$dir" "row archive $id $arg" ;; *) step "$dir" "row binding $id $arg" ;; esac ;;
    file) printf '# %s: decision\n\n- **Status:** %s\n- **Applies to:** everything\n\n## Decision\n\nWe will.\n' \
            "$id" "$(status_of "$arg")" > "$dir/docs/adr/$file" ;;
    row)
      read -r _ _ id arg <<< "$2"
      file="$(adr_file "$id")" key="$id"
      rest="| [$key]($file) | Decision | everything | $(status_of "$arg") |"
      if [[ "$2" == "row binding "* ]]; then insert_before "$index" "### Superseded" "$rest"; else echo "$rest" >> "$index"; fi ;;
    field) insert_before "$dir/docs/adr/$file" "- **Applies to:**" "- **${arg%% *}:** ${rest#"${arg%% *}" }" ;;
    status) sed_in "$dir/docs/adr/$file" "s/^- \*\*Status:\*\* .*/- **Status:** $(status_of "$arg")/" ;;
    applies)
      # The file's field and the cell of its row, which the rule docs/adr-index holds alike.
      for item in "$dir/docs/adr/$file" "$index"; do
        ROW="| [$key]($file) |" NEW="$rest" awk '
          index($0, "- **Applies to:**") == 1 { $0 = "- **Applies to:** " ENVIRON["NEW"] }
          index($0, ENVIRON["ROW"]) == 1 && (at = index($0, "| everything |")) {
            $0 = substr($0, 1, at + 1) ENVIRON["NEW"] substr($0, at + 12) }
          { print }' "$item" > "$item.tmp" && mv "$item.tmp" "$item"
      done ;;
    supersedes)
      list="- **Supersedes:**"
      for item in ${arg//,/ }; do
        [[ "$item" == +* ]] && sep+="\n  " item="${item#+}"
        list+="$sep [$item]($(adr_file "$item"))" sep=","
      done
      insert_before "$dir/docs/adr/$file" "- **Applies to:**" "$list" ;;
    heading) sed_in "$dir/docs/adr/$file" "1s|.*|$rest|" ;;
    text) table_text "$rest" && mkdir -p "$(dirname "$dir/$id")" && printf '%s\n' "$TEXT" >> "$dir/$id" ;;
    write) table_text "$rest" && mkdir -p "$(dirname "$dir/$id")" && printf '%s' "$TEXT" > "$dir/$id" ;;
    edit)
      # ENVIRON, unlike -v, hands awk the texts as they are, a backslash included.
      if [[ "$rest" == *" =>" ]]; then old="${rest% =>}" new="" how=del; else old="${rest%% => *}" new="${rest#* => }" how=put; fi
      OLD="$old" NEW="$new" awk -v how="$how" '
        index($0, ENVIRON["OLD"]) == 1 && !done { done = 1; if (how == "put") print ENVIRON["NEW"]; next }
        { print }
        END { exit !done }' "$dir/$id" > "$dir/$id.tmp" \
        || { echo "fixture table: no line of $id starts with '$old'" >&2; exit 2; }
      mv "$dir/$id.tmp" "$dir/$id" ;;
    rm) rm -r "${dir:?}/$id" ;;
    link) mkdir -p "$(dirname "$dir/$id")" && ln -s "$arg" "$dir/$id" ;;
    layout) layout "$dir" "$id" ;;
    category) insert_before "$index" "### Binding" "- **$id** — a category of the project." ;;
    blobs) (cd "$dir/docs/adr" && bash -c 'source "$1" && adr_blobs .' _ "$ROOT/scripts/lib/adr.sh") > "$dir/docs/adr/NUC.blobs" ;;
    release) step "$dir" "edit README.md - **Template release:** => - **Template release:** ${2#release }" ;;
    setup) echo "# Template setup" > "$dir/TEMPLATE-SETUP.md" && layout "$dir" TEMPLATE-SETUP.md ;;
    commit) g "$dir" add -A && g "$dir" commit -q -m "$op" ;;
    delete) g "$dir" rm -q "docs/adr/$file" && sed_in "$index" "/^| \[$key\]/d" && step "$dir" commit \
              && DELETED="$(g "$dir" rev-parse --short HEAD)" ;;
    branch) g "$dir" switch -q -c "$id" ;;
    back) g "$dir" switch -q - ;;
    rename) g "$dir" branch -q -m "$id" ;;
    merge) g "$dir" merge -q --no-ff -m "$op" "$id" ;;
    skill) printf '%b' "${2#skill }" > "$dir/.agents/skills/review/SKILL.md" ;;
    *) echo "fixture table: unknown step '$op'" >&2; exit 1 ;;
  esac
}

# g <dir> <git args...> — git in a fixture, committing as a fixed author.
g() { local dir="$1"; shift; fixture "$dir"; git -C "$dir" -c user.name=t -c user.email=t@t -c commit.gpgsign=false "$@"; }

n=0
while IFS= read -r line; do
  [[ "$line" =~ ^(docs/[a-z0-9-]+|all)\ \| ]] || continue
  n=$((n + 1))
  line="$(table_hyphens "$line")"
  IFS=$'\x1f' read -r rule want needle name tree <<< "${line// | /$'\x1f'}"
  tree="${tree//$'\x1f'/ | }"
  [[ "$tree" != - ]] || tree=""
  DELETED=""
  d="$(repo "record-$n")"
  rm "$d/docs/adr/$(adr_id 1).md"
  printf '# ADRs\n\n### Categories\n\n- **NUC** — the template.\n\n### Binding\n\n%s\n%s\n%s\n### Superseded and rejected\n\n%s\n%s\n' \
    "| ADR | Title | Applies to | Status |" "|-----|-------|------------|--------|" "$(for r in "${ROLES[@]}"; do role_row "$r"; done)" \
    "| ADR | Title | Applies to | Status |" "|-----|-------|------------|--------|" > "$d/docs/adr/README.md"
  while IFS= read -r s; do [[ -z "$s" ]] || step "$d" "$s"; done <<< "${tree//; /$'\n'}"
  needle="${needle//<deleted>/$DELETED}"
  git -C "${d:?}" add -A
  out="$(bash "$CHECK" --root "$d" 2>&1)"
  table_case "$rule: $name" "$want" "$needle" "$?" "$out"
done < "$TABLE"
((n > 0)) || { echo "FAIL: no case read from $TABLE" >&2; failed=$((failed + 1)); }

# A git that cannot read the history, as one before 2.31 rejects --diff-merges, fails the rule
# docs/adr-identifiers.
d="$(repo old-git)"
g "$d" add -A && g "$d" commit -q -m base
mkdir -p "$work/old-git-bin"
printf '#!/usr/bin/env bash
[[ " $* " == *" --diff-merges="* ]] && { echo "fatal: unrecognized argument: --diff-merges=first-parent" >&2; exit 128; }
exec %q "$@"
' \
  "$(command -v git)" > "$work/old-git-bin/git"
chmod +x "$work/old-git-bin/git"
PATH="$work/old-git-bin:$PATH" expect fail "docs/adr-identifiers: a git too old to read the history" "$d" \
  "[docs/adr-identifiers:history] @ --diff-merges=first-parent"

# A git that warns on stderr and succeeds: the warning, here the name of an ADR in the tree, is no
# deleted ADR of the rule docs/adr-identifiers.
d="$(repo warn-git)"
g "$d" add -A && g "$d" commit -q -m base
mkdir -p "$work/warn-git-bin"
printf '#!/usr/bin/env bash
[[ " $* " == *" --diff-filter=D "* ]] && echo "docs/adr/%s.md" >&2
exec %q "$@"
' "$(adr_id 1)" "$(command -v git)" > "$work/warn-git-bin/git"
chmod +x "$work/warn-git-bin/git"
PATH="$work/warn-git-bin:$PATH" expect pass "docs/adr-identifiers: a warning of git on stderr is no deleted ADR" "$d"

# --- the rule keys: one place names each check ----------------------------------------------

# The header of check-docs.sh lists each check by its rule key, one item a line of its own,
# and is the one place that maps a key to its check (see that header). The keys the functions
# carry are the items, one each, and every key the script prints is an item or named in the
# header's prose, as docs/files is.
header_items() { sed -nE '/^set /q; s/^#   ([a-z]+\/[a-z0-9-]+)( \(was: [^)]+\))?$/\1/p' "$1"; }
header_olds() { sed -nE '/^set /q; s/^#   [a-z]+\/[a-z0-9-]+ \(was: ([^)]+)\)$/\1/p' "$1" | tr -s ', ' '\n'; }
function_keys() { sed -nE 's/^check_[a-z_]+\(\) \{  # ([a-z]+\/[a-z0-9-]+)$/\1/p' "$1" | sort; }
# shellcheck disable=SC2317,SC2329 # run by check
fails() { ! "$@" 2> /dev/null; }
# keys_listed <script> — every key the script prints in '[<key>:' stands in its header.
# shellcheck disable=SC2317,SC2329 # run by check
keys_listed() {
  local header key
  header="$(sed '/^set /q' "$1")"
  while IFS= read -r key; do
    [[ "$header" =~ (^|[^a-z/-])"$key"([^a-z0-9-]|$) ]] || { echo "  unlisted key: $key" >&2; return 1; }
  done < <(grep -oE '\[[a-z]+/[a-z0-9-]+:' "$1" | sed -E 's/^\[//; s/:$//' | sort -u)
}
# own_keys <script> — each check prints only the key its function carries, so that a key leads
# to the check that reports it.
# shellcheck disable=SC2317,SC2329 # run by check
own_keys() {
  local stray
  stray="$(awk '/^check_[a-z_]+\(\) \{  # / { key = $NF; next } /^}/ { key = "" }
    key != "" { s = $0; while (match(s, /\[[a-z]+\/[a-z0-9-]+:/)) {
      k = substr(s, RSTART + 1, RLENGTH - 2); if (k != key) print key " prints " k
      s = substr(s, RSTART + RLENGTH) } }' "$1")"
  [[ -z "$stray" ]] || { echo "  $stray" >&2; return 1; }
}
check "every check of check-docs.sh carries a rule key" test "$(grep -cE '^check_.*\{$' "$CHECK")" = 0
check "the keys of the checks are the items of the header, one each" \
  test "$(function_keys "$CHECK")" = "$(header_items "$CHECK" | sort)"
check "no key of the header is listed twice" \
  test "$(header_items "$CHECK" | sort | uniq -d)" = ""
check "no old key of the header is a key or listed twice" \
  test "$( { header_items "$CHECK"; header_olds "$CHECK"; } | sort | uniq -d)" = ""
check "the header carries docs/adr-numbering as the old key of docs/adr-identifiers" \
  grep -qx '#   docs/adr-identifiers (was: docs/adr-numbering)' "$CHECK"
check "every key check-docs.sh prints stands in its header" keys_listed "$CHECK"
sed 's/^check_prose_width() {.*/&\n  add_error "x [docs\/unlisted-rule:new]"/' "$CHECK" > "$work/unlisted.sh"
check "a key the script prints and its header lacks fails" \
  fails keys_listed "$work/unlisted.sh"
check "each check of check-docs.sh prints only its own key" own_keys "$CHECK"
sed 's/^check_prose_width() {.*/&\n  add_error "x [docs\/links:new]"/' "$CHECK" > "$work/stray.sh"
check "a check that prints the key of another fails" fails own_keys "$work/stray.sh"

# --- summary -------------------------------------------------------------------------------

summary "Documentation self-test"
