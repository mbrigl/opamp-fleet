#!/usr/bin/env bash
#
# Documentation consistency checks for this repository.
#
# Enforces — as CI, not just convention — the documentation rules from AGENTS.md and
# docs/adr/README.md:
#
#   1. ADR index integrity: every docs/adr/NNNN-*.md file and every draft docs/adr/draft-*.md
#      (ADR-0002) is listed in the index table in
#      docs/adr/README.md once, every index row points to an existing file, and the status and the
#      'Applies to' cell shown in the index match the **Status:** and **Applies to:** lines
#      inside each ADR — the index routes a reader from a change to the ADRs that bind it
#      (AGENTS.md, section 2), so a row that misstates what its ADR governs misroutes them.
#      A superseded or rejected row sits below the 'Superseded and rejected' heading and an
#      accepted or proposed one above it: the first table is what a session reads, and a
#      row in the wrong one is either a dead decision read as binding or a binding one
#      never read (docs/adr/README.md).
#   2. ADR numbering integrity: the ADR files run 0001..N without gaps, and every file's
#      '# ADR-NNNN' heading matches its filename (docs/adr/README.md). A draft is outside the
#      count: its slug is lower case letters and digits joined by hyphens, and its heading reads
#      '# ADR-draft-<slug>' for the file 'draft-<slug>.md' (ADR-0002).
#   3. Relative-link integrity: every relative Markdown link in every tracked .md file resolves
#      to a file or directory that exists. Links inside fenced code blocks are illustrations,
#      not claims about files on disk, and are skipped (here and in check 6); so is a target that
#      still carries the 'NNNN' placeholder of docs/adr/template.md; an optional link title
#      ('[text](file "title")') is not part of the path. Reference-style links
#      ('[text][label]') are not resolved — this repository uses inline links only.
#
#      Checks 3 to 6 skip ⚪ superseded and 🔴 rejected ADRs (see is_inactive_adr): both
#      describe a repository state that does not exist, so holding their references against the
#      current tree would demand that it still contain what the project decided against.
#   4. Section-reference integrity: every section reference matches a numbered '## N.' heading
#      in AGENTS.md: the section sign followed by a number in Markdown, YAML, and shell files
#      and in the git hooks, and the words 'AGENTS.md, section N' in every text file but
#      Markdown. AGENTS.md is the only numbered document in this repository; extend the check if
#      another one appears. The headings there must themselves run 1..N in order: a gap or a
#      repeat is a botched renumbering, and it is the only half of the renumbering hazard a check
#      can see (AGENTS.md, section 3).
#   5. ADR-reference integrity: every 'ADR-NNNN' reference (with actual digits) and every
#      'ADR-draft-<slug>' reference (with an actual slug) names an ADR file that exists in
#      docs/adr/ — anticipated follow-ups are described by topic, never by a number that does
#      not exist yet (docs/adr/README.md).
#   6. ADR link agreement: a Markdown link whose text cites an ADR points at that ADR's own
#      file — the identifier and the file it links to must name the same decision
#      (docs/adr/README.md).
#   7. ADR header hygiene: every ADR's '**Status:**' line carries exactly one legend emoji and the
#      word that belongs to it, and no ADR still contains the instruction paragraph from
#      docs/adr/template.md — recognized by a needle read off that template, so rewording it
#      cannot silently disable the check. Check 1 compares index and file emoji-to-emoji, so a
#      leftover list of alternatives and a word contradicting its emoji both survive it — and the
#      word is what a reader believes (docs/adr/README.md). Every header field is the list item
#      '- **<Name>:** …' of template.md, since scripts/lib/adr.sh reads only that form: a field
#      in another form would pass here and be skipped by the binding ADRs sensor.
#   8. Supersession agreement: once the new ADR is accepted, its '**Supersedes:**' field and the
#      '⚪ superseded by ADR-NNNN' status line of every old one it names — one, or several it
#      consolidates — name each other; the field names a draft by its identifier. While it is
#      proposed or rejected, no old one names it as its successor: only a human changes a status,
#      and the human flips the old lines when accepting the new one (docs/adr/README.md). A new
#      ADR superseded in its turn counts as accepted, so a chain stays whole. Check 1 cannot
#      see a missing half: it only mirrors the index against whatever status the file carries.
#      Every name of a Supersedes field counts, on whichever line it sits.
#   9. Check-list agreement: the jobs of .github/workflows/checks.yml and the required status
#      checks named between the 'required-checks' markers in README.md list the same jobs, and
#      the workflows under .github/workflows/ and scripts/check-all.sh run the same scripts. A
#      check that runs in only one of them is a gate somebody believes in and does not have
#      (AGENTS.md, section 5). A job that runs no script under scripts/ fails too: its commands
#      run in CI and never in the local gate; a workflow check-all.sh names on a '# Not a gate:'
#      line, one that publishes rather than checks, is exempt, while the scripts it runs still
#      count. So does a script under scripts/ that neither list runs, unless check-all.sh names
#      it on an '# On instruction:' line. A job with a matrix reports one check run per entry, so
#      the README names it by those, '<job> (<values>)', and never by its bare name.
#  10. Project Layout integrity: every path listed in the 'Project Layout' block of README.md
#      exists, a trailing '/' meaning a directory. The block is written by hand and nothing else
#      tells a reader that one of its lines has gone stale. A path with a '*' matches at least one
#      file, and docs/plan/ may be absent, since open work lives there only while there is some
#      (ADR-0006). The reverse holds for the top level only: every tracked top-level file and
#      directory is named, by itself or by a path under it. Below it the block is a tour of the
#      parts a newcomer needs, not a file listing.
#  11. Skill pointer integrity: every skill under .agents/skills/ (a directory with a SKILL.md)
#      has a symlink of the same name under .claude/skills/ that resolves to it, and every entry
#      under .claude/skills/ is such a symlink (ADR-0003). Claude Code reads only the pointer
#      directory, so a skill without its pointer is invisible to it and to no other agent —
#      nobody notices; a copy instead of a symlink is a second source that drifts, and a skill
#      that lives only in the pointer directory is one no other agent finds. Each message says
#      the command that fixes it. A repository with neither directory has nothing to check.
#  12. Inherited ADRs decided: once TEMPLATE-SETUP.md is gone, every ADR in force whose
#      '**Deciders:**' field names the NUC maintainer also names a decider of this project and is
#      not 🟡 proposed. An inherited ADR binds a derived project once its own maintainer has added
#      their name and accepted it, or it is superseded (docs/adr/README.md): an inherited ADR
#      nobody here decided is a rule nobody decided. A superseded or rejected one is not read.
#      While the setup file exists the decision is pending by design, and the check is silent.
#  13. Template identity replaced: once TEMPLATE-SETUP.md is gone, none of the template's
#      placeholders is left. Step 1 of the setup: the project name NUC in the README title, its
#      intro, its Why NUC section, and the Dev Container name, the template's repository in the
#      README badges, the copyright holder in LICENSE, the security contact in SECURITY.md, the
#      reporting contact in CODE_OF_CONDUCT.md, and a CODEOWNERS with no active rule or with its TODO. Step 3: the
#      '<Project Name>' title of the specification, the overview, the glossary, and the
#      conventions, a criterion of the specification still reading '…' or '<Quality', and the
#      scaffold's '<Part>', '<Term>', and '<Convention>' entries. Step 5: a 'TODO' as the Build,
#      Test, Lint, or Run command of the README, its Overview sentence, and its Usage TODO. A
#      placeholder that survives the setup describes the template, not the project. A file that
#      does not exist has nothing to check.
#  14. Template release named: README.md carries a '**Template release:**' line naming a
#      'vX.Y.Z' tag or 'unreleased' (ADR-0005). Tags do not travel with GitHub's template
#      mechanism, so the line is the one thing that tells a derived project which release of the
#      template it carries; a release moves it, and a project that takes a release up moves it.
#  15. Prose width: a line of Markdown prose is at most 100 columns, so a diff shows the sentence
#      that changed. Fenced code, front matter, table rows, headings, and ADR header fields are
#      exempt, and so is a line that cannot break, such as a link standing on a line of its own.
#      Superseded and rejected ADRs are not read, nor is a file .gitattributes marks
#      'linguist-generated', such as a changelog a tool writes. Indented code blocks are read as
#      prose.
#  16. No draft on main: a change that lands on main carries no draft ADR — a draft takes its
#      number on the branch whose pull request brings it there, by scripts/number-adrs.sh
#      (ADR-0002). The branch a change lands on is the one CI names: the base of a pull request,
#      else the branch a push runs on. A draft pull request passes, since it tests a feat/ branch
#      still under rework, and so does a run outside CI, where nothing names the branch; the
#      workflow says in PULL_REQUEST_DRAFT whether a pull request is a draft.
#
# Checks 5 and 6 read every text file of the repository, not a list of documentation extensions:
# docs/adr/README.md states that code may reference an ADR number, so a verifier restricted to
# documentation file types would leave the references most likely to go stale — those in source
# comments, which no reviewer reads alongside the ADR index — unchecked. Check 4 reads the
# section sign only in documentation file types on purpose: 'ADR-NNNN' and the words
# 'AGENTS.md, section N' mean one thing wherever they appear, but '§' in source code is an
# ordinary character (see is_doc_file), and flagging it there would make a project's own string
# literals fail this repository's CI.
#
# Pure bash + coreutils/grep/sed/awk/find, plus git to enumerate the repository's files — all present
# in the Dev Container base image, so running it adds no toolchain and no dependency that would
# require an ADR.
#
# Usage:
#     scripts/check-docs.sh [repository root]        (or: bash scripts/check-docs.sh)
# The root defaults to this repository; scripts/test-check-docs.sh passes fixture repositories.
# Exit code 0 when all checks pass, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"
# shellcheck source-path=SCRIPTDIR source=lib/check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/check.sh"

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)"
ADR_DIR="$ROOT/docs/adr"
ADR_INDEX="$ADR_DIR/README.md"


# The helpers below set a variable rather than print: a check calls them once per ADR or per
# index row, and a subshell each time made the script's cost grow with the record.

# status_emoji <text> — sets EMOJI to the first legend status emoji (🟢 🟡 🔴 ⚪) in the text, empty
# when there is none, and EMOJI_COUNT to how many the text carries.
EMOJI="" EMOJI_COUNT=0
status_emoji() {
  local text="$1" e before at=-1 rest
  EMOJI="" EMOJI_COUNT=0
  for e in 🟢 🟡 🔴 ⚪; do
    before="${text%%"$e"*}"
    [[ "$before" == "$text" ]] && continue
    if ((at < 0 || ${#before} < at)); then at=${#before}; EMOJI="$e"; fi
    rest="${text//"$e"/}"
    EMOJI_COUNT=$((EMOJI_COUNT + (${#text} - ${#rest}) / ${#e}))
  done
}

# trim <text> — sets TRIMMED to the text without leading and trailing whitespace.
TRIMMED=""
trim() {
  TRIMMED="${1#"${1%%[![:space:]]*}"}"
  TRIMMED="${TRIMMED%"${TRIMMED##*[![:space:]]}"}"
}

# key_of <file> — sets KEY to what follows 'ADR-' in the identifier of the ADR file, as adr_key
# prints it.
KEY=""
key_of() {
  local base="${1##*/}"
  case "$base" in
    draft-*) KEY="${base%.md}" ;;
    *) KEY="${base%%-*}" ;;
  esac
}

# The header of every ADR, read by one awk pass over the record instead of a grep per field, per
# ADR, and per check. Keyed by the ADR's path: the first line of each header field anywhere in the
# file, as adr_field_line reads it (ADR_STATUS, ADR_APPLIES); the Supersedes and Deciders fields
# with their continuation lines, since a header field wraps (ADR_SUPERSEDES, ADR_DECIDERS); the
# first '# ADR-' heading (ADR_HEADING); every header field written in another form, as
# adr_stray_fields reports it (ADR_STRAY); and whether the file still carries the template's
# instruction paragraph (ADR_NEEDLE). ADR_LIST is every ADR file in the order adr_files gives, ADR_BY_KEY the file each
# key names.
ADR_LIST=()
declare -A ADR_KNOWN=() ADR_BY_KEY=() ADR_STATUS=() ADR_APPLIES=() ADR_SUPERSEDES=() ADR_DECIDERS=() ADR_HEADING=() \
  ADR_STRAY=() ADR_NEEDLE=()
read_adr_headers() {
  local file field value
  [[ -d "$ADR_DIR" ]] || return 0
  mapfile -t ADR_LIST < <(adr_files "$ADR_DIR")
  ((${#ADR_LIST[@]})) || return 0
  for file in "${ADR_LIST[@]}"; do
    ADR_KNOWN["$file"]=1
    key_of "$file"
    [[ -v ADR_BY_KEY["$KEY"] ]] || ADR_BY_KEY["$KEY"]="$file"
  done
  while IFS=$'\x1f' read -r file field value; do
    case "$field" in
      Status) ADR_STATUS["$file"]="$value" ;;
      "Applies to") ADR_APPLIES["$file"]="$value" ;;
      Supersedes) ADR_SUPERSEDES["$file"]+="$value"$'\n' ;;
      Deciders) ADR_DECIDERS["$file"]+="$value"$'\n' ;;
      heading) ADR_HEADING["$file"]="$value" ;;
      stray) ADR_STRAY["$file"]+="$value"$'\n' ;;
      needle) ADR_NEEDLE["$file"]=1 ;;
    esac
  done < <(awk -v fields="$ADR_FIELDS" -v needle="$(instruction_needle)" '
    function out(k, v) { printf "%s\037%s\037%s\n", FILENAME, k, v }
    FNR == 1 { head = 1; cont = ""; split("", seen) }
    {
      if (head && /^## /) head = 0
      if (head && $0 ~ ("\\*\\*(" fields "):\\*\\*") && $0 !~ /^- \*\*/) out("stray", FNR ":" $0)
      if (needle != "" && !("needle" in seen) && index($0, needle)) { seen["needle"]; out("needle", 1) }
      if (!("heading" in seen) && /^# ADR-/) { seen["heading"]; out("heading", $0) }
      if (cont != "" && /^[[:space:]]+[^[:space:]]/) { out(cont, $0); next }
      cont = ""
      if (match($0, /^- \*\*[A-Za-z ]+:\*\*/)) {
        name = substr($0, 5, RLENGTH - 7)
        if (!(name in seen)) {
          seen[name]; out(name, $0)
          if (name == "Supersedes" || name == "Deciders") cont = name
        }
      }
    }
  ' "${ADR_LIST[@]}")
}

# adr_header <file> — sets HEADER_STATUS and HEADER_APPLIES to the Status and Applies to lines of
# the file: from the cache for an ADR of the record, read from the file for one outside it, such as
# a file an index row links that is no ADR.
HEADER_STATUS="" HEADER_APPLIES=""
adr_header() {
  if [[ -v ADR_KNOWN["$1"] ]]; then
    HEADER_STATUS="${ADR_STATUS["$1"]-}" HEADER_APPLIES="${ADR_APPLIES["$1"]-}"
  else
    HEADER_STATUS="$(adr_field_line "$1" Status)"
    HEADER_APPLIES="$(adr_field_line "$1" 'Applies to')"
  fi
}

# applies_to_of — sets TRIMMED to the content of the '**Applies to:**' line adr_header read,
# trimmed; empty when the line is missing or blank.
applies_to_of() { trim "${HEADER_APPLIES##*\*\*Applies to:\*\*}"; }

# TEXT_FILES — every text file the repository consists of, absolute paths, sorted. Collected once
# by collect_text_files below and then iterated by each check: building the list reads every
# file, so rebuilding it inside every check would multiply that cost by the number of checks, and
# a repository of a few thousand files makes that difference visible in CI.
#
# The list comes from git: tracked files plus new, not-yet-added ones, minus everything
# .gitignore excludes. That keeps generated trees (node_modules/, target/, dist/) out without
# this script having to guess the directory names of a toolchain this repository does not yet know.
# Binary files are dropped by grep -I, which never matches inside one.
# Outside a work tree (an exported tarball) git cannot answer, so fall back to a plain walk.
TEXT_FILES=()
collect_text_files() {
  local f paths=()
  while IFS= read -r f; do
    [[ -f "$f" ]] && paths+=("$f")
  done < <(
    if git -C "$ROOT" rev-parse --is-inside-work-tree > /dev/null 2>&1; then
      git -C "$ROOT" ls-files --cached --others --exclude-standard \
        | sed "s|^|$ROOT/|"
    else
      find "$ROOT" -type f -not -path '*/.git/*'
    fi | sort
  )
  # One grep for every file, in their order: -l names a file with a line, -I passes a binary one
  # over.
  ((${#paths[@]})) && mapfile -t TEXT_FILES < <(grep -Il -- . "${paths[@]}" 2>/dev/null)
  if ((${#TEXT_FILES[@]} == 0)); then
    add_error "no text files found under $ROOT — the file list is empty, so nothing was checked"
  fi
}

# is_doc_file <file> — true for the file types in which the section sign is a reference into
# AGENTS.md by convention: Markdown, YAML, shell scripts, and the git hooks. Elsewhere, in JSON
# and in source code, the section sign is an ordinary character with its own meanings — a statute
# cited in a German string literal, a translated message, a test fixture — none of which are
# claims about a section of AGENTS.md.
# The ADR checks scan every text file because 'ADR-NNNN' is unambiguous; this notation is not.
is_doc_file() {
  case "$1" in
    *.md | *.yml | *.yaml | *.sh | "$ROOT"/.githooks/*) return 0 ;;
    *) return 1 ;;
  esac
}

# is_inactive_adr <file> — true for an ADR whose status is ⚪ superseded or 🔴 rejected. Both are
# historical record rather than live decisions, and both describe a repository state that does not
# exist: a superseded ADR is frozen with the past it was written against (AGENTS.md,
# docs/adr/README.md), and a rejected one describes a road not taken — the check script, the
# workflow job, the AGENTS.md section it names were never created, or were removed together with
# the rejection. Verifying either one's references would demand that the tree still contain what
# the project decided against, so the reference checks below skip them.
is_inactive_adr() {
  [[ "$1" == "$ADR_DIR"/* ]] || return 1
  local status="${ADR_STATUS["$1"]-}"
  [[ "$status" == *⚪* || "$status" == *🔴* ]]
}

check_adr_index() {
  if [[ ! -f "$ADR_INDEX" ]]; then
    add_error "ADR index not found: docs/adr/README.md"
    return
  fi

  # ADR files on disk (basenames), numbered and drafts; the template is neither.
  local disk_files=()
  local f
  for f in "${ADR_LIST[@]}"; do disk_files+=("${f##*/}"); done

  # Parse the index into parallel arrays: filename, 'Applies to' cell, and status emoji per row.
  # Columns are '| ADR | Title | Applies to | Status |'; the cell is compared verbatim.
  local indexed_files=() indexed_applies=() indexed_status=()
  local line number target filename applies status archive=0
  local -a cells
  local row_re='^\|[[:space:]]*\[([0-9]{4}|draft-[^]]+)\]\(([^)]+)\)'
  while IFS= read -r line; do
    [[ "$line" =~ ^#+[[:space:]]+Superseded\ and\ rejected[[:space:]]*$ ]] && { archive=1; continue; }
    [[ "$line" =~ $row_re ]] || continue
    number="${BASH_REMATCH[1]}"
    target="${BASH_REMATCH[2]}"
    filename="${target%%#*}"
    # Cell 3 after the leading pipe, trimmed and compared verbatim.
    IFS='|' read -r -a cells <<< "$line"
    trim "${cells[3]-}"
    applies="$TRIMMED"
    status_emoji "$line"
    status="$EMOJI"
    indexed_files+=("$filename")
    indexed_applies+=("$applies")
    indexed_status+=("$status")
    if [[ "$number" == draft-* ]]; then
      [[ "$filename" == "$number.md" ]] \
        || add_error "ADR index: row for $number links to '$filename', not to '$number.md'"
    elif [[ "$filename" != "$number"-* ]]; then
      add_error "ADR index: row for $number links to '$filename', which does not start with '$number-'"
    fi
    case "$status" in
      ⚪|🔴)
        ((archive)) \
          || add_error "ADR index: row for $number is $status but sits above the 'Superseded and rejected' heading — move it there, a session reads the first table as binding"
        ;;
      🟢|🟡)
        ((archive)) \
          && add_error "ADR index: row for $number is $status but sits below the 'Superseded and rejected' heading — move it up, a binding decision there is never read"
        ;;
    esac
  done < "$ADR_INDEX"

  # A file listed twice is a row both sides of a merge changed (.githooks/merge-adr-index).
  local -A seen_rows=()
  for f in "${indexed_files[@]}"; do
    [[ -v seen_rows["$f"] ]] \
      && add_error "ADR index: lists '$f' twice — keep the row that matches the file, delete the other"
    seen_rows["$f"]=1
  done

  # Every file on disk must be listed.
  local d
  for d in "${disk_files[@]}"; do
    if [[ ! -v seen_rows["$d"] ]]; then
      add_error "ADR index: file '$d' exists but is not listed in the index"
    fi
  done

  # Every listed file must exist, and its status and 'Applies to' must match the file.
  local i file_status file_applies
  for i in "${!indexed_files[@]}"; do
    f="${indexed_files[$i]}"
    status="${indexed_status[$i]}"
    applies="${indexed_applies[$i]}"
    if [[ ! -f "$ADR_DIR/$f" ]]; then
      add_error "ADR index: lists '$f', but no such ADR file exists"
      continue
    fi
    adr_header "$ADR_DIR/$f"
    status_emoji "$HEADER_STATUS"
    file_status="$EMOJI"
    if [[ -z "$file_status" ]]; then
      add_error "$f: no '**Status:**' line with a status emoji found"
    elif [[ "$file_status" != "$status" ]]; then
      add_error "ADR index: status for '$f' is ${status:-<none>} in the index but $file_status in the file"
    fi
    applies_to_of
    file_applies="$TRIMMED"
    if [[ -z "$file_applies" ]]; then
      add_error "$f: no '**Applies to:**' line naming what the decision constrains"
    elif [[ "$file_applies" != "$applies" ]]; then
      add_error "ADR index: 'Applies to' for '$f' is '${applies:-<none>}' in the index but '$file_applies' in the file"
    fi
  done
}

# ADR numbers run 0001..N without gaps, and each file's heading carries its own number.
# Superseded ADRs stay on disk, so a gap can only ever mean a deleted ADR or a botched rename.
check_adr_numbering() {
  local f base number expected heading key n=0
  for f in "${ADR_LIST[@]}"; do
    base="${f##*/}"
    [[ "$base" == draft-* ]] && continue
    number="${base%%-*}"
    n=$((n + 1))
    printf -v expected '%04d' "$n"
    if [[ "$number" != "$expected" ]]; then
      add_error "ADR numbering: expected '$expected-*.md' at position $n, found '$base' — numbers must run 0001..N without gaps"
      break
    fi
    heading=""
    [[ "${ADR_HEADING["$f"]-}" =~ ^#\ ADR-[0-9]{4} ]] && heading="${BASH_REMATCH[0]}"
    if [[ "$heading" != "# ADR-$number" ]]; then
      add_error "$base: heading says '${heading:-<none>}', but the filename says ADR-$number"
    fi
  done

  # A draft stands outside the count; its slug and heading follow its file name (ADR-0002).
  for f in "${ADR_LIST[@]}"; do
    base="${f##*/}"
    [[ "$base" == draft-* ]] || continue
    key="${base%.md}"
    if [[ ! "$key" =~ ^draft-$ADR_SLUG_RE$ ]]; then
      add_error "$base: a draft's slug is lower case letters and digits joined by single hyphens"
      continue
    fi
    heading=""
    [[ "${ADR_HEADING["$f"]-}" =~ ^#\ $ADR_REF_RE ]] && heading="${BASH_REMATCH[0]}"
    if [[ "$heading" != "# ADR-$key" ]]; then
      add_error "$base: heading says '${heading:-<none>}', but the filename says ADR-$key"
    fi
  done
}

# Check 16 (see the header). lands_on — the branch CI says the change lands on: the base of a pull
# request, else the branch a push or a run by hand runs on; nothing outside CI.
lands_on() {
  if [[ -n "${GITHUB_BASE_REF:-}" ]]; then
    printf '%s\n' "$GITHUB_BASE_REF"
  elif [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then
    printf '%s\n' "${GITHUB_REF_NAME:-}"
  fi
}

check_no_draft_on_main() {
  [[ "$(lands_on)" == "main" ]] || return
  # A draft pull request tests a feat/ branch still under rework; marking it ready runs this again.
  [[ "${PULL_REQUEST_DRAFT:-}" == "true" ]] && return
  local f
  while IFS= read -r f; do
    add_error "$(basename "$f"): a draft lands on main — give it its number first with scripts/number-adrs.sh (ADR-0002)"
  done < <(find "$ADR_DIR" -maxdepth 1 -type f -name 'draft-*.md' | LC_ALL=C sort)
}

# instruction_needle — the opening of the instruction paragraph in docs/adr/template.md: the first
# line of its italic block, without the leading '*' and cut to a phrase. Reading the needle off the
# template instead of hard-coding a sentence is what keeps the check alive when the paragraph is
# reworded — which is what a project does with a template. Empty when the template is gone or
# carries no such block; the sub-check then does not run rather than reporting a phantom.
instruction_needle() {
  local line
  line="$(grep -m1 -E '^\*[^*]' "$ADR_DIR/template.md" 2>/dev/null)" || return 0
  printf '%s' "${line:1:40}"
}

# Every ADR's '**Status:**' line carries exactly one legend emoji plus the word that belongs to it,
# and no ADR still carries the instruction paragraph from template.md. check_adr_index reads the
# status as the first emoji status_emoji finds, so on its own it would accept a line that kept a
# list of alternatives ('🟡 proposed | 🟢 accepted | …') as whatever stands first, and a line whose word
# and emoji disagree passes there on the strength of the emoji — while the word is what a reader
# believes. A missing or emoji-less status line is left to check_adr_index, which reports it.
check_adr_headers() {
  local f base line expected word stray
  for f in "${ADR_LIST[@]}"; do
    base="${f##*/}"
    while IFS= read -r stray; do
      [[ -n "$stray" ]] || continue
      add_error "$base:${stray%%:*}: a header field not written as the list item '- **<Name>:** …' of template.md — every script reads only that form"
    done <<< "${ADR_STRAY["$f"]-}"
    line="${ADR_STATUS["$f"]-}"
    status_emoji "$line"
    if [[ -n "$EMOJI" ]]; then
      case "$EMOJI" in
        🟢) expected=accepted ;;
        🟡) expected=proposed ;;
        🔴) expected=rejected ;;
        ⚪) expected=superseded ;;
      esac
      read -r word _ <<<"${line#*"$EMOJI"}"
      if ((EMOJI_COUNT > 1)); then
        add_error "$base: status line carries $EMOJI_COUNT status emoji — keep exactly one"
      elif [[ "$word" != "$expected" ]]; then
        add_error "$base: status line reads '$EMOJI ${word:-<nothing>}', but $EMOJI means '$expected'"
      fi
    fi
    if [[ -v ADR_NEEDLE["$f"] ]]; then
      add_error "$base: still carries the instruction paragraph from template.md — delete it"
    fi
  done
}

# superseding_key <status line> — sets SUCCESSOR to the key, a four-digit number or
# 'draft-<slug>', that a '⚪ superseded by ADR-NNNN' status line names, empty when it names none.
# The citation may be plain or a Markdown link, so the bracket is optional (docs/adr/README.md
# asks for the link form).
SUCCESSOR=""
superseding_key() {
  local re="superseded by \\[?$ADR_REF_RE"
  SUCCESSOR=""
  [[ "$1" =~ $re ]] && SUCCESSOR="${BASH_REMATCH[1]}"
  return 0
}

# adr_refs <text> — sets REFS to every ADR identifier the text cites, in order.
REFS=()
adr_refs() {
  local rest="$1"
  REFS=()
  while [[ "$rest" =~ $ADR_REF_RE ]]; do
    REFS+=("${BASH_REMATCH[0]}")
    rest="${rest#*"${BASH_REMATCH[0]}"}"
  done
}

# A supersession is written into two files: the new ADR's '**Supersedes:**' field, from the
# proposal on, and the old ADR's status line, which the human flips when accepting the new one
# (docs/adr/README.md). Nothing else compares them — check_adr_index only mirrors the index
# against whatever status a file carries, so a half performed flip passes it. Both directions
# are verified here; a cited number that names no file is left to check_adr_refs, which reports
# it repository-wide. A Supersedes field wraps at the line width like any other header field,
# and a name on its second line counts.
check_adr_supersessions() {
  local f base nr line target target_file succ_file named accepted
  for f in "${ADR_LIST[@]}"; do
    base="${f##*/}"
    key_of "$f"
    nr="$KEY"
    line="${ADR_STATUS["$f"]-}"

    # Forward: once this one is accepted, every ADR it claims to supersede — one, or several
    # whose decisions it consolidates — must carry the matching status line. One superseded in
    # its turn was accepted before, and its own successor keeps the line it flipped.
    status_emoji "$line"
    accepted=0
    [[ "$EMOJI" == "🟢" || "$EMOJI" == "⚪" ]] && accepted=1
    adr_refs "${ADR_SUPERSEDES["$f"]-}"
    for target in "${REFS[@]}"; do
      target_file="${ADR_BY_KEY["${target#ADR-}"]-}"
      if [[ -n "$target_file" ]] && ((accepted)); then
        superseding_key "${ADR_STATUS["$target_file"]-}"
        [[ "$SUCCESSOR" == "$nr" ]] || add_error \
          "${target_file##*/}: $base supersedes it, but its status line does not say 'superseded by ADR-$nr'"
      fi
    done

    # Backward: a superseded ADR names its successor, and the successor claims it.
    superseding_key "$line"
    if [[ -z "$SUCCESSOR" ]]; then
      status_emoji "$line"
      [[ "$EMOJI" == "⚪" ]] && add_error "$base: status is ⚪ but the line names no superseding ADR"
    else
      succ_file="${ADR_BY_KEY["$SUCCESSOR"]-}"
      if [[ -n "$succ_file" ]]; then
        adr_refs "${ADR_SUPERSEDES["$succ_file"]-}"
        named=0
        for target in "${REFS[@]}"; do [[ "$target" == "ADR-$nr" ]] && named=1; done
        ((named)) || add_error "${succ_file##*/}: $base says it supersedes, but it carries no '**Supersedes:**' field naming ADR-$nr"
        # A successor superseded in its turn was accepted before: a chain of supersessions.
        status_emoji "${ADR_STATUS["$succ_file"]-}"
        [[ "$EMOJI" == "🟢" || "$EMOJI" == "⚪" ]] || add_error \
          "$base: superseded by ADR-$SUCCESSOR, which is not accepted — the status line flips when the human accepts ADR-$SUCCESSOR"
      fi
    fi
  done
}

# Print the GitHub-style slug of every ATX heading in a Markdown file. One awk pass per file: a
# pipeline per heading made this the most expensive part of the script.
extract_heading_slugs() {
  awk '
    match($0, /^#+[[:space:]]+/) {
      hashes = $0; sub(/[^#].*$/, "", hashes)
      if (length(hashes) > 6) next
      text = substr($0, RLENGTH + 1)
      # Drop a trailing run of "#" (closed ATX headings), then slugify the way GitHub does:
      # lowercase, remove everything but [a-z0-9 _-], then one hyphen per remaining space. A run
      # of spaces yields a run of hyphens: GitHub drops a character such as "&" and keeps the
      # spaces around it, so "Quality bar & Definition of Done" anchors as
      # "quality-bar--definition-of-done".
      sub(/[[:space:]]+#*[[:space:]]*$/, "", text)
      text = tolower(text)
      gsub(/[^a-z0-9 _-]+/, "", text)
      gsub(/[[:space:]]/, "-", text)
      print text
    }
  ' "$1"
}

# HEADING_SLUGS — the slugs of each Markdown file an anchor was looked up in, newline-framed, keyed
# by path: a file is linked from many others, and its headings are read once.
declare -A HEADING_SLUGS=()

# anchor_resolves <file> <fragment> — true if the fragment exists in the file.
# Handles GitHub line anchors (Lnn, Lnn-Lmm) for any file and heading-slug anchors for
# Markdown files; any other anchor is unverifiable from disk and is accepted.
anchor_resolves() {
  local file="$1" frag="$2" start end lines
  if [[ "$frag" =~ ^L([0-9]+)(-L([0-9]+))?$ ]]; then
    start="${BASH_REMATCH[1]}"; end="${BASH_REMATCH[3]:-$start}"
    lines="$(wc -l < "$file")"
    # +1 tolerates a final line with no trailing newline (uncounted by wc -l).
    (( start >= 1 && end >= start && end <= lines + 1 ))
    return
  fi
  case "$file" in
    *.md)
      [[ -v HEADING_SLUGS["$file"] ]] \
        || HEADING_SLUGS["$file"]=$'\n'"$(extract_heading_slugs "$file")"$'\n'
      [[ "${HEADING_SLUGS["$file"]}" == *$'\n'"$frag"$'\n'* ]] ;;
    *) return 0 ;;
  esac
}

# The Markdown of the repository, read by one awk pass over every file instead of a pipeline per
# file and per check. Keyed by the file's path:
#
#   MD_LINKS  every '[text](target)' expression outside a fenced code block, one per line. A fence
#             (``` or ~~~, also inside a blockquote) opens and closes on any line that starts with
#             one: links in fences are illustrations, not claims about files on disk.
#   MD_SECT   every line outside such a fence that carries the section sign, as '<line>:<text>'.
#   MD_WIDE   every line longer than 100 bytes outside the front matter and a fence as check 15
#             reads one, as '<line>:<text>': a fence there closes only on a line of the same
#             character, at least as long, and nothing else. Bytes are at least the characters,
#             so check 15 counts the characters of these lines alone.
declare -A MD_LINKS=() MD_SECT=() MD_WIDE=()
scan_markdown() {
  local files=() f kind file line value
  for f in "${TEXT_FILES[@]}"; do [[ "$f" == *.md ]] && files+=("$f"); done
  ((${#files[@]})) || return 0
  while IFS=$'\x1f' read -r kind file value; do
    case "$kind" in
      L) MD_LINKS["$file"]+="$value"$'\n' ;;
      S) MD_SECT["$file"]+="$value"$'\n' ;;
      W) MD_WIDE["$file"]+="$value"$'\n' ;;
    esac
  done < <(awk '
    function out(k, v) { printf "%s\037%s\037%s\n", k, FILENAME, v }
    FNR == 1 { fence = 0; wfence = ""; front = 0 }
    {
      if (/^[[:space:]>]*(```|~~~)/) fence = !fence
      else if (!fence) {
        rest = $0
        while (match(rest, /\[[^]]*\]\([^)]+\)/)) {
          out("L", substr(rest, RSTART, RLENGTH))
          rest = substr(rest, RSTART + RLENGTH)
        }
        if (index($0, "§")) out("S", FNR ":" $0)
      }

      if (FNR == 1 && $0 == "---") { front = 1; next }
      if (front) { if ($0 == "---") front = 0; next }
      if (wfence != "") {
        if (match($0, /^[[:space:]]*[`~]+[[:space:]]*$/)) {
          run = $0; gsub(/[[:space:]]/, "", run)
          if (length(run) >= 3 && index(run, wfence) == 1) wfence = ""
        }
        next
      }
      if (match($0, /^[[:space:]]*(```+|~~~+)/)) {
        wfence = substr($0, RSTART, RLENGTH); sub(/^[[:space:]]+/, "", wfence)
        if (length(wfence) >= 3) next
        wfence = ""
      }
      if (length($0) > 100) out("W", FNR ":" $0)
    }
  ' "${files[@]}")
}

# link_target <linkexpr> — sets LINK_TARGET to the target of one '[text](target)' expression:
# trimmed, with an optional trailing '"title"' or "'title'" removed (a link title is not part of
# the path). It sets a variable rather than printing, so that a caller reading every link of the
# repository forks no subshell per link.
LINK_EXPR_RE='^\[[^]]*\]\(([^)]+)\)$'
# LINK_RE — one '[text](target)' expression anywhere in a line, as the checks' grep -o reads it.
LINK_RE='\[[^]]*\]\([^)]+\)'
LINK_TITLE_RE="^([^[:space:]]+)[[:space:]]+(\"[^\"]*\"|'[^']*')\$"
LINK_TARGET=""
link_target() {
  local target="$1"
  [[ "$target" =~ $LINK_EXPR_RE ]] && target="${BASH_REMATCH[1]}"
  target="${target#"${target%%[![:space:]]*}"}"
  target="${target%"${target##*[![:space:]]}"}"
  [[ "$target" =~ $LINK_TITLE_RE ]] && target="${BASH_REMATCH[1]}"
  LINK_TARGET="$target"
}

check_relative_links() {
  local md rel linkexpr target path_part frag dir target_file
  for md in "${TEXT_FILES[@]}"; do
    case "$md" in *.md) ;; *) continue ;; esac
    rel="${md#"$ROOT"/}"
    is_inactive_adr "$md" && continue
    dir="${md%/*}"
    while IFS= read -r linkexpr; do
      [[ -n "$linkexpr" ]] || continue
      link_target "$linkexpr"
      target="$LINK_TARGET"
      case "$target" in
        http://*|https://*|mailto:*) continue ;;
      esac
      # 'NNNN' is the placeholder docs/adr/template.md uses where a real ADR carries its number,
      # so such a target names no file and claims none exists — the convention checks 5 and 6
      # follow by matching only references with actual digits.
      case "$target" in *NNNN*) continue ;; esac
      # Split into a path part and an optional '#fragment'. An empty path is a same-file anchor.
      path_part="${target%%#*}"
      frag=""
      [[ "$target" == *"#"* ]] && frag="${target#*#}"
      if [[ -z "$path_part" ]]; then
        target_file="$md"
      else
        target_file="$dir/$path_part"
        if [[ ! -e "$target_file" ]]; then
          add_error "$rel: broken relative link -> '$target'"
          continue
        fi
      fi
      # Validate the anchor fragment against the resolved file, when there is one.
      if [[ -n "$frag" && -f "$target_file" ]]; then
        anchor_resolves "$target_file" "$frag" \
          || add_error "$rel: link '$target' has no matching anchor '#$frag' in ${target_file#"$ROOT"/}"
      fi
    done <<< "${MD_LINKS["$md"]-}"
  done
}

# Section references point into AGENTS.md, whose sections are numbered '## N.' headings. Inserting
# or removing a section renumbers the ones after it, and a bare '§N' would keep pointing at
# whatever now carries that number — valid-looking and wrong. In Markdown a reference is therefore
# written as a link to the section's own anchor, which carries the number *and* the title
# ('AGENTS.md#3-adr-rules'): renumbering or retitling a section makes that anchor stop resolving,
# and check 3 fails on every reference to it. This check verifies the other half — that the number
# in the text and the anchor the link points at name the same section.
#
# Named gap (AGENTS.md, section 5): outside Markdown there are no links, so a '§N' in a workflow or
# a script comment is still only checked for the number existing. Those references live in files a
# maintainer edits next to AGENTS.md; the weaker guarantee is recorded here rather than hidden.
check_section_refs() {
  local agents="$ROOT/AGENTS.md"
  if [[ ! -f "$agents" ]]; then
    add_error "AGENTS.md not found in the repository root"
    return
  fi

  local valid_sections=()
  local n
  while IFS= read -r n; do
    valid_sections+=("$n")
  done < <(sed -nE 's/^## ([0-9]+)\..*/\1/p' "$agents")
  if ((${#valid_sections[@]} == 0)); then
    add_error "AGENTS.md: no numbered '## N.' section headings found"
    return
  fi

  # The numbering itself: 1..N in order, no gap and no repeat. A botched renumbering is what this
  # catches; that a reference still means the section it meant before is what the anchors do.
  local i expected
  for i in "${!valid_sections[@]}"; do
    expected=$((i + 1))
    if [[ "${valid_sections[$i]}" != "$expected" ]]; then
      add_error "AGENTS.md: numbered sections must run 1..N in order — expected '## $expected.' at position $expected, found '## ${valid_sections[$i]}.'"
      break
    fi
  done

  # The anchor each section carries, read with the same slug rule the link check uses.
  local section_slug=() slug
  while IFS= read -r slug; do
    [[ "$slug" =~ ^([0-9]+)- ]] || continue
    n="${BASH_REMATCH[1]}"
    _contains "$n" "${valid_sections[@]}" && section_slug[n]="$slug"
  done < <(extract_heading_slugs "$agents")

  local f rel lineno line clean linkexpr target frag rest
  local links=()
  for f in "${TEXT_FILES[@]}"; do
    rel="${f#"$ROOT"/}"
    is_inactive_adr "$f" && continue
    # The word form, 'AGENTS.md, section N', is unambiguous wherever it stands, so it is read in
    # every text file but Markdown, which cites a section as a link.
    if [[ "$f" != *.md ]]; then
      while IFS=: read -r lineno line; do
        n="${line##* }"
        _contains "$n" "${valid_sections[@]}" \
          || add_error "$rel:$lineno: reference 'AGENTS.md, section $n' matches no numbered section in AGENTS.md"
      done < <(grep -noE 'AGENTS\.md,? section [0-9]+' "$f")
    fi
    is_doc_file "$f" || continue

    case "$f" in
      *.md)
        # Only the lines that carry the section sign, outside a fence (scan_markdown).
        while IFS= read -r line; do
          [[ -n "$line" ]] || continue
          lineno="${line%%:*}"
          line="${line#*:}"
          # Inline code spans hold literals, not citations: '`§1`' in a sentence about references
          # is the notation itself. Dropping them also strips the '`AGENTS.md`' inside a link text,
          # which leaves the link expression itself intact. A span runs from a backtick to the
          # next one; an unpaired backtick stays.
          rest="$line" line=""
          while [[ "$rest" == *'`'*'`'* ]]; do
            line+="${rest%%\`*}"
            rest="${rest#*\`}"
            rest="${rest#*\`}"
          done
          line+="$rest"
          [[ "$line" == *§* ]] || continue
          # Split the line into its links and what is left once every link is removed, which is a
          # reference nothing verifies. Each match is the leftmost one, so cutting the text up to
          # its first occurrence cuts exactly the text up to the match.
          links=() clean="" rest="$line"
          while [[ "$rest" =~ $LINK_RE ]]; do
            links+=("${BASH_REMATCH[0]}")
            clean+="${rest%%"${BASH_REMATCH[0]}"*}"
            rest="${rest#*"${BASH_REMATCH[0]}"}"
          done
          clean+="$rest"
          for linkexpr in "${links[@]}"; do
            [[ "$linkexpr" =~ §([0-9]+) ]] || continue
            n="${BASH_REMATCH[1]}"
            if ! _contains "$n" "${valid_sections[@]}"; then
              add_error "$rel:$lineno: reference '§$n' matches no numbered section in AGENTS.md"
              continue
            fi
            link_target "$linkexpr"
            target="$LINK_TARGET"
            frag=""
            [[ "$target" == *"#"* ]] && frag="${target#*#}"
            if [[ -z "$frag" ]]; then
              add_error "$rel:$lineno: '§$n' is cited without an anchor — point the link at '#${section_slug[$n]}'"
            elif [[ "$frag" != "${section_slug[$n]}" ]]; then
              add_error "$rel:$lineno: '§$n' links to anchor '#$frag', but section $n of AGENTS.md anchors as '#${section_slug[$n]}'"
            fi
          done
          rest="$clean"
          while [[ "$rest" =~ §([0-9]+) ]]; do
            n="${BASH_REMATCH[1]}"
            rest="${rest#*"${BASH_REMATCH[0]}"}"
            if _contains "$n" "${valid_sections[@]}"; then
              add_error "$rel:$lineno: '§$n' stands outside a link — cite a section as a link to its anchor, e.g. [\`AGENTS.md\` §$n](AGENTS.md#${section_slug[$n]})"
            else
              add_error "$rel:$lineno: reference '§$n' matches no numbered section in AGENTS.md"
            fi
          done
        done <<< "${MD_SECT["$f"]-}"
        ;;
      *)
        while IFS=: read -r lineno line; do
          n="${line#§}"
          _contains "$n" "${valid_sections[@]}" \
            || add_error "$rel:$lineno: reference '§$n' matches no numbered section in AGENTS.md"
        done < <(grep -noE '§[0-9]+' "$f")
        ;;
    esac
  done
}

# Every 'ADR-NNNN' reference (with digits — the literal 'ADR-NNNN' placeholder never matches)
# must name an ADR file that already exists. Anticipated follow-up decisions are described by
# topic, not by a reserved number (docs/adr/README.md).
check_adr_refs() {
  local f files=() lineno match
  for f in "${TEXT_FILES[@]}"; do
    [[ "$f" == "$ADR_DIR/template.md" ]] && continue
    is_inactive_adr "$f" && continue
    files+=("$f")
  done
  ((${#files[@]})) || return 0
  # One grep over every file; --null ends each file name with a NUL, which no path carries.
  while IFS= read -r -d '' f && IFS=: read -r lineno match; do
    [[ -v ADR_BY_KEY["${match#ADR-}"] ]] \
      || add_error "${f#"$ROOT"/}:$lineno: reference '$match' matches no ADR file in docs/adr/"
  done < <(grep -Hno --null -E "$ADR_REF_RE" "${files[@]}")
}

# A Markdown link that cites 'ADR-NNNN' in its text must point at that ADR's own file. Both
# halves resolve on their own — the number names a file that exists, the target is a file that
# exists — so a renumbering, a copied line, or a consolidated decision set leaves the two naming
# different ADRs without any single check noticing. Only links into docs/adr/ are compared; a
# reference that deliberately points elsewhere (an index, a section about the decision) is left
# alone.
check_adr_link_targets() {
  local f rel linkexpr text target base number
  for f in "${TEXT_FILES[@]}"; do
    case "$f" in *.md) ;; *) continue ;; esac
    rel="${f#"$ROOT"/}"
    [[ "$f" == "$ADR_DIR/template.md" ]] && continue
    is_inactive_adr "$f" && continue
    while IFS= read -r linkexpr; do
      [[ -n "$linkexpr" ]] || continue
      # The text runs from the opening bracket to the first closing one, which it cannot contain.
      text="${linkexpr#\[}"
      text="${text%%]*}"
      [[ "$text" =~ $ADR_REF_RE ]] || continue
      number="${BASH_REMATCH[1]}"
      link_target "$linkexpr"
      target="${LINK_TARGET%%#*}"
      base="${target##*/}"
      # Compare only against ADR filenames; anything else is not a claim about which ADR it is.
      case "$base" in [0-9][0-9][0-9][0-9]-*.md | draft-*.md) ;; *) continue ;; esac
      if [[ "$number" == draft-* ]]; then
        [[ "$base" == "$number.md" ]]
      else
        [[ "$base" == "$number"-* ]]
      fi || add_error "$rel: link '$linkexpr' cites ADR-$number but points at '$base'"
    done <<< "${MD_LINKS["$f"]-}"
  done
}

# The three places that list this repository's checks have to agree: the jobs in
# .github/workflows/checks.yml, the invocations in scripts/check-all.sh, and the required status
# checks named in README.md. AGENTS.md, section 5 makes check-all.sh the local equivalent of the
# workflow, and the README list is what a maintainer configures the ruleset from — a check that
# runs in only one of the three is a gate somebody believes in and does not have. Both files say
# in their own header that a check added to one is added to the other in the same change; this is
# that sentence, verified.
check_check_lists() {
  local rel_wf=".github/workflows/checks.yml" rel_all="scripts/check-all.sh"
  local wf="$ROOT/$rel_wf" all="$ROOT/$rel_all" readme="$ROOT/README.md"
  local f
  for f in "$wf" "$all" "$readme"; do
    if [[ ! -f "$f" ]]; then
      add_error "${f#"$ROOT"/}: not found — the check lists cannot be compared"
      return
    fi
  done

  # The workflow: job names at two-space indent under 'jobs:', and the check script each job runs.
  # A job that runs no script under scripts/ — its commands written inline — is work CI does and
  # the local gate cannot: the toolchain job of a derived project, written that way, left the
  # pre-push hook without the build and the tests while the two lists still agreed.
  # Every workflow under .github/workflows/ counts for the scripts it runs; the jobs of checks.yml
  # alone are the required checks. A workflow that publishes rather than checks, such as a
  # release, builds and uploads with commands of its own that no push could run; the local runner
  # names it on a line '# Not a gate: .github/workflows/<file>', and its jobs may then run no
  # script, while the scripts they do run still count. checks.yml is the gate and cannot be named.
  local not_gate=() name
  while IFS= read -r name; do
    if [[ "$ROOT/$name" == "$wf" ]]; then
      add_error "$rel_all: '# Not a gate: $name' names the workflow of the required checks"
    elif [[ ! "$name" =~ ^\.github/workflows/[A-Za-z0-9._-]+\.ya?ml$ || ! -f "$ROOT/$name" ]]; then
      add_error "$rel_all: '# Not a gate: $name' names no workflow of this repository"
    fi
    not_gate+=("$name")
  done < <(sed -n 's/^#[[:space:]]*Not a gate:[[:space:]]*\([^[:space:]]*\).*/\1/p' "$all")
  local line job="" job_scripts=0 in_jobs=0 file rel is_checks is_gate key=""
  local wf_jobs=() wf_scripts=() matrix_jobs=()
  # job_done — report the job just parsed when none of its steps ran a script.
  job_done() {
    [[ -n "$job" ]] && ((is_gate && job_scripts == 0)) \
      && add_error "$rel: job '$job' runs no script under scripts/ — the local gate cannot run what it does"
    job=""
  }
  for file in "$ROOT"/.github/workflows/*.yml "$ROOT"/.github/workflows/*.yaml; do
    [[ -f "$file" ]] || continue
    rel="${file#"$ROOT"/}" in_jobs=0 job="" job_scripts=0 is_checks=0 is_gate=1
    [[ "$file" == "$wf" ]] && is_checks=1
    _contains "$rel" "${not_gate[@]}" && is_gate=0
    while IFS= read -r line; do
      [[ "$line" == "jobs:"* ]] && { in_jobs=1; continue; }
      ((in_jobs)) || continue
      if [[ "$line" =~ ^\ \ ([a-z][a-z0-9-]*):[[:space:]]*$ ]]; then
        job_done
        job="${BASH_REMATCH[1]}"
        job_scripts=0
        ((is_checks)) && wf_jobs+=("$job")
        key=""
        continue
      fi
      # A job with a matrix reports one check run per entry, '<job> (<values>)', never '<job>'.
      # The matrix is the one under the job's 'strategy:', in block or in flow style; a 'matrix'
      # elsewhere, such as an output that computes one, is no strategy of this job.
      [[ "$line" =~ ^\ {4}([a-z][a-z-]*): ]] && key="${BASH_REMATCH[1]}"
      if ((is_checks)) && [[ -n "$job" ]] \
        && { [[ "$key" == strategy && "$line" =~ ^\ {6}matrix: ]] \
          || [[ "$line" =~ ^\ {4}strategy:.*[{,][[:space:]]*matrix: ]]; }; then
        _contains "$job" "${matrix_jobs[@]}" || matrix_jobs+=("$job")
      fi
      [[ "$line" =~ run:[[:space:]]+bash[[:space:]]+(scripts/[A-Za-z0-9._-]+\.sh) ]] \
        && { wf_scripts+=("${BASH_REMATCH[1]}"); job_scripts=$((job_scripts + 1)); }
    done < "$file"
    job_done
  done
  if ((${#wf_jobs[@]} == 0)); then
    add_error "$rel_wf: no jobs found under 'jobs:' — the check lists cannot be compared"
    return
  fi

  # The local runner: every 'run <label> <script>' line.
  local all_scripts=()
  while IFS= read -r line; do
    [[ "$line" =~ ^[[:space:]]*run[[:space:]]+\"[^\"]*\"[[:space:]]+(scripts/[A-Za-z0-9._-]+\.sh) ]] \
      && all_scripts+=("${BASH_REMATCH[1]}")
  done < "$all"
  if ((${#all_scripts[@]} == 0)); then
    add_error "$rel_all: no 'run <label> <script>' invocations found — the check lists cannot be compared"
    return
  fi

  local s
  for s in "${wf_scripts[@]}"; do
    _contains "$s" "${all_scripts[@]}" \
      || add_error "$rel_all: does not run '$s', which a workflow runs — CI and the local gate disagree"
  done
  for s in "${all_scripts[@]}"; do
    _contains "$s" "${wf_scripts[@]}" \
      || add_error "$rel_all: runs '$s', which no job of a workflow runs — CI and the local gate disagree"
  done

  # Every script under scripts/ runs in the two lists, unless the local runner names it on a line
  # '# On instruction: scripts/<name>.sh', as for a sensor that reads GitHub (ADR-0002). A script
  # in neither list is a check nobody runs.
  local on_instruction=() name
  while IFS= read -r name; do on_instruction+=("$name"); done \
    < <(sed -n 's/^#[[:space:]]*On instruction:[[:space:]]*\(scripts\/[A-Za-z0-9._-]*\.sh\).*/\1/p' "$all")
  while IFS= read -r name; do
    s="scripts/${name##*/}"
    [[ "$s" == "$rel_all" ]] && continue
    _contains "$s" "${all_scripts[@]}" "${on_instruction[@]}" \
      || add_error "$s: no workflow and not $rel_all runs it — add it to both lists, or name it on an '# On instruction:' line of $rel_all"
  done < <(find "$ROOT/scripts" -maxdepth 1 -type f -name '*.sh' | LC_ALL=C sort)

  # The README: the check names fenced by the 'required-checks' markers, which a maintainer copies
  # into the ruleset. Rulesets name checks as they report: a job by its name, a job with a matrix
  # by one check run per entry, '<job> (<values>)'. A name that does not report is a required
  # check that never arrives and blocks every pull request. Which values a matrix has is the
  # workflow's; the check holds only the form.
  local in_block=0 readme_names=() readme_jobs=() base
  while IFS= read -r line; do
    [[ "$line" == *"required-checks begin"* ]] && { in_block=1; continue; }
    [[ "$line" == *"required-checks end"* ]] && { in_block=0; continue; }
    ((in_block)) || continue
    # shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
    while IFS= read -r name; do
      readme_names+=("$name")
    done < <(grep -oE '`[a-z][a-z0-9-]*( \([^`()]+\))?`' <<< "$line" | tr -d '`')
  done < "$readme"
  if ((${#readme_names[@]} == 0)); then
    add_error "README.md: no job names between the 'required-checks' markers — the required status checks are unverifiable"
    return
  fi

  for name in "${readme_names[@]}"; do
    base="${name%% (*}"
    readme_jobs+=("$base")
    if ! _contains "$base" "${wf_jobs[@]}"; then
      add_error "README.md: required check '$name' is not a job of $rel_wf — a ruleset would wait for a check that never reports"
    elif [[ "$name" == "$base" ]] && _contains "$base" "${matrix_jobs[@]}"; then
      add_error "README.md: required check '$name' is a job with a matrix, which reports as '$name (<values>)' — name each check run"
    elif [[ "$name" != "$base" ]] && ! _contains "$base" "${matrix_jobs[@]}"; then
      add_error "README.md: required check '$name' names a check run of '$base', which has no matrix and reports as '$base'"
    fi
  done
  for job in "${wf_jobs[@]}"; do
    _contains "$job" "${readme_jobs[@]}" \
      || add_error "README.md: job '$job' of $rel_wf is missing from the required checks — it would run without gating a merge"
  done
}

# The Project Layout block in README.md is a hand-written inventory, and nothing tells a reader
# that a line of it has gone stale: a renamed or deleted path leaves the block describing a
# repository that no longer exists. Every path it lists therefore has to exist, a trailing '/'
# meaning a directory.
#
# Only that direction is checked. The block is a tour of the parts a newcomer needs, not a listing
# of every file, so a path missing from it is an editorial judgment rather than a defect.
check_project_layout() {
  local readme="$ROOT/README.md"
  if [[ ! -f "$readme" ]]; then
    add_error "README.md: not found — the Project Layout block cannot be checked"
    return
  fi

  local line path in_section=0 in_fence=0 entries=0 listed=()
  while IFS= read -r line; do
    if ((!in_section)); then
      [[ "$line" == "## Project Layout" ]] && in_section=1
      continue
    fi
    if ((!in_fence)); then
      [[ "$line" =~ ^(\`\`\`|~~~) ]] && { in_fence=1; continue; }
      # A new heading before the fence means the section holds no block at all.
      [[ "$line" =~ ^##[[:space:]] ]] && break
      continue
    fi
    [[ "$line" =~ ^(\`\`\`|~~~) ]] && break
    path="${line%%[[:space:]]*}"
    # Blank lines and the continuation lines of a wrapped comment carry no path.
    [[ -z "$path" || "$path" == "#"* ]] && continue
    entries=$((entries + 1))
    listed+=("$path")
    if [[ "$path" == *"*"* ]]; then
      compgen -G "$ROOT/$path" >/dev/null \
        || add_error "README.md: Project Layout lists '$path', which matches nothing in this repository"
    elif [[ "$path" == */ ]]; then
      # Open work lives under docs/plan/ only while there is some (ADR-0006).
      [[ -d "$ROOT/${path%/}" || "$path" == "docs/plan/" ]] \
        || add_error "README.md: Project Layout lists '$path', which is not a directory of this repository"
    else
      [[ -f "$ROOT/$path" ]] \
        || add_error "README.md: Project Layout lists '$path', which does not exist"
    fi
  done < "$readme"

  if ((entries == 0)); then
    add_error "README.md: no paths found under '## Project Layout' — the inventory is unverifiable"
    return
  fi

  # The reverse: every top-level part of the tree is named, by itself or by a path under it.
  local top covered
  while IFS= read -r top; do
    covered=0
    for path in "${listed[@]}"; do
      [[ "$path" == "$top" || "$path" == "$top/"* ]] && { covered=1; break; }
    done
    ((covered)) || add_error "README.md: Project Layout does not name '$top', a top-level part of this repository"
  done < <(git -C "$ROOT" ls-files --cached 2>/dev/null | cut -d/ -f1 | LC_ALL=C sort -u)
}

# Check 11 (see the header). A pointer is resolved with 'cd -P', which every shell has, rather
# than realpath, whose flags differ between GNU and BSD.
check_skill_pointers() {
  local skills="$ROOT/.agents/skills" pointers="$ROOT/.claude/skills"
  local d name p target want
  for d in "$skills"/*/; do
    [[ -f "$d/SKILL.md" ]] || continue
    name="$(basename "$d")"
    p="$pointers/$name"
    if [[ -L "$p" ]]; then
      continue
    elif [[ -e "$p" ]]; then
      add_error ".claude/skills/$name: is a copy of the skill, not a symlink — a second source drifts; replace it: rm -r .claude/skills/$name && ln -s ../../.agents/skills/$name .claude/skills/$name"
    else
      add_error ".claude/skills/$name: no pointer — the skill .agents/skills/$name is invisible to Claude Code; add it: ln -s ../../.agents/skills/$name .claude/skills/$name"
    fi
  done
  for p in "$pointers"/*; do
    [[ -e "$p" || -L "$p" ]] || continue
    name="$(basename "$p")"
    if [[ ! -L "$p" ]]; then
      # A copy of an existing skill was reported above; what is left is a skill living here only.
      [[ -f "$skills/$name/SKILL.md" ]] \
        || add_error ".claude/skills/$name: is not a symlink into .agents/skills/ — a skill lives there, where every agent reads it, and is pointed at from here (ADR-0003)"
      continue
    fi
    if ! target="$(cd -P "$p" 2>/dev/null && pwd -P)"; then
      add_error ".claude/skills/$name: dangles — its target does not exist; point it at .agents/skills/$name or delete it"
      continue
    fi
    want="$(cd -P "$skills/$name" 2>/dev/null && pwd -P)"
    [[ -n "$want" && "$target" == "$want" && -f "$want/SKILL.md" ]] \
      || add_error ".claude/skills/$name: points at '${target#"$ROOT"/}', not at the skill .agents/skills/$name"
  done
}

# Check 12 (see the header). Inherited means the Deciders field still names the template's
# maintainer, on whichever of its lines; a derived project's own ADRs never do. Another decider
# is whatever is left of the field once that name, commas, and the word 'and' are removed.
check_inherited_adrs() {
  [[ -f "$ROOT/TEMPLATE-SETUP.md" ]] && return
  local f base deciders others status
  for f in "${ADR_LIST[@]}"; do
    deciders="${ADR_DECIDERS["$f"]-}"
    [[ "$deciders" == *"NUC maintainer"* ]] || continue
    status_emoji "${ADR_STATUS["$f"]-}"
    status="$EMOJI"
    [[ "$status" == "⚪" || "$status" == "🔴" ]] && continue
    base="${f##*/}"
    others="$(sed -E 's/^- \*\*Deciders:\*\*//; s/NUC maintainer//g; s/(^|[[:space:],])and([[:space:],]|$)/ /g; s/[[:space:],]//g' <<< "$deciders" | tr -d '\n')"
    if [[ -z "$others" ]]; then
      add_error "$base: inherited from the template and not decided by this project — add your name to Deciders and accept it, or supersede it (docs/adr/README.md)"
    elif [[ "$status" == "🟡" ]]; then
      add_error "$base: inherited from the template and still proposed after setup — flip the status to accepted, or supersede it (docs/adr/README.md)"
    fi
  done
}

# Check 13 (see the header). Each placeholder is a pattern in one file; the file is skipped when
# it does not exist, so a project that dropped one of them is not asked to edit it.
check_template_identity() {
  [[ -f "$ROOT/TEMPLATE-SETUP.md" ]] && return
  # placeholder <file> <grep -E pattern> <message>
  placeholder() {
    [[ -f "$ROOT/$1" ]] || return 0
    grep -qE "$2" "$ROOT/$1" && add_error "$1: $3"
  }
  placeholder README.md '^# NUC — an Agentic' "the title is still the template's; give the project its name"
  placeholder README.md '^\*\*NUC\*\* is named after' "the intro still explains the template's name; say what the project is"
  placeholder README.md '^## Why NUC$' "the Why NUC section still presents the template; delete it"
  placeholder README.md '^Describe what this project does' "the Overview is still the scaffold's sentence; describe the project"
  placeholder README.md '^TODO — show a minimal example' "the Usage section is still TODO; show how the project is used, or say there is nothing to use yet"
  # Anchored to a badge line: the Template section links the template's repository on purpose,
  # and that link stays. The name ends where a URL goes on, so that a derived repository named
  # after the template, as hivevm/nuc-tools, is not taken for it.
  placeholder README.md '^\[!\[[^]]*\]\([^)]*hivevm/nuc([/)?#&]|\.svg|$)' "a badge still points at the template's repository; repoint or delete it"
  placeholder .devcontainer/devcontainer.json '"name": "NUC DevContainer"' "the Dev Container still carries the template's name"
  placeholder LICENSE '^Copyright\b.*\bMaintainer\b' "the copyright holder is still the placeholder; name the maintainer"
  placeholder SECURITY.md 'TODO: add a security contact' "the security contact is still the placeholder"
  placeholder CODE_OF_CONDUCT.md 'TODO: add a contact address' "the reporting contact is still the placeholder"
  placeholder .github/CODEOWNERS '^# TODO: add a code owner' "the instruction to add a code owner is still there; remove it once the rules are active"
  if [[ -f "$ROOT/.github/CODEOWNERS" ]] && ! grep -qE '^[^#[:space:]]' "$ROOT/.github/CODEOWNERS"; then
    add_error ".github/CODEOWNERS: no active rule; name the code owner and uncomment the two rules"
  fi
  local doc
  for doc in SPECIFICATION ARCHITECTURE GLOSSARY CONVENTIONS; do
    placeholder "docs/$doc.md" '^# .*<Project Name>' "the title still carries '<Project Name>'; give the project its name"
  done
  placeholder docs/SPECIFICATION.md '\*\*[GQ]-[0-9]+\*\* — (…|<Quality)' "a criterion is still the scaffold's placeholder; write it, or delete the item"
  placeholder docs/ARCHITECTURE.md '^- \*\*<Part>\*\*' "the scaffold's building block is still listed; describe the parts, or delete the entry"
  placeholder docs/GLOSSARY.md '^- \*\*<Term>\*\*' "the scaffold's entry is still listed; write the first term, or delete the entry"
  placeholder docs/CONVENTIONS.md '^- \*\*<Convention>\*\*' "the scaffold's entry is still listed; write the first convention, or delete the entry"
  placeholder README.md '^- \*\*(Build|Test|Lint|Run):\*\* TODO' "a Build, Test, Lint, or Run command is still TODO; name the command the toolchain ADR chose"
}

# LINK_SPACE_RE — a space inside the text of a link; check 15 reads a link as one word.
LINK_SPACE_RE='(\[[^] ]*) ([^]]*\]\()'

# Check 15 (see the header). Characters are counted, not bytes, so an em dash is one column;
# scan_markdown hands over only the lines longer than 100 bytes outside front matter and fences.
check_prose_width() {
  local f rel n line indent body in_adr attr
  local LC_ALL=C.UTF-8
  # A file git marks as generated is written by a tool, not wrapped by hand; asked once for all.
  local -A generated=()
  while IFS= read -r attr; do
    case "$attr" in
      *": linguist-generated: set" | *": linguist-generated: true")
        generated["${attr%: linguist-generated: *}"]=1 ;;
    esac
  done < <(for f in "${!MD_WIDE[@]}"; do printf '%s\n' "${f#"$ROOT"/}"; done \
             | git -C "$ROOT" check-attr --stdin linguist-generated 2>/dev/null)
  for f in "${TEXT_FILES[@]}"; do
    [[ -v MD_WIDE["$f"] ]] || continue
    is_inactive_adr "$f" && continue
    rel="${f#"$ROOT"/}"
    [[ -v generated["$rel"] ]] && continue
    in_adr=0
    [[ "$f" == "$ADR_DIR"/* ]] && in_adr=1
    while IFS= read -r line; do
      [[ -n "$line" ]] || continue
      n="${line%%:*}"
      line="${line#*:}"
      ((${#line} > 100)) || continue
      # A table row, a heading, and an ADR header field are one line by their syntax.
      [[ "$line" =~ ^[[:space:]]*\| || "$line" =~ ^# ]] && continue
      [[ in_adr -eq 1 && "$line" =~ ^-\ \*\*[A-Za-z\ ]+:\*\* ]] && continue
      # A link is one word, whatever its text holds. A line without a space outside its links
      # cannot break; any other line can.
      indent="${line%%[! ]*}"
      body="${line:${#indent}}"
      while [[ "$body" =~ $LINK_SPACE_RE ]]; do
        body="${body/"${BASH_REMATCH[0]}"/"${BASH_REMATCH[1]}_${BASH_REMATCH[2]}"}"
      done
      [[ "$body" == *" "* ]] \
        && add_error "$rel:$n: ${#line} columns — wrap the prose at 100 (a link that does not fit stands on a line of its own)"
    done <<< "${MD_WIDE["$f"]}"
  done
}

# Check 14 (see the header). The first matching line is read; a Markdown list marker before the
# bold label is allowed, the version may be written as a link or in backticks, and what follows
# it — a sentence, a link target — is not read. After the version only a space, a punctuation
# mark, or the end of the line may follow: a pre-release suffix, build metadata, and a fourth
# number are not a tag ADR-0005 cuts, and SemVer allows no leading zero.
check_template_release() {
  local readme="$ROOT/README.md"
  [[ -f "$readme" ]] || return   # check 10 reports the missing README
  local line value
  line="$(grep -m1 -E '^[[:space:]]*([-*] )?\*\*Template release:\*\*' "$readme")"
  if [[ -z "$line" ]]; then
    add_error "README.md: no '**Template release:**' line — the Template section names the release of the template this repository carries, or 'unreleased' (ADR-0005)"
    return
  fi
  value="${line#*\*\*Template release:\*\*}"
  value="${value#"${value%%[![:space:]]*}"}"
  local num='(0|[1-9][0-9]*)'
  # shellcheck disable=SC2016  # the backticks are Markdown to match, not a command substitution
  local re='^[[`]?(v'"$num"'\.'"$num"'\.'"$num"'|unreleased)($|[][:space:],;:)`]|\.([[:space:]]|$))'
  [[ "$value" =~ $re ]] \
    || add_error "README.md: the Template release line names neither a 'vX.Y.Z' tag nor 'unreleased' — a release moves it to the tag it cuts (ADR-0005)"
}

collect_text_files
read_adr_headers
scan_markdown
check_adr_index
check_adr_numbering
check_no_draft_on_main
check_adr_headers
check_adr_supersessions
check_relative_links
check_section_refs
check_adr_refs
check_adr_link_targets
check_check_lists
check_project_layout
check_skill_pointers
check_inherited_adrs
check_template_identity
check_template_release
check_prose_width

if ((${#errors[@]} > 0)); then
  echo "Documentation checks FAILED:"
  echo
  for e in "${errors[@]}"; do echo "  - $e"; done
  echo
  echo "${#errors[@]} problem(s) found."
  exit 1
fi

echo "Documentation checks passed."
exit 0
