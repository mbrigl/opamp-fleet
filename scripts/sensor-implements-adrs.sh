#!/usr/bin/env bash
#
# Sensor: the ADRs a branch's commits implement (AGENTS.md, section 4: commits name the ADR
# they implement).
#
# Lists the commits between a base and a head whose message names, on a line that says
# 'Implements', capitalized, an ADR that is neither accepted nor proposed at the head: one a
# later ADR superseded, one rejected, or one the head does not have. Such a commit names the
# decision it replaced instead of its successor. An ADR is 'ADR-NNNN', read from the status line of
# docs/adr/NNNN-*.md, or a draft, 'ADR-draft-<slug>': proposed while docs/adr/draft-<slug>.md
# exists, and once numbered the ADR the map of the numbering commit gives it
# ('ADR-draft-<slug> → ADR-NNNN'). Only the commits of the change are read: the history before
# the base keeps the names it had. A number a later normalization gave to another ADR is read as
# the head numbers it, which this sensor cannot tell apart. It reports and never fails.
#
# Pure bash + git + grep + sed. Usage:
#     scripts/sensor-implements-adrs.sh [--root <dir>] [base [head]]
# It reads the repository it lives in, as the checks do; --root names another, as its self-test
# does with each fixture.
# Without a base, the merge base of head with origin/main (else main) is used; CI passes the
# pull request's base. A base of zeros, which a push event reports for a new branch, counts as
# none. With nothing to compare — on main without a remote, or base and head the same — it says
# so. Exit code 0 always, except 2 on a base or head that does not resolve.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

enter_root "Implements sensor" "${BASH_SOURCE[0]}" "$@"
shift "$ROOT_TAKEN"

resolve_change "Implements sensor" "${1:-}" "${2:-HEAD}" || exit 0
base="$CHANGE_BASE" head="$CHANGE_HEAD"
if [[ "$(git rev-parse "$base^{commit}")" == "$(git rev-parse "$head^{commit}")" ]]; then
  echo "Implements sensor: nothing to compare — base and head are the same commit."
  exit 0
fi

# state <id> — 'in force' for an ADR accepted or proposed at the head, else what it is.
state() {
  local id="$1" file status
  if [[ "$id" == ADR-draft-* ]]; then
    if git cat-file -e "$head:docs/adr/${id#ADR-}.md" 2>/dev/null; then
      echo "in force"
      return
    fi
    # A numbered draft: the map of the numbering commit, 'ADR-draft-<slug> → ADR-NNNN'.
    local number
    number="$(git log --format=%B -F --grep="$id → ADR-" "$head" \
      | grep -m 1 -oE "^$id → ADR-[0-9]{4}" | grep -oE 'ADR-[0-9]{4}$')"
    [[ -n "$number" ]] || { echo "no draft of that name and no number for it"; return; }
    id="$number"
    local through=" (numbered $number)"
  fi
  file="$(git ls-tree --name-only "$head" docs/adr/ 2>/dev/null \
    | grep -m 1 -E "^docs/adr/${id#ADR-}-[^/]*\.md$")"
  [[ -n "$file" ]] || { echo "no ADR of that number${through:-}"; return; }
  # The status without its emoji and with a link's text alone: 'superseded by ADR-0039'.
  status="$(git show "$head:$file" | sed -n 's/^- \*\*Status:\*\* *//p' | head -n 1 \
    | sed -E 's/\[([^]]*)\]\([^)]*\)/\1/g; s/^[^A-Za-z]*//')"
  case "$status" in
    *accepted* | *proposed*) echo "in force" ;;
    "") echo "no status line${through:-}" ;;
    *) echo "$status${through:-}" ;;
  esac
}

findings=() count=0
while read -r commit; do
  ids="$(git log -1 --format=%B "$commit" | grep -w 'Implements' \
    | grep -oE 'ADR-([0-9]{4}|draft-[a-z0-9]+(-[a-z0-9]+)*)' | sort -u)"
  [[ -n "$ids" ]] || continue
  count=$((count + 1))
  while read -r id; do
    what="$(state "$id")"
    [[ "$what" == "in force" ]] && continue
    findings+=("$(git log -1 --format='%h %s' "$commit") — $id: $what")
  done <<< "$ids"
done < <(git rev-list --reverse "$base..$head")

echo "Implements sensor ($(git rev-parse --short "$base")..$(git rev-parse --short "$head")):" \
  "$count commit(s) name an ADR they implement; ${#findings[@]} name one not in force."
((${#findings[@]})) && printf '  - %s\n' "${findings[@]}"
exit 0
