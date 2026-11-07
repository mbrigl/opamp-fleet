#!/usr/bin/env bash
#
# Sensor: the backlog (ADR-0006).
#
# Lists the backlog entries, one file each under docs/backlog/: those on main, and those a local
# branch adds that main does not have yet, with the branch, so that a session in a parallel
# worktree sees a finding recorded on a branch not yet merged. An entry a branch adds counts from
# the branch's merge base with main, so a branch cut before main deleted an entry does not bring
# the entry back. Each line carries the entry's title, its first '# ' heading. Main is origin/main
# when it exists, else main, else HEAD. A project whose backlog is the issue tracker of its host
# has no docs/backlog/, and the sensor says there is nothing to report. An entry not yet committed
# is not listed: ADR-0006 has each created in a commit of its own. It reports and never fails.
#
# Pure bash + git. Usage:
#     scripts/sensor-backlog.sh [repository root]
# The root defaults to this repository; scripts/test-sensor-backlog.sh passes fixtures.
# Exit code 0, except 2 on a root that is no git repository.

set -uo pipefail

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" 2>/dev/null && pwd)"
DIR="docs/backlog"
if [[ -z "$ROOT" ]] || ! git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  echo "Backlog sensor: '${1:-}' is no git repository." >&2
  exit 2
fi
g() { git -C "$ROOT" "$@"; }

main=""
for candidate in origin/main main HEAD; do
  g rev-parse -q --verify "$candidate^{commit}" >/dev/null && { main="$candidate"; break; }
done
[[ -n "$main" ]] || { echo "Backlog sensor: no commit yet — nothing to report."; exit 0; }

# title <ref> <path> — the first '# ' heading of the entry at the ref, or its file name.
title() {
  local t
  t="$(g show "$1:$2" 2>/dev/null | sed -n 's/^# //p' | head -n 1)"
  printf '%s\n' "${t:-${2##*/}}"
}

declare -A on_main=()
lines=()
while IFS= read -r path; do
  [[ "$path" == *.md ]] || continue
  on_main["$path"]=1
  lines+=("- $path — $(title "$main" "$path")")
done < <(g ls-tree -r --name-only "$main" -- "$DIR/")
n_main=${#lines[@]}

n_branch=0
while IFS= read -r branch; do
  [[ "$branch" == main ]] && continue
  g merge-base --is-ancestor "$branch" "$main" 2>/dev/null && continue
  base="$(g merge-base "$main" "$branch" 2>/dev/null)" || continue
  while IFS= read -r path; do
    [[ "$path" == *.md && ! -v on_main["$path"] ]] || continue
    lines+=("- $path — $(title "$branch" "$path") (on $branch)")
    n_branch=$((n_branch + 1))
  done < <(g diff --no-renames --name-only --diff-filter=A "$base" "$branch" -- "$DIR/")
done < <(g for-each-ref --format='%(refname:short)' refs/heads/)

if ((n_main + n_branch == 0)); then
  echo "Backlog sensor: no entries under $DIR/ — nothing to report."
  exit 0
fi
echo "Backlog sensor: $n_main on $main, $n_branch only on unmerged local branches."
printf '%s\n' "${lines[@]}"
exit 0
