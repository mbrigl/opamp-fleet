#!/usr/bin/env bash
#
# Sensor: the ADRs a branch's commits implement (AGENTS.md, section 4: commits name the ADR
# they implement).
#
# Lists the commits between a base and a head whose message names, on a line that says
# 'Implements', capitalized, an ADR that is neither accepted nor proposed at the head: one a
# later ADR superseded, one rejected, or one the head does not have. Such a commit names the
# decision it replaced instead of its successor. An ADR is its identifier, '<CAT>-<slug>' of a
# category the head's index knows (NUC-adrs-are-named-by-category §1, §2), read from the status
# line of docs/adr/<CAT>-<slug>.md at the head. Only the commits of the change are read: the
# history before the base keeps the names it had, numbers among them, which no longer read as an
# ADR. It reports and never fails.
#
# Pure bash + git + grep + sed. Usage:
#     scripts/sensor-implements-adrs.sh [--root <dir>] [base [head]]
# It reads the repository it lives in, as the checks do; --root names another, as its self-test
# does with each fixture.
# Without a base, the merge base of head with origin/main (else main) is used; CI passes the
# pull request's base. A base of zeros, which a push event reports for a new branch, counts as
# none. With nothing to compare — on main without a remote, or base and head the same — it says
# so. Exit code 0 always, except 2 on a base or head that does not resolve, an argument it does
# not read, or a root that is no directory.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"
# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/sensor-implements-adrs.sh [--root <dir>] [base [head]]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
set -- "${ARGS[@]}"
rest_args "${BASH_SOURCE[0]}" "$usage" 2 "$@"

resolve_change "Implements sensor" "${1:-}" "${2:-HEAD}" || exit 0
base="$CHANGE_BASE" head="$CHANGE_HEAD"
if [[ "$(git rev-parse "$base^{commit}")" == "$(git rev-parse "$head^{commit}")" ]]; then
  echo "Implements sensor: nothing to compare — base and head are the same commit."
  exit 0
fi

# The categories of the head's index, so that an identifier of a project's own category reads as
# one.
index="$(mktemp)"
trap 'rm -f "$index"' EXIT
git show "$head:docs/adr/README.md" > "$index" 2>/dev/null
adr_categories "$index"

# state <id> — 'in force' for an ADR accepted or proposed at the head, else what it is.
state() {
  local id="$1" file="docs/adr/$1.md" status line
  git cat-file -e "$head:$file" 2>/dev/null || { echo "no ADR of that identifier"; return; }
  # The status without its emoji and with a link's text alone: 'superseded by <CAT>-<slug>'.
  status=""
  if line="$(git show "$head:$file" | adr_field_line - Status)"; then
    adr_field_value "$line" Status
    status="$(sed -E 's/\[([^]]*)\]\([^)]*\)/\1/g' <<< "$ADR_VALUE")"
    status="${status#"${status%%[A-Za-z]*}"}"
  fi
  case "$status" in
    *accepted* | *proposed*) echo "in force" ;;
    "") echo "no status line" ;;
    *) echo "$status" ;;
  esac
}

findings=() count=0
while read -r commit; do
  ids="$(git log -1 --format=%B "$commit" | grep -w 'Implements' \
    | grep -oE "$ADR_REF_WORD_RE" | grep -oE "$ADR_REF_RE" | sort -u)"
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
