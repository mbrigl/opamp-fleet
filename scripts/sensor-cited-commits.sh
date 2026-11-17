#!/usr/bin/env bash
#
# Sensor: the commits the plan and the backlog cite (ADR-0002, the sensors the template ships).
#
# Lists the commit hashes written in the files under docs/plan/ and docs/backlog/ that the head
# does not reach. A rework that rewrites a branch leaves the old hashes in a ticket or an entry;
# such a hash still resolves, since the rewritten commit stays in the object store, so whether the
# head reaches it decides, not whether it resolves. A hash is a word of 7 to 40 hexadecimal digits
# with at least one digit and one of a to f, so 'decade' and '1234567' are none. A hash that
# resolves to no commit is listed apart, and is as likely the same mistake: a fresh clone, as CI
# makes, fetches only what a branch or tag reaches, so there a rewritten commit resolves to
# nothing. Only a clone that still holds the old object tells the two apart; the rest of that
# list is a commit of another repository, a dropped object, or a word that only looks like a
# hash. It reports and never fails.
#
# Pure bash + git + grep. Usage:
#     scripts/sensor-cited-commits.sh [--root <dir>] [head]
# It reads the files of the working tree and the history of the head, HEAD when none is given;
# --root names another repository, as its self-test does with each fixture.
# Exit code 0, except 2 on a head that does not resolve.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

enter_root "Cited commits sensor" "${BASH_SOURCE[0]}" "$@"
shift "$ROOT_TAKEN"

head="${1:-HEAD}"
if ! git rev-parse -q --verify "$head^{commit}" >/dev/null; then
  echo "Cited commits sensor: '$head' does not resolve to a commit." >&2
  exit 2
fi

dirs=()
for d in docs/plan docs/backlog; do [[ -d "$d" ]] && dirs+=("$d"); done
if ((${#dirs[@]} == 0)); then
  echo "Cited commits sensor: no docs/plan/ and no docs/backlog/ — nothing to report."
  exit 0
fi

unreached=() unknown=() count=0
while IFS=: read -r file line word; do
  [[ "$word" =~ [0-9] && "$word" =~ [a-f] ]] || continue
  count=$((count + 1))
  if ! git rev-parse -q --verify "$word^{commit}" >/dev/null 2>&1; then
    unknown+=("$file:$line: $word")
  elif ! git merge-base --is-ancestor "$word" "$head" 2>/dev/null; then
    unreached+=("$file:$line: $word")
  fi
done < <(grep -rnowE '[0-9a-f]{7,40}' --include='*.md' "${dirs[@]}" | LC_ALL=C sort)

echo "Cited commits sensor: $count hash(es) cited; ${#unreached[@]} the head does not reach," \
  "${#unknown[@]} that resolve to no commit."
if ((${#unreached[@]})); then
  echo "Not reached by $head — rewritten or on another branch; cite the commit the branch has:"
  printf '  - %s\n' "${unreached[@]}"
fi
if ((${#unknown[@]})); then
  echo "Resolving to no commit — rewritten (in a fresh clone), another repository, or no hash:"
  printf '  - %s\n' "${unknown[@]}"
fi
exit 0
