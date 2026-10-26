#!/usr/bin/env bash
#
# Applies the map of a normalization to every reference in the tree (ADR-0001).
#
# The map is a file of lines 'OLD NEW'. Blank lines and lines starting with '#' are ignored.
#
#     0004-old-title.md 0002-new-title.md
#     0009-merged-in.md 0002-new-title.md
#     0011-no-heir.md   -
#     G-3               G-1
#
# An ADR is mapped by its file name: the file name and its 'ADR-NNNN' move together. Several old
# ADRs may map to one new one, which is how a merge is written. A criterion is mapped by its
# identifier, 'G-n' or 'Q-n'. NEW is '-' for an ADR or a criterion with no successor.
#
# Every match is looked up in the map once, so a swap of two numbers comes out swapped and no new
# number is mapped a second time. 'ADR-NNNN', an ADR's file name, and the number in the text of a
# link to that file, as in an index row, are rewritten in every text file. The tree carries them
# only as references: the self-tests assemble the ones in their fixtures at run time. A criterion
# identifier is rewritten in Markdown and on a line that carries a marker of the traceability
# check; elsewhere it may be an ordinary string. The script
# never edits the record a map describes, because the normalization writes that record new: no
# ADR identifier is rewritten under docs/adr/, and no criterion in docs/SPECIFICATION.md.
#
# The files are the ones git tracks plus new ones it does not ignore, as check-docs.sh reads
# them. A reference to an ADR or a criterion mapped to '-' is left in place and listed, for the
# normalization to rewrite by hand.
#
# Pure bash, git, and awk. Usage, anywhere in the repository:
#     .agents/skills/normalize/remap.sh <map file>
# Prints each file it changed. Exit code 0 when every reference moved, 2 when references to
# something mapped to '-' are left, 1 on anything else. An invalid map changes nothing.
set -uo pipefail

map="${1:-}"
[[ -n "$map" && -f "$map" ]] || { echo "remap: '$map' is no map file" >&2; exit 1; }
map="$(cd "$(dirname "$map")" && pwd)/$(basename "$map")"

top="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "remap: not inside a git repository" >&2; exit 1; }
cd "$top" || exit 1

adr_file_re='^[0-9]{4}-[A-Za-z0-9._-]+\.md$'
crit_re='^[GQ]-[0-9]+$'

# Validate the whole map before touching a file, so that an invalid one changes nothing.
declare -A seen=()
errors=0
lineno=0
while IFS= read -r line || [[ -n "$line" ]]; do
  lineno=$((lineno + 1))
  [[ "$line" =~ ^[[:space:]]*(#|$) ]] && continue
  read -r old new extra <<< "$line"
  if [[ -n "${extra:-}" || -z "${new:-}" ]]; then
    echo "remap: map line $lineno: expected 'OLD NEW'" >&2; errors=$((errors + 1)); continue
  fi
  if [[ "$old" =~ $adr_file_re ]]; then
    [[ "$new" == - || "$new" =~ $adr_file_re ]] \
      || { echo "remap: map line $lineno: '$new' is no ADR file name and no '-'" >&2; errors=$((errors + 1)); }
  elif [[ "$old" =~ $crit_re ]]; then
    [[ "$new" == - || ( "$new" =~ $crit_re && "${new:0:1}" == "${old:0:1}" ) ]] \
      || { echo "remap: map line $lineno: '$new' is no '${old:0:1}-n' and no '-'" >&2; errors=$((errors + 1)); }
  else
    echo "remap: map line $lineno: '$old' is no ADR file name and no criterion" >&2; errors=$((errors + 1)); continue
  fi
  if [[ -n "${seen[${old%%-*}]:-}" && "$old" =~ $adr_file_re ]]; then
    echo "remap: map line $lineno: ADR-${old%%-*} is mapped twice" >&2; errors=$((errors + 1))
  elif [[ -n "${seen[$old]:-}" ]]; then
    echo "remap: map line $lineno: $old is mapped twice" >&2; errors=$((errors + 1))
  fi
  seen[$old]=1
  [[ "$old" =~ $adr_file_re ]] && seen[${old%%-*}]=1
done < "$map"
((errors == 0)) || exit 1
((${#seen[@]})) || { echo "remap: the map is empty" >&2; exit 1; }

left="$(mktemp)"
trap 'rm -f "$left" "$left".new' EXIT

# One awk program per file: the map first, then the file, rewritten match by match.
# adr_on and crit_on say which kinds of reference this file may have rewritten.
read -r -d '' program <<'AWK'
FNR == NR {
  if ($0 ~ /^[ \t]*(#|$)/) next
  old = $1; new = $2
  if (old ~ /^[0-9][0-9][0-9][0-9]-/) {
    fname[old] = new
    adr["ADR-" substr(old, 1, 4)] = (new == "-") ? "-" : "ADR-" substr(new, 1, 4)
  } else {
    crit[old] = new
  }
  next
}
{
  line = $0; out = ""
  do_crit = crit_on && (all_crit || line ~ /Verifies:/)
  while (match(line, /\[[0-9][0-9][0-9][0-9]\]\([A-Za-z0-9._\/-]*[0-9][0-9][0-9][0-9]-[A-Za-z0-9._-]+\.md|ADR-[0-9][0-9][0-9][0-9]|[0-9][0-9][0-9][0-9]-[A-Za-z0-9._-]+\.md|[GQ]-[0-9]+/)) {
    at = RSTART; len = RLENGTH
    tok = substr(line, at, len)
    before = (at > 1) ? substr(line, at - 1, 1) : ""
    rep = tok
    if (tok ~ /^\[/) {
      inner = substr(tok, 8)
      match(inner, /[0-9][0-9][0-9][0-9]-[A-Za-z0-9._-]+\.md$/)
      dir = substr(inner, 1, RSTART - 1); file = substr(inner, RSTART)
      if (adr_on && (file in fname))
        rep = (fname[file] == "-") ? "-" : "[" substr(fname[file], 1, 4) "](" dir fname[file]
    } else if (tok ~ /^ADR-/) {
      if (adr_on && (tok in adr)) rep = adr[tok]
    } else if (tok ~ /^[0-9]/) {
      if (adr_on && (tok in fname)) rep = fname[tok]
    } else if (do_crit && before !~ /[A-Za-z0-9_-]/ && (tok in crit)) {
      rep = crit[tok]
    }
    if (rep == "-") {
      print FILENAME ":" FNR ": " (tok ~ /^\[/ ? file : tok) " has no successor" >> left
      rep = tok
    }
    out = out substr(line, 1, at - 1) rep
    line = substr(line, at + len)
  }
  print out line
}
AWK

while IFS= read -r f; do
  [[ -f "$f" && ! -L "$f" ]] || continue
  grep -Iq . "$f" 2>/dev/null || continue
  adr_on=1 crit_on=1 all_crit=0
  [[ "$f" == docs/adr/* ]] && adr_on=0
  [[ "$f" == docs/SPECIFICATION.md ]] && crit_on=0
  [[ "$f" == *.md ]] && all_crit=1
  awk -v adr_on="$adr_on" -v crit_on="$crit_on" -v all_crit="$all_crit" -v left="$left" \
    "$program" "$map" "$f" > "$left.new" || exit 1
  if ! cmp -s "$f" "$left.new"; then
    cat "$left.new" > "$f"
    echo "$f"
  fi
done < <(git ls-files --cached --others --exclude-standard | sort -u)

if [[ -s "$left" ]]; then
  echo "remap: references left to rewrite by hand:" >&2
  cat "$left" >&2
  exit 2
fi
