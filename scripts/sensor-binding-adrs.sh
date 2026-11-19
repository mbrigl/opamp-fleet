#!/usr/bin/env bash
#
# Sensor: the ADRs that bind a change (NUC-decisions-are-proposed-then-accepted §5, AGENTS.md §2).
#
# Lists the ADRs in force — accepted or proposed — whose '**Applies to:**' line names a path
# a change touches. A path is what that line writes in backticks with no space in it, such as
# `docs/plan/`, `README.md`, or `Makefile`; it names itself and everything under it, and a file
# of a language that lays out a module's children in the directory of the file's name names that
# directory too (MODULE_DIRS below): a Rust file `x.rs` names `x/`, so that a module split off
# a file stays bound. A word in backticks that is no path, such as a skill's name, matches no
# changed path. A file named 'check N of `<file>`' or 'checks N, M, and K of `<file>`' binds only
# where a line of a hunk, or the line on either side, lies in such a check's function, from
# 'check_<name>() {  # check N' to its closing brace; a line outside the checks, such as the header
# or a shared helper, binds every ADR that names the file, as a named path does. The rest is prose
# no script reads, so the list is the ADRs a session reads at the least: the index is still read
# by its prose. Otherwise a path stands for the whole file, also where the ADR governs a part of
# it. It reports and never fails, so that the implementing session and the reviewer of AGENTS.md
# section 5 start from the same list.
#
# Pure bash + git. Usage:
#     scripts/sensor-binding-adrs.sh [--root <dir>] [--sections] [--path <path>]... [base [head]]
# It reads the repository it lives in, as the checks do; --root names another, as its self-test
# does with each fixture. With --sections it prints after the list what a session reads of the
# record (AGENTS.md section 2): of each ADR listed its header and the sections ADR_RULE_SECTIONS of
# scripts/lib/adr.sh names, so that no session opens a whole ADR for them, and the Applies to of
# every other ADR in force that paths alone do not decide, the 'NUC-' ones of the base included,
# read from its file for the session to match by its prose: an item of it, split at commas and
# 'and', that is not one path in backticks. Each --path names a path the work is about to touch,
# for a session that has changed nothing yet; the paths stand in for the change, and the ADRs are
# read as the working tree has them. A named directory also binds an ADR that names a path under
# it. With --path, a base or head given as well is ignored. Without a base, the merge base of head
# with origin/main (else main) is used; CI passes the pull request's base. A base of zeros, which
# a push event reports for a new branch, counts as none. Without a head, the change is the working
# tree, uncommitted and untracked files included, and the ADRs are read as the working tree has
# them, so a session gets its list before its first commit; with a head, the change ends at that
# commit and the ADRs are read as it has them. With nothing to compare — no base and no main, or
# no change — it says so. Exit code 0 always, except 2 on a base or head that does not resolve.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

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

# adr_list — the ADR files as the change has them: one that adds an ADR or flips one is read with
# its own record.
adr_list() {
  if ((worktree)); then
    adr_files docs/adr 2>/dev/null
  else
    git ls-tree -r --name-only "$head" -- docs/adr/ | grep -E "^docs/adr/$ADR_FILE_RE\$"
  fi
}

# finish — print what --sections asks for, and end. Reads 'files', the ADRs listed.
files=()
finish() {
  if ((sections)); then
    local file text listed=" " applies status rows=0 prose=() parts part banner="the header"
    # The banner of an ADR names what is printed of it: its header and ADR_RULE_SECTIONS.
    IFS='|' read -r -a parts <<< "$ADR_RULE_SECTIONS"
    for part in "${parts[@]}"; do
      [[ "$part" == "${parts[-1]}" ]] && banner+=", and the $part" || banner+=", the $part"
    done
    listed+="${files[*]} "
    # shellcheck disable=SC2016  # the backticks are Markdown to match, not a command substitution
    while IFS= read -r file; do
      [[ -n "$file" ]] || continue
      text="$(show "$file")"
      status="$(adr_field_line - Status <<< "$text")"
      [[ "$status" == *"🟢"* || "$status" == *"🟡"* ]] || continue
      rows=$((rows + 1))
      [[ "$listed" == *" $file "* ]] && continue
      adr_field_value "$(adr_field_line - 'Applies to' <<< "$text")" 'Applies to'
      applies="${ADR_VALUE#"${ADR_VALUE%%[![:space:]]*}"}"
      grep -qvE '^[[:space:]]*`[^`[:space:]]+`[[:space:]]*$' \
        <<< "$(sed -E 's/, and /\n/g; s/ and /\n/g; s/, /\n/g' <<< "$applies")" \
        && prose+=("$(adr_id "$file"): $applies")
    done < <(adr_list)
    echo
    if ((rows == 0)); then
      echo "No ADR in force found in docs/adr/; read the index for the ADRs whose Applies to is prose."
    elif ((${#prose[@]})); then
      echo "The ADRs in force no path above decides, by their Applies to; match the prose against the work:"
      printf '  - %s\n' "${prose[@]}"
    else
      echo "Every other ADR in force names paths alone, and none of them names one of the work."
    fi
    for file in "${files[@]}"; do
      text="$(show "$file")"
      echo
      echo "=== $(adr_id "$file") ($file): $banner ==="
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

# held — of each line of a file, the number of the check whose function holds it (see the header).
held() { awk '/^check_.*# check [0-9]+$/ { n = $NF } { print n } /^}/ { n = 0 } END { print 0 }'; }

# checked <text> <path> — whether the change touches a check of the file the text numbers.
checked() {
  { [[ "$1" != *[0-9]* ]] || ((${#named[@]})); } && return 0
  local at=("$base") n t; ((worktree)) || at+=("$head")
  t="$(awk 'NR == FNR { m[FNR] = $0; next } { split($1, r, ","); d = r[2] == "" ? 1 : r[2]
      for (i = r[1] - (d > 0); i <= r[1] - (d > 0) + d + 1; i++) t[m[i] ? m[i] : "all"] = 1 }
    END { for (k in t) o = o " " k; print o " " }' <(show "$2" 2>/dev/null | held) \
    <(git diff -U0 --no-renames "${at[@]}" -- "$2" | sed -nE 's/^@@ [^+]*\+([0-9,]+) .*/\1/p'))"
  for n in ${1//[!0-9]/ } all; do [[ "$t" == *" $n "* || "$t" == " " ]] && return 0; done
  return 1
}

adrs="$(adr_list)"

found=()
while IFS= read -r file; do
  [[ -n "$file" ]] || continue
  text="$(show "$file")"
  status="$(adr_field_line - Status <<< "$text")"
  [[ "$status" == *"🟢"* || "$status" == *"🟡"* ]] || continue
  applies="$(adr_field_line - 'Applies to' <<< "$text")"
  paths=() nums=()
  # shellcheck disable=SC2016  # the backticks are Markdown to match, not a command substitution
  while IFS= read -r token; do
    p="${token#*\`}" && p="${p%\`}"
    [[ "$p" =~ ^[^[:space:]]+$ ]] && paths+=("${p%/}") && nums+=("${token%%\`*}")
  done < <(grep -oE '(checks? [0-9][0-9, and]* of )?`[^`]+`' <<< "$applies")
  ((${#paths[@]})) || continue
  dirs=()
  for p in "${paths[@]}"; do dirs+=("$(module_dir "$p")"); done
  hits=()
  for c in "${changed[@]}"; do
    for i in "${!paths[@]}"; do
      p="${paths[$i]}" dir="${dirs[$i]}"
      if [[ "$c" == "$p" || "$c" == "$p/"* || ( -n "$dir" && "$c" == "$dir"* ) \
        || ( ${#named[@]} -gt 0 && "$p" == "${c%/}/"* ) ]] && checked "${nums[$i]}" "$c"; then
        hits+=("$c")
        break
      fi
    done
  done
  ((${#hits[@]})) || continue
  id="$(adr_id "$file")"
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
