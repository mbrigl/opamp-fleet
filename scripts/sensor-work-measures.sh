#!/usr/bin/env bash
#
# Sensor: what the merged work cost in time and in rounds of review.
#
# For each of the ten newest merges of a pull request on main, it reports the branch, the whole
# hours from the branch's first commit to its merge, and the rounds of review. A claim made by a
# local branch has no commit, so the first commit of the branch, by its author date, stands for
# the claim. The rounds come from the description of the pull request, which the merge commit
# does not carry: the 'review rounds <n>' of its Measures line (.github/PULL_REQUEST_TEMPLATE.md).
# Its 'Reviewed at:' line counts none, since the review moves that one line after each round.
# More than one round means a review ran again after the fixes of its findings.
# The subject of a merge and the gh call that reads its description are those of
# scripts/sensor-release-notes.sh, its twin.
#
# The commits of a branch that were red alone are named by scripts/sensor-red-commits.sh, which
# needs no gh.
#
# It runs on a human's instruction, not in scripts/check-all.sh or checks.yml, since it reads
# GitHub (NUC-checks-are-bash §7). It reports and never fails. scripts/test-sensor-work-measures.sh
# runs it on fixture repositories: GH names the command that stands in for gh.
#
# Bash + coreutils + git + grep + sed, plus the GitHub CLI, authenticated through its web flow
# (NUC-main-only-via-pr §3). Usage:
#     scripts/sensor-work-measures.sh [--root <dir>]
# The root defaults to this repository. main is origin/main where it exists, else main. Exit
# code 0 always, except 2 on an unknown argument, a root that is no git repository, or a missing
# gh.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"

NAME="Work measures sensor"
usage="scripts/sensor-work-measures.sh [--root <dir>]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
rest_args "${BASH_SOURCE[0]}" "$usage" 0 "${ARGS[@]}"
last=10  # the merges read, the newest first
git -C "$ROOT" rev-parse --git-dir > /dev/null 2>&1 \
  || { echo "work measures sensor: '$ROOT' is no git repository." >&2; exit 2; }
GH="${GH:-gh}"
command -v "$GH" > /dev/null 2>&1 \
  || { echo "work measures sensor: the GitHub CLI ($GH) is not installed." >&2; exit 2; }

main=origin/main
git -C "$ROOT" rev-parse -q --verify "$main^{commit}" > /dev/null || main=main

# Every merge of a pull request on main, the newest first: '<merge> <pull request> <branch>'.
mapfile -t merges < <(git -C "$ROOT" log --first-parent --merges --format='%H %s' "$main" -- 2>/dev/null \
  | sed -nE 's/^([0-9a-f]+) Merge pull request #([0-9]+) from [^/]+\/(.+)$/\1 \2 \3/p')
if ((${#merges[@]} == 0)); then
  echo "$NAME: no merge of a pull request on main — nothing to report."
  exit 0
fi
read_n=$((last < ${#merges[@]} ? last : ${#merges[@]}))
echo "$NAME: ${#merges[@]} merge$( ((${#merges[@]} == 1)) || echo s) of a pull request on main, the newest $read_n read."

for entry in "${merges[@]:0:read_n}"; do
  read -r merge pr branch <<< "$entry"
  merged_at="$(git -C "$ROOT" log -1 --format=%ct "$merge")"
  first_at="$(git -C "$ROOT" log --reverse --format=%at "$merge^1..$merge^2" | head -n 1)"
  hours=$(( (merged_at - ${first_at:-$merged_at}) / 3600 ))
  if body="$(cd "$ROOT" && "$GH" pr view "$pr" --json body --jq .body 2>/dev/null)"; then
    rounds="$(grep -oE 'review rounds [0-9]+' <<< "$body" | head -n 1 | grep -oE '[0-9]+$')"
    if [[ -n "$rounds" ]] && ((rounds > 0)); then reviews="$rounds round$( ((rounds == 1)) || echo s) of review"
    else reviews="rounds of review unknown"; fi
  else
    reviews="rounds of review not read"
  fi
  echo "  - #$pr $branch: $hours h from its first commit to its merge, $reviews"
done
exit 0
