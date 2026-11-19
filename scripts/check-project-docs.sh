#!/usr/bin/env bash
#
# Documentation checks of this project's own, beside the template's scripts/check-docs.sh.
#
# The rules below stood in scripts/check-docs.sh until the design revision of 2026-11-19 moved
# them here, so that the template's file is the template's again and the take-up of a template
# release replaces it whole. The template's checks offer no point where a project's rule runs on
# what they have parsed, so this script reads the documents again for itself: the text files of
# the repository and the Status line of each ADR. Each check is named by its rule key, which every
# finding it reports ends in, '[<key>:<reason>]', and which its function carries,
# 'check_<name>() {  # <key>', as in scripts/check-docs.sh, so that the binding ADRs sensor routes
# a change by it. The keys keep the area 'docs/' they had there.
#
#   docs/named-sections
#      Named-section integrity: in every text file, a section of README.md or CONTRIBUTING.md
#      named in prose names a heading of that document. The form read is the file name, an
#      arrow, and the heading in double quotes, as in 'README.md → "Usage"' or 'README → "Usage"'.
#      The document is the one beside the file, else the one in the root; a file name after
#      '../' is a path. A reference wrapped over two lines is joined, the comment marker of the
#      second line dropped. A heading in angle brackets is a placeholder and names nothing. A
#      heading in a fenced code block is not a heading. Not read: an unquoted or emphasized
#      heading; a quoted word before 'in README.md', which is as often a value as a heading; a
#      file name after a path other than '../', such as 'docs/README.md'; and a section named
#      by what it says instead of by its heading, such as 'the manual checklist in README.md'.
#      Superseded and rejected ADRs are not read.
#   docs/clause-citations
#      Clause citations: a citation of an ADR's clause names a numbered clause of that ADR's
#      Decision, of an ADR in force: a normalization re-cites them by its clause table, and a
#      citation that names no clause, or a clause of a dead ADR, is stale. The form read is
#      NUC-adrs-are-named-by-category §3's: an identifier, a space, and the section sign with the
#      number, and a list of them after the identifier, '§N, §M', '§N and §M', '§N, §M, and §K' or a
#      range '§N–§M', inside a Markdown link's text where it is one. A citation wrapped over two
#      lines is joined, the comment marker of the second line and a backslash ending the first
#      dropped. Each number, both ends of a range included, lies between 1 and the count of the
#      Decision's numbered items ('N. ' at the start of a line, outside fenced code, where a '## '
#      line ends no section either); a Decision that numbers none is not held. A citation of a
#      superseded or rejected ADR fails outside docs/adr/, whose ADRs keep the past they were
#      written against, and outside CHANGELOG.md, which records what held when each entry was
#      written. So does a citation of a project ADR by a place instead of a clause, which a
#      normalization's clause table cannot follow: 'point', 'paragraph', 'line', 'rule', 'item',
#      'bullet', 'step', 'section', the older word 'clause', or the section sign after a link,
#      before a number and after an identifier, and 'clause N of' an identifier. The base's own text
#      is the template's, and only its clause numbers are read. An identifier that names no ADR is
#      the rule docs/adr-refs' of scripts/check-docs.sh. Superseded and rejected ADRs are not read,
#      nor is the template.
#
# Pure bash + coreutils/grep/sed/awk/find, plus git to enumerate the repository's files, as
# scripts/check-docs.sh needs (NUC-checks-are-bash §1).
#
# Usage:
#     scripts/check-project-docs.sh [--root <dir>]
# The root defaults to this repository; scripts/test-check-project-docs.sh passes fixture
# repositories. Exit code 0 when all checks pass, 1 otherwise, and 2 on an argument it does not
# read or a root that is no directory.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"
# shellcheck source-path=SCRIPTDIR source=lib/check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/check.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/check-project-docs.sh [--root <dir>]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
rest_args "${BASH_SOURCE[0]}" "$usage" 0 "${ARGS[@]}"
ADR_DIR="$ROOT/docs/adr"
ADR_INDEX="$ADR_DIR/README.md"

# TEXT_FILES — every text file the repository consists of, absolute paths, sorted, collected as
# scripts/check-docs.sh collects them: tracked files plus new, not-yet-added ones, minus everything
# .gitignore excludes, and a plain walk outside a work tree. grep -I drops a binary file.
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
  ((${#paths[@]})) && mapfile -t TEXT_FILES < <(grep -Il -- . "${paths[@]}" 2>/dev/null)
}

# The record: ADR_CATEGORIES from the index (adr_categories), ADR_LIST every ADR file in the order
# adr_files gives, ADR_BY_ID the file each identifier names, and ADR_STATUS the Status line of each.
ADR_LIST=()
declare -A ADR_BY_ID=() ADR_STATUS=()
read_adrs() {
  local file
  adr_categories "$ADR_INDEX"
  [[ -d "$ADR_DIR" ]] || return 0
  mapfile -t ADR_LIST < <(adr_files "$ADR_DIR")
  for file in "${ADR_LIST[@]}"; do
    adr_key_of "$file"
    [[ -v ADR_BY_ID["$ADR_ID"] ]] || ADR_BY_ID["$ADR_ID"]="$file"
    ADR_STATUS["$file"]="$(adr_field_line "$file" Status)"
  done
}

# is_inactive_adr <file> — true for an ADR whose status is ⚪ superseded or 🔴 rejected, as
# scripts/check-docs.sh reads it: both describe a repository state that does not exist, so their
# references are not held against the tree.
is_inactive_adr() {
  [[ "$1" == "$ADR_DIR"/* ]] || return 1
  local status="${ADR_STATUS["$1"]-}"
  [[ "$status" == *⚪* || "$status" == *🔴* ]]
}

# heading_texts <file> — print the text of every ATX heading outside a fenced code block.
heading_texts() {
  awk '
    /^[[:space:]]*(```|~~~)/ { fence = !fence; next }
    !fence && match($0, /^#+[[:space:]]+/) {
      hashes = $0; sub(/[^#].*$/, "", hashes)
      if (length(hashes) > 6) next
      text = substr($0, RLENGTH + 1)
      sub(/[[:space:]]+#*[[:space:]]*$/, "", text)
      print text
    }
  ' "$1"
}

# The rule docs/named-sections (see the header). One awk pass over every text file that mentions
# either document prints each reference as file, line, the '../' before the name, the name, and the
# heading. A line is read joined to the next, whose comment marker is dropped, and a match counts on
# the line it starts on: a reference wrapped over two lines is read once, and whole. HEADING_TEXTS
# holds the headings of each document, newline-framed, read once.
declare -A HEADING_TEXTS=()
check_named_sections() {  # docs/named-sections
  local files=() f
  for f in "${TEXT_FILES[@]}"; do is_inactive_adr "$f" || files+=("$f"); done
  ((${#files[@]})) || return 0
  mapfile -t files < <(grep -lE 'README|CONTRIBUTING' -- "${files[@]}" 2>/dev/null)
  ((${#files[@]})) || return 0
  local file lineno up name heading rel doc
  while IFS=$'\x1f' read -r file lineno up name heading; do
    # '<Heading>' is the placeholder a sentence about these references writes, as '<CAT>' is
    # for an ADR's category (docs/links of scripts/check-docs.sh).
    [[ "$heading" == '<'*'>' ]] && continue
    rel="${file#"$ROOT"/}"
    f="${file%/*}"
    if [[ -n "$up" ]]; then
      doc="$f/$up$name.md"
      [[ -f "$doc" ]] && doc="$(cd "${doc%/*}" && pwd)/$name.md"
    elif [[ -f "$f/$name.md" ]]; then
      doc="$f/$name.md"
    else
      doc="$ROOT/$name.md"
    fi
    if [[ ! -f "$doc" ]]; then
      add_error "$rel:$lineno: \"$heading\" names a section of $up$name.md, which does not exist [docs/named-sections:document]"
      continue
    fi
    [[ -v HEADING_TEXTS["$doc"] ]] || HEADING_TEXTS["$doc"]=$'\n'"$(heading_texts "$doc")"$'\n'
    [[ "${HEADING_TEXTS["$doc"]}" == *$'\n'"$heading"$'\n'* ]] \
      || add_error "$rel:$lineno: \"$heading\" names no heading of ${doc#"$ROOT"/} [docs/named-sections:heading]"
  done < <(awk '
    BEGIN {
      doc = "(\\.\\./)*(README|CONTRIBUTING)(\\.md)?`?"
      arrow = "(^|[^A-Za-z0-9_./-])`?" doc "[[:space:]]*→[[:space:]]*\"[^\"]*\""
    }
    function refs(line, joined, file, n,   rest, off, m, d, h, up) {
      rest = joined; off = 0
      while (match(rest, arrow)) {
        if (off + RSTART > length(line)) return
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        d = m; sub(/[[:space:]]*→.*$/, "", d)
        h = m; sub(/^[^"]*"/, "", h); sub(/"$/, "", h)
        gsub(/`/, "", d)
        match(d, /(\.\.\/)*(README|CONTRIBUTING)(\.md)?$/); d = substr(d, RSTART)
        up = d; sub(/(README|CONTRIBUTING)(\.md)?$/, "", up)
        sub(/^(\.\.\/)*/, "", d); sub(/\.md$/, "", d)
        printf "%s\037%s\037%s\037%s\037%s\n", file, n, up, d, h
      }
    }
    function scan(line, following,   joined) {
      sub(/[[:space:]]+$/, "", line)
      sub(/^[[:space:]]*(\/\/[\/!]?|#+|--|\*+|>+)?[[:space:]]*/, "", following)
      joined = line " " following
      refs(line, joined, file, n)
    }
    FNR == 1 && NR > 1 { scan(prev, "") }
    { if (FNR > 1) scan(prev, $0); prev = $0; file = FILENAME; n = FNR }
    END { if (NR) scan(prev, "") }
  ' "${files[@]}")
}

# The rule docs/clause-citations (see the header). DECISION_CLAUSES holds, for each ADR's
# identifier, the numbers of its Decision's numbered items, space-framed, outside fenced code
# (ADR_FENCE_AWK), read once. One awk pass over every file that names an ADR prints each citation as
# file, line, the key, the word cited by ('clause' or a place), and its numbers, a range as 'N-M'; a
# line is read joined to the next, as in docs/named-sections.
declare -A DECISION_CLAUSES=()
check_clause_citations() {  # docs/clause-citations
  local files=() f
  for f in "${TEXT_FILES[@]}"; do
    [[ "$f" == "$ADR_DIR/template.md" ]] && continue
    is_inactive_adr "$f" || files+=("$f")
  done
  ((${#files[@]})) || return 0
  local cats
  cats="$(IFS='|'; echo "${ADR_CATEGORIES[*]}")"
  mapfile -t files < <(grep -lE -- "($cats)-[a-z0-9]" "${files[@]}" 2>/dev/null)
  ((${#files[@]})) || return 0
  if ((${#ADR_LIST[@]})); then
    while IFS=$'\x1f' read -r f n; do
      adr_key_of "$f"
      DECISION_CLAUSES["$ADR_ID"]="$n"
    done < <(awk "$ADR_FENCE_AWK"'
      FNR == 1 { if (NR > 1) printf "%s\037%s\n", file, n; file = FILENAME; n = " "; d = 0; fence_c = "" }
      { f = fenced($0) }
      !f && /^## / { d = ($0 ~ /^## Decision[[:space:]]*$/); next }
      d && !f && /^[0-9]+\. / { k = $1; sub(/\.$/, "", k); n = n (k + 0) " " }
      END { if (NR) printf "%s\037%s\n", file, n }
    ' "${ADR_LIST[@]}")
  fi
  local file lineno key word numbers rel target n cited end low high have items
  while IFS=$'\x1f' read -r file lineno key word numbers; do
    rel="${file#"$ROOT"/}"
    target="${ADR_BY_ID["$key"]-}"
    [[ -n "$target" ]] || continue
    if [[ "$word" != clause ]]; then
      # The base writes its own text, and a project never edits it (NUC-base-is-fixed §1).
      [[ "$rel" == CHANGELOG.md || "$key" == NUC-* ]] \
        || add_error "$rel:$lineno: $key is cited by $word, a place a normalization cannot follow — cite its clause as '$key §N' [docs/clause-citations:place]"
      continue
    fi
    if is_inactive_adr "$target" && [[ "$file" != "$ADR_DIR"/* && "$rel" != CHANGELOG.md ]]; then
      add_error "$rel:$lineno: $key is no longer in force — cite the clause of the ADR that is [docs/clause-citations:inactive]"
      continue
    fi
    have="${DECISION_CLAUSES["$key"]- }"
    read -r -a items <<< "$have"
    n=${#items[@]}
    # A Decision that numbers no clauses, as the fixtures of the template's checks write one, is
    # not held; every ADR of the record numbers its clauses (NUC-adrs-are-named-by-category §3).
    ((n > 0)) || continue
    for cited in $numbers; do
      low="${cited%-*}" high="${cited#*-}"
      for end in "$low" "$high"; do
        if [[ "$have" != *" $((10#$end)) "* ]]; then
          add_error "$rel:$lineno: $key §$end is cited, and its Decision has $n [docs/clause-citations:beyond]"
          [[ "$low" == "$high" ]] && break
        fi
      done
    done
  done < <(awk -v cats="$cats" '
    BEGIN {
      # An identifier of a known category; docs/adr-refs holds what it names.
      key = "(" cats ")-[a-z0-9]+(-[a-z0-9]+)+"
      # A separator is spelled out, never bare space, and no starred term stands inside a repeated
      # group: mawk then matches nothing.
      sep = "(,[[:space:]]|,[[:space:]]and[[:space:]]|,[[:space:]]or[[:space:]]|[[:space:]]and[[:space:]]"
      sep = sep "|[[:space:]]or[[:space:]]|[[:space:]]to[[:space:]]|[[:space:]]?\342\200\223[[:space:]]?)"
      list = "[0-9]+(" sep "[0-9]+)*"
      # The clause form: the identifier, a space, and the section sign before each number.
      sign = "\302\247"
      clause = key "[[:space:]]" sign "[0-9]+((,[[:space:]]|,[[:space:]]and[[:space:]]|[[:space:]]and[[:space:]]|\342\200\223)" sign "[0-9]+)*"
      # The words a citation names a place by, the older word clause among them, and the section sign
      # after a link, which leaves the link text of the identifier.
      word = "([Cc]lauses?|[Pp]oints?|[Pp]aragraphs?|[Ll]ines?|[Rr]ules?|[Ii]tems?|[Bb]ullets?"
      word = word "|[Ss]teps?|[Ss]ections?)"
      after = key "(\\]\\([^)]+\\))?,?[[:space:]]+\\(?" word "[[:space:]]?" list
      linked = key "\\]\\([^)]+\\),?[[:space:]]+\\(?" sign "[[:space:]]?" list
      before = "[Cc]lauses?[[:space:]]+" list "[[:space:]]+of[[:space:]]+\\[?" key
    }
    # numbers <text> — the numbers of a list, each range as N-M, joined by spaces.
    function numbers(text,   out) {
      gsub(sign, "", text)
      gsub(/[[:space:]]*(to|\342\200\223)[[:space:]]*/, "-", text)
      gsub(/,|and|or/, " ", text)
      out = ""
      while (match(text, /[0-9]+(-[0-9]+)?/)) {
        out = out (out == "" ? "" : " ") substr(text, RSTART, RLENGTH)
        text = substr(text, RSTART + RLENGTH)
      }
      return out
    }
    function emit(id, w, list) {
      printf "%s\037%s\037%s\037%s\037%s\n", file, n, id, w, numbers(list)
    }
    # whole <list> <rest> — the list without its last number where the text goes on as a number
    # does, as a date after a comma would: "clause 2, 2026-11-14" cites clause 2.
    function whole(list, rest) {
      if (rest ~ /^[-\/0-9]/) sub(/[^0-9]+[0-9]+$/, "", list)
      return list
    }
    function cites(line, joined,   rest, off, m, id, w, list) {
      rest = joined; off = 0
      while (match(rest, clause)) {
        if (off + RSTART > length(line)) break
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        match(m, key); id = substr(m, RSTART, RLENGTH)
        list = substr(m, RSTART + RLENGTH)
        emit(id, "clause", list)
      }
      rest = joined; off = 0
      while (match(rest, linked)) {
        if (off + RSTART > length(line)) break
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        match(m, key); id = substr(m, RSTART, RLENGTH)
        emit(id, "the section sign after its link", "")
      }
      rest = joined; off = 0
      while (match(rest, after)) {
        if (off + RSTART > length(line)) break
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        match(m, key); id = substr(m, RSTART, RLENGTH)
        # The word and the list follow the identifier, its link and its comma.
        w = m; sub(/^[A-Z]+-[^[:space:]]*[[:space:]]+\(?/, "", w)
        list = w; sub(/^[^0-9]+/, "", list)
        sub(/[[:space:]0-9].*$/, "", w); w = tolower(w); sub(/s$/, "", w)
        if (w == "clause") w = "the older word clause"
        emit(id, w, whole(list, rest))
      }
      rest = joined; off = 0
      while (match(rest, before)) {
        if (off + RSTART > length(line)) break
        m = substr(rest, RSTART, RLENGTH)
        off += RSTART + RLENGTH - 1; rest = substr(rest, RSTART + RLENGTH)
        match(m, key); id = substr(m, RSTART, RLENGTH)
        emit(id, "clause N of", "")
      }
    }
    # A line ending in a backslash, as a Rust string continues, joins as any other.
    function scan(line, following) {
      sub(/[[:space:]]*\\?[[:space:]]*$/, "", line)
      sub(/^[[:space:]]*(\/\/[\/!]?|#+|--|\*+|>+)?[[:space:]]*/, "", following)
      cites(line, line " " following)
    }
    FNR == 1 && NR > 1 { scan(prev, "") }
    { if (FNR > 1) scan(prev, $0); prev = $0; file = FILENAME; n = FNR }
    END { if (NR) scan(prev, "") }
  ' "${files[@]}")
}

collect_text_files
read_adrs
check_named_sections
check_clause_citations

if ((${#errors[@]} > 0)); then
  echo "Project documentation checks FAILED:"
  echo
  for e in "${errors[@]}"; do echo "  - $e"; done
  echo
  echo "${#errors[@]} problem(s) found."
  exit 1
fi

echo "Project documentation checks passed."
exit 0
