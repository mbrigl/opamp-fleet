#!/usr/bin/env bash
#
# Applies a map of ADR identifiers, clauses, and criteria to every reference in the tree
# (NUC-decisions-are-proposed-then-accepted §8, NUC-releases-are-semver-tags §7).
#
# The map is a reviewed file of lines 'OLD NEW'. Blank lines and lines starting with '#' are
# ignored. An identifier is '<CAT>-<slug>'; a clause is written without a space, '<CAT>-<slug>§n'.
#
#     <CAT>-old-slug    <CAT>-new-slug
#     <CAT>-old-slug§3  <CAT>-other-slug§1
#     <CAT>-gone-slug   -
#     <CAT>-split-slug  <CAT>-first-half <CAT>-second-half
#     0004-old-title.md <CAT>-new-slug
#     G-3               G-1
#
# An identifier moves wherever it stands as a word: on its own, before a clause '§n', and in the
# name of its file, so a link moves with its text. A clause line moves only that clause, and the
# file of a link whose text cites it. A numbered ADR, the form a project carries until it migrates
# once, is mapped by its file name: its old identifier, its file name, and a row that links it move
# to the new identifier. NEW is '-' for an ADR or a criterion with no successor, and several
# identifiers for a split: their references are listed and left for the hand. On a line with a
# marker of the traceability check that already names a successor, the old identifier is dropped.
# A criterion, 'G-n' or 'Q-n', moves in Markdown and on a marker line; elsewhere it may be an
# ordinary string.
#
# Every match is looked up in the map once. Identifiers are never used twice
# (NUC-adrs-are-named-by-category §1), so a map whose successor is itself mapped is refused, and a
# second run of a map changes nothing. Criteria are renumbered by a normalization and may swap; a
# map of criteria is one pass. The script never edits a criterion in docs/SPECIFICATION.md, nor
# docs/adr/, whose record the normalization writes. With --record, for the migration to identifiers
# (NUC-decisions-are-proposed-then-accepted §8), it edits docs/adr/ too, the index and the headings
# among it, but never a NUC- file, the blob list, or AGENTS.md, which the release holds, and renames
# each numbered ADR file that the map moves to one identifier, refusing a name that exists or that
# two take.
#
# The files are the ones git tracks plus new ones it does not ignore, as check-docs.sh reads
# them. A file is written only when the map changes its text, and then keeps every other byte,
# a missing final newline included; a file with a NUL byte, which grep -I takes for binary, is
# skipped, and so is the map file itself, so that a map shipped in the tree applies as it stands.
#
# Pure bash, git, and awk. Usage, anywhere in the repository:
#     .agents/skills/normalize/remap.sh [--check] [--record] <map file>
# Prints each file it changed. Exit code 0 when every reference moved, 2 when references are left
# for the hand, 1 on anything else; an invalid map changes nothing. With --check it changes
# nothing, prints each reference the map would move as 'file:line: OLD -> NEW' and each rename as
# 'OLD -> NEW', lists the references it would leave, and exits 1 when one would move.
set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=../../../scripts/lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib/bash.sh"

# The script may rewrite its own text, so bash reads all of it before running any: one block.
{
check=0 record=0
while [[ "${1:-}" == --check || "${1:-}" == --record ]]; do
  [[ "$1" == --check ]] && check=1 || record=1
  shift
done
map="${1:-}"
[[ -n "$map" && -f "$map" ]] || { echo "remap: '$map' is no map file" >&2; exit 1; }
map="$(cd "$(dirname "$map")" && pwd -P)/$(basename "$map")"

top="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "remap: not inside a git repository" >&2; exit 1; }
cd "$top" || exit 1
# The map's path comes from pwd -P, so the top's does too: Git for Windows names it C:/…, where
# Git Bash names the map /c/…, and the map would not know itself.
top="$(pwd -P)"

id_re='[A-Z]{2,5}-[a-z0-9]+(-[a-z0-9]+){1,4}'
new_re="^$id_re(§[0-9]+)?\$"
num_re='^[0-9]{4}-[A-Za-z0-9._-]+\.md$'
crit_re='^[GQ]-[0-9]+$'

# Validate the whole map before touching a file, so that an invalid one changes nothing.
declare -A seen=() olds=()
news=()
errors=0
lineno=0
err() { echo "remap: map line $lineno: $1" >&2; errors=$((errors + 1)); }
while IFS= read -r line || [[ -n "$line" ]]; do
  lineno=$((lineno + 1))
  [[ "$line" =~ ^[[:space:]]*(#|$) ]] && continue
  read -r old new <<< "$line"
  read -ra targets <<< "${new:-}"
  if ((${#targets[@]} == 0)); then err "expected 'OLD NEW'"; continue; fi
  if [[ "$old" =~ $crit_re ]]; then
    [[ ${#targets[@]} -eq 1 && ( "$new" == - || ( "$new" =~ $crit_re && "${new:0:1}" == "${old:0:1}" ) ) ]] \
      || err "'$new' is no single '${old:0:1}-n' and no '-'"
  elif [[ "$old" =~ $num_re || "$old" =~ $new_re ]]; then
    if [[ "$new" != - ]]; then
      for t in "${targets[@]}"; do
        [[ "$t" =~ $new_re ]] || { err "'$t' is no identifier and no '-'"; continue; }
        [[ "$t" == *§* && ( "$old" != *§* || ${#targets[@]} -gt 1 ) ]] \
          && err "'$t' is a clause, which only a clause maps to, and alone"
        news+=("$t")
      done
    fi
    [[ "$old" == *§* && ${#targets[@]} -gt 1 ]] && err "a clause maps to one clause or to '-'"
    [[ "$old" =~ $num_re ]] && { key="${old%%-*}"; [[ -n "${seen[$key]:-}" ]] && err "$old: its number is mapped twice"; seen[$key]=1; }
    [[ "$old" =~ $new_re ]] && olds[$old]=1
  else
    err "'$old' is no identifier, clause, numbered ADR file, or criterion"; continue
  fi
  [[ -n "${seen[$old]:-}" ]] && err "$old is mapped twice"
  seen[$old]=1
done < "$map"
# A successor that is itself mapped, whole or by a clause, would move again on the next run.
for t in "${news[@]}"; do
  for o in "${!olds[@]}"; do
    if [[ "$t" == "$o" || "${t%%§*}" == "$o" || ( "$t" != *§* && "${o%%§*}" == "$t" ) ]]; then
      lineno="?"; err "$t is mapped and is a successor: an identifier is never used twice, so a map has no chain or cycle"
    fi
  done
done
((errors == 0)) || exit 1
((${#seen[@]})) || { echo "remap: the map is empty" >&2; exit 1; }

left="$(mktemp)"
moves="$(mktemp)"
trap 'rm -f "$left" "$moves" "$left".new' EXIT

# One awk program per file: the map first, then the file, rewritten match by match.
# adr_on and crit_on say which kinds of reference this file may have rewritten.
read -r -d '' program <<'AWK'
FNR == NR {
  if ($0 ~ /^[ \t]*(#|$)/) next
  old = $1; new = $2
  for (i = 3; i <= NF; i++) new = new " " $i
  if (old ~ /^[0-9][0-9][0-9][0-9]-/) {
    to = (new == "-" || new ~ / /) ? new : new ".md"
    fname[old] = to
    adr["ADR-" substr(old, 1, 4)] = new
    num[substr(old, 1, 4)] = new
  } else if (old ~ /^[GQ]-/) {
    crit[old] = new
  } else {
    adr[old] = new
  }
  next
}
# word(t) — the identifier of a target, without its clause.
function word(t) { sub(/§.*/, "", t); return t }
# has(id) — true when the original line names the identifier as a word.
function has(id,  s, at) {
  s = orig
  while ((at = index(s, id))) {
    if ((at == 1 || substr(s, at - 1, 1) !~ /[A-Za-z0-9_-]/) && substr(s, at + length(id), 1) !~ /[A-Za-z0-9_-]/) return 1
    s = substr(s, at + length(id))
  }
  return 0
}
# resolve(tok, to) — the replacement of tok mapped to 'to': itself when left, empty when a marker
# already names a successor of an ADR.
function resolve(tok, to,  n, parts, i, dedupe) {
  dedupe = marker && tok !~ /^[GQ]-/
  if (to == "-" || to ~ / /) {
    if (dedupe && to != "-") {
      n = split(to, parts, " ")
      for (i = 1; i <= n; i++) if (has(word(parts[i]))) return ""
    }
    print FILENAME ":" FNR ": " tok (to == "-" ? " has no successor" : " is split into " to) >> left
    return tok
  }
  if (dedupe && has(word(to))) return ""
  return to
}
{
  orig = $0; line = $0; out = ""
  marker = (line ~ /Verifies:/)
  do_crit = crit_on && (all_crit || marker)
  while (match(line, /\[[0-9][0-9][0-9][0-9]\]\([A-Za-z0-9._\/-]*[0-9][0-9][0-9][0-9]-[A-Za-z0-9._-]+\.md|ADR-[0-9][0-9][0-9][0-9]|[0-9][0-9][0-9][0-9]-[A-Za-z0-9._-]+\.md|[A-Z][A-Z]+-[a-z0-9]+(-[a-z0-9]+)*( §[0-9]+)?|[GQ]-[0-9]+/)) {
    at = RSTART; len = RLENGTH
    tok = substr(line, at, len)
    before = (at > 1) ? substr(line, at - 1, 1) : ""
    after = substr(line, at + len, 1)
    rep = tok
    if (before ~ /[A-Za-z0-9_-]/ || (after ~ /[A-Za-z0-9_-]/ && tok !~ /^\[/ && tok !~ /\.md$/)) {
      # part of a longer word
    } else if (tok ~ /^\[/) {
      n4 = substr(tok, 2, 4)
      inner = substr(tok, 8)
      match(inner, /[0-9][0-9][0-9][0-9]-[A-Za-z0-9._-]+\.md$/)
      dir = substr(inner, 1, RSTART - 1); file = substr(inner, RSTART)
      if (adr_on && (file in fname)) {
        to = fname[file]
        r = resolve(file, to)
        if (r != file && r != "") rep = "[" num[n4] "](" dir r
      }
    } else if (tok ~ /^ADR-[0-9][0-9][0-9][0-9]$/ || tok ~ /^[0-9][0-9][0-9][0-9]-/) {
      if (adr_on && tok ~ /^ADR-/ && (tok in adr)) rep = resolve(tok, adr[tok])
      else if (adr_on && (tok in fname)) rep = resolve(tok, fname[tok])
    } else if (tok ~ /^[GQ]-/) {
      if (do_crit && (tok in crit)) rep = resolve(tok, crit[tok])
    } else if (adr_on) {
      id = tok; clause = ""
      if (index(tok, " §")) { id = substr(tok, 1, index(tok, " §") - 1); clause = substr(tok, index(tok, " §") + 3) }
      if (clause != "" && ((id "§" clause) in adr)) {
        to = adr[id "§" clause]
        r = resolve(tok, to)
        if (r != tok) {
          rep = (r == "") ? "" : word(r) " §" substr(r, index(r, "§") + 2)
          # The file of a link whose text is this citation moves with it.
          rest = substr(line, at + len)
          if (r != "" && match(rest, "^\\]\\([A-Za-z0-9._/-]*" id "\\.md")) {
            seg = substr(rest, 1, RLENGTH)
            sub(id "\\.md$", word(r) ".md", seg)
            line = substr(line, 1, at + len - 1) seg substr(rest, RLENGTH + 1)
          }
        }
      } else if (id in adr) {
        r = resolve(id, adr[id])
        if (r != id) rep = (r == "" || clause == "") ? r : r " §" clause
      } else if (clause != "") {
        # only the identifier is a match; the clause after it is text
        len = length(id); tok = id; rep = id
      }
    }
    if (rep == "" && tok != "") {
      # A dropped identifier takes one separator with it: the one before, or after a colon the one after.
      pre = substr(line, 1, at - 1)
      post = substr(line, at + len)
      if (out pre ~ /:[ \t]*$/) sub(/^[ ,]+/, "", post)
      else sub(/[ ,]+$/, "", pre)
      line = pre post
      out = out ""
      # re-scan from where the identifier stood
      if (check) print FILENAME ":" FNR ": " tok " -> (dropped, its successor is named)" >> moves
      continue
    }
    if (rep != tok && check) print FILENAME ":" FNR ": " tok " -> " rep >> moves
    out = out substr(line, 1, at - 1) rep
    line = substr(line, at + len)
  }
  # A line is printed once the next one is read, so the last one can end as the file did.
  if (held) print last
  last = out line; held = 1
}
END { if (held) printf "%s%s", last, (open_end ? "" : "\n") }
AWK

# With --record, each numbered ADR the map moves to one identifier takes its file name, in the order
# of the old names; a name that exists or that two of them take is refused before any is written.
declare -A target=()
while ((record)) && read -r old new rest; do
  [[ "$old" =~ $num_re && "$new" =~ $new_re && -z "$rest" && -f "docs/adr/$old" ]] || continue
  [[ -e "docs/adr/$new.md" || " ${target[*]} " == *" $new "* ]] && { echo "remap: $old: docs/adr/$new.md is taken" >&2; exit 1; }
  target[$old]="$new"
done <<< "$(grep -vE '^[[:space:]]*(#|$)' "$map")"

changed=0
while IFS= read -r f; do
  [[ -f "$f" && ! -L "$f" && "$top/$f" != "$map" ]] || continue
  grep -Iq . "$f" 2>/dev/null || continue
  adr_on=1 crit_on=1 all_crit=0
  [[ "$f" == docs/adr/* && ( $record -eq 0 || "$f" == docs/adr/NUC[-.]* ) || $record -eq 1 && "$f" == AGENTS.md ]] && adr_on=0
  [[ "$f" == docs/SPECIFICATION.md ]] && crit_on=0
  [[ "$f" == *.md ]] && all_crit=1
  # A file whose last byte is no newline keeps it so, and awk reads bytes, not characters of the
  # locale, so a byte no character encodes passes through as it was.
  open_end=0
  [[ -n "$(tail -c 1 "$f")" ]] && open_end=1
  LC_ALL=C awk -v adr_on="$adr_on" -v crit_on="$crit_on" -v all_crit="$all_crit" -v left="$left" \
    -v moves="$moves" -v check="$check" -v open_end="$open_end" "$program" "$map" "$f" > "$left.new" || exit 1
  if ! cmp -s "$f" "$left.new"; then
    changed=1
    ((check)) && continue
    cat "$left.new" > "$f"
    echo "$f"
  fi
done < <(git ls-files --cached --others --exclude-standard | sort -u)

for old in $(printf '%s\n' "${!target[@]}" | LC_ALL=C sort); do
  changed=1 new="${target[$old]}"
  if ((check)); then echo "docs/adr/$old -> docs/adr/$new.md" >> "$moves"
  else git mv -- "docs/adr/$old" "docs/adr/$new.md" 2>/dev/null || mv -- "docs/adr/$old" "docs/adr/$new.md"; echo "docs/adr/$new.md"; fi
done
((check)) && cat "$moves"
if [[ -s "$left" ]]; then
  echo "remap: references left to rewrite by hand:" >&2
  cat "$left" >&2
fi
((check && changed)) && exit 1
[[ -s "$left" ]] && exit 2
exit 0
}
