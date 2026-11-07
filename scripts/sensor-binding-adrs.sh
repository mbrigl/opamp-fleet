#!/usr/bin/env bash
#
# Sensor: the ADRs that bind a change (ADR-0001, AGENTS.md section 2).
#
# Lists the ADRs in force — accepted or proposed — whose '**Applies to:**' line names a path a
# change touches. A path is what that line writes in backticks with no space in it, such as
# `docs/plan/`, `README.md`, or `Makefile`; it names itself and everything under it, and a file of
# a language that lays out a module's children in the directory of the file's name names that
# directory too (MODULE_DIRS below): a Rust file `x.rs` names `x/`, so that a module split off a
# file stays bound by the ADRs that name the file. A word in
# backticks that is no path, such as a skill's name, matches no changed path unless one carries
# that name, so it needs no filter. The rest of the line is prose no script reads ("every
# accepted ADR", "the pull request template"), so the list is the ADRs a session reads at the
# least, not all it reads: the index is still read by its prose. A path stands for the whole
# file even where the ADR governs a part of it, such as one section of the README, so the list
# also names an ADR whose part the change did not touch. It reports and never fails, so that the
# implementing session and the reviewer of AGENTS.md section 5 start from the same list.
#
# Pure bash + git. Usage:
#     scripts/sensor-binding-adrs.sh [--root <dir>] [--sections] [--path <path>]... [base [head]]
# It reads the repository it lives in, as the checks do; --root names another, as its self-test
# does with each fixture. With --sections it prints after the list what a session reads of the
# record (AGENTS.md section 2): of each ADR listed its header and the sections ADR_RULE_SECTIONS
# of scripts/lib/adr.sh names, its Decision and its Enforcement, so that no session opens a whole
# ADR for them, and from the Binding table of docs/adr/README.md the Applies to of every other ADR in force that paths alone do not decide,
# for the session to match by its prose. An Applies to is decided by paths alone when each of
# its items, split at commas and 'and', is one path in backticks.
# Each --path names a path the work is about to touch, for a session that has changed nothing
# yet; the paths stand in for the change, and the ADRs are read as the working tree has them. A
# named directory also binds an ADR that names a path under it. With --path, a base or head
# given as well is ignored.
# Without a base, the merge base of head with origin/main (else main) is used; CI passes the
# pull request's base. A base of zeros, which a push event reports for a new branch, counts as
# none. Without a head, the change is the working tree, uncommitted and untracked files
# included, and the ADRs are read as the working tree has them, so a session gets its list
# before its first commit; with a head, the change ends at that commit and the ADRs are read as
# it has them. With nothing to compare — no base and no main, or no change — it says so. Exit
# code 0 always, except 2 on a base or head that does not resolve.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"
# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

enter_root "Binding ADRs sensor" "${BASH_SOURCE[0]}" "$@"
shift "$ROOT_TAKEN"

sections=0
named=()
while (($#)); do
  case "$1" in
    --sections) sections=1; shift ;;
    --path)
      [[ -n "${2:-}" ]] || { echo "Binding ADRs sensor: --path needs a path." >&2; exit 2; }
      named+=("${2#./}")
      shift 2
      ;;
    *) break ;;
  esac
done

# MODULE_DIRS — per language that lays out a module's children in the directory of its file's
# name, the extension and the file names that do not, '<extension> <name>...' a line. Rust's
# lib.rs, main.rs, and mod.rs have their children beside them, and build.rs is no module.
MODULE_DIRS='rs lib.rs main.rs mod.rs build.rs'

# module_dir <path> — the directory that holds the children of the module in the file, with its
# trailing slash, by MODULE_DIRS; fails for a path that names none.
module_dir() {
  local name="${1##*/}" ext excluded
  [[ "$name" == ?*.* ]] || return 1
  while read -r ext excluded; do
    [[ "${name##*.}" == "$ext" ]] || continue
    [[ " $excluded " == *" $name "* ]] && return 1
    printf '%s/' "${1%.*}"
    return 0
  done <<< "$MODULE_DIRS"
  return 1
}

# The hint for the ADRs whose Applies to is prose; --sections prints them itself.
prose_hint="Read the index for the ADRs whose Applies to is prose."
prose_hint_list="the index for the ADRs whose Applies to is prose:"
if ((sections)); then
  prose_hint="The ADRs whose Applies to is prose follow below."
  prose_hint_list="the ADRs whose Applies to is prose, below:"
fi

# The change as resolve_change of scripts/lib/git.sh sets it; named paths are read in the working
# tree.
base="" head=HEAD worktree=1 verb=changed

# show <path> — a file as the change has it.
show() { change_file "$( ((worktree)) || echo "$head")" "$1"; }

# finish — print what --sections asks for, and end. Reads 'files', the ADRs listed.
files=()
finish() {
  if ((sections)); then
    local file text listed=" " key applies rows=0 prose=() parts part banner="the header"
    # The banner of an ADR names what is printed of it: its header and ADR_RULE_SECTIONS.
    IFS='|' read -r -a parts <<< "$ADR_RULE_SECTIONS"
    for part in "${parts[@]}"; do
      [[ "$part" == "${parts[-1]}" ]] && banner+=", and the $part" || banner+=", the $part"
    done
    for file in "${files[@]}"; do listed+="$(adr_key "$file") "; done
    # shellcheck disable=SC2016  # the backticks are Markdown to match, not a command substitution
    while IFS=$'\t' read -r key applies; do
      rows=$((rows + 1))
      [[ "$listed" == *" $key "* ]] && continue
      grep -qvE '^[[:space:]]*`[^`[:space:]]+`[[:space:]]*$' \
        <<< "$(sed -E 's/, and /\n/g; s/ and /\n/g; s/, /\n/g' <<< "$applies")" \
        && prose+=("ADR-$key: $applies")
    done < <(show docs/adr/README.md 2>/dev/null | awk -F'|' '
      /^### Binding/ { p = 1; next } p && /^#/ { exit }
      p && /^\| \[/ {
        key = $2; sub(/^ *\[/, "", key); sub(/\].*/, "", key)
        applies = $4; gsub(/^ +| +$/, "", applies)
        print key "\t" applies
      }')
    echo
    if ((rows == 0)); then
      echo "No row found under '### Binding' in docs/adr/README.md; read the index for the ADRs whose Applies to is prose."
    elif ((${#prose[@]})); then
      echo "The ADRs in force no path above decides, by their Applies to; match the prose against the work:"
      printf '  - %s\n' "${prose[@]}"
    else
      echo "Every other ADR in force names paths alone, and none of them names one of the work."
    fi
    for file in "${files[@]}"; do
      text="$(show "$file")"
      echo
      echo "=== ADR-$(adr_key "$file") ($file): $banner ==="
      awk '/^## / { exit } { print }' <<< "$text"
      for part in "${parts[@]}"; do
        [[ "$part" == "${parts[0]}" ]] || echo
        adr_section "$part" <<< "$text"
      done
    done
  fi
  exit 0
}

if ((${#named[@]})); then
  worktree=1
  changed=("${named[@]}")
  range="named paths"
  verb=named
else
  resolve_change "Binding ADRs sensor" "${1:-}" "${2:-}"; resolved=$?
  base="$CHANGE_BASE" head="$CHANGE_HEAD" worktree="$CHANGE_WORKTREE"
  ((resolved == 0)) || finish

  # git quotes a path with bytes outside ASCII unless told not to, and a quoted path matches
  # nothing an ADR writes.
  changed=()
  while IFS= read -r p; do [[ -n "$p" ]] && changed+=("$p"); done < <(
    if ((worktree)); then
      git -c core.quotePath=false diff --name-only --no-renames "$base"
      git -c core.quotePath=false ls-files --others --exclude-standard
    else
      git -c core.quotePath=false diff --name-only --no-renames "$base" "$head"
    fi | LC_ALL=C sort -u)

  short_base="$(git rev-parse --short "$base")"
  range="$short_base..$(git rev-parse --short "$head")"
  ((worktree)) && range="$short_base..working tree"
  if ((${#changed[@]} == 0)); then
    echo "Binding ADRs sensor ($range): nothing to compare — no path changed. Name the paths the work will touch with --path."
    finish
  fi
fi

# The ADRs as the change has them: one that adds an ADR or flips one is read with its own record.
if ((worktree)); then
  adrs="$(adr_files docs/adr 2>/dev/null)"
else
  adrs="$(git ls-tree -r --name-only "$head" -- docs/adr/ | grep -E "^docs/adr/$ADR_FILE_RE\$")"
fi

found=()
while IFS= read -r file; do
  [[ -n "$file" ]] || continue
  text="$(show "$file")"
  status="$(adr_field_line - Status <<< "$text")"
  [[ "$status" == *"🟢"* || "$status" == *"🟡"* ]] || continue
  applies="$(adr_field_line - 'Applies to' <<< "$text")"
  paths=()
  # shellcheck disable=SC2016  # the backticks are Markdown to match, not a command substitution
  while IFS= read -r token; do
    [[ "$token" =~ ^[^[:space:]]+$ ]] && paths+=("${token%/}")
  done < <(grep -oE '`[^`]+`' <<< "$applies" | tr -d '`')
  ((${#paths[@]})) || continue
  dirs=()
  for p in "${paths[@]}"; do dirs+=("$(module_dir "$p")"); done
  hits=()
  for c in "${changed[@]}"; do
    for i in "${!paths[@]}"; do
      p="${paths[$i]}" dir="${dirs[$i]}"
      if [[ "$c" == "$p" || "$c" == "$p/"* || ( -n "$dir" && "$c" == "$dir"* ) \
        || ( ${#named[@]} -gt 0 && "$p" == "${c%/}/"* ) ]]; then
        hits+=("$c")
        break
      fi
    done
  done
  ((${#hits[@]})) || continue
  id="ADR-$(adr_key "$file")"
  more=""
  ((${#hits[@]} > 3)) && more=", and $((${#hits[@]} - 3)) more"
  found+=("$id ($file) — ${hits[*]:0:3}$more")
  files+=("$file")
done <<< "$adrs"

if ((${#found[@]} == 0)); then
  echo "Binding ADRs sensor ($range): ${#changed[@]} path(s) $verb; no ADR in force names one of them. $prose_hint"
  finish
fi
echo "Binding ADRs sensor ($range): ${#changed[@]} path(s) $verb; ${#found[@]} ADR(s) in force name one of them. Read these, and $prose_hint_list"
printf '  - %s\n' "${found[@]}"
finish
