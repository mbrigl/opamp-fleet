#!/usr/bin/env bash
#
# Sensor: the commits of the merged work that were red alone.
#
# For each of the ten newest merges of a pull request on main, it runs the scripts/check-all.sh
# of every commit the merge brought in, merges aside, in a temporary worktree of that commit, and
# lists each on which it exited 1, a failed check. A commit on which it exited otherwise is named
# as not decided, such as an old script refusing its bash; one without scripts/check-all.sh, or
# whose worktree could not be made, is counted as not run. It reports and never fails.
#
# It needs only git and the commits. It runs on a human's instruction, not in scripts/check-all.sh
# or checks.yml, since it costs a full run of the checks per commit (NUC-checks-are-bash §7).
# scripts/test-sensor-red-commits.sh runs it on fixture repositories, with a stub check-all.sh.
#
# Each commit's checks run with the tools of this machine and the sensor's environment, and with
# no timeout: an old check-all.sh that hangs blocks the run until a human stops it. After a stop,
# the jobs an old check-all.sh started in the background may run on for some seconds in the
# removed worktree.
# No case covers INT: a background job of a non-interactive shell ignores it, so only a run by
# hand shows that Ctrl-C stops the checks.
#
# Bash + coreutils + git + sed. Usage:
#     scripts/sensor-red-commits.sh [--root <dir>]
# The root defaults to this repository. main is origin/main where it exists, else main. Exit
# code 0 always, except 2 on an unknown argument, a root that is no git repository, or no
# temporary directory, and 130 or 143 when INT or TERM stops it.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"

NAME="Red commits sensor"
usage="scripts/sensor-red-commits.sh [--root <dir>]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
rest_args "${BASH_SOURCE[0]}" "$usage" 0 "${ARGS[@]}"
last=10  # the merges read, the newest first
git -C "$ROOT" rev-parse --git-dir > /dev/null 2>&1 \
  || { echo "red commits sensor: '$ROOT' is no git repository." >&2; exit 2; }

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

# red_alone <merge> — the commits the merge brought in, merges aside, oldest first, each run with
# its own scripts/check-all.sh in a temporary worktree. Prints a line for each on which it exited
# 1, a failed check; one for each on which it exited otherwise, which decides nothing; and how
# many it could not run. Names each commit on stderr as it starts, since a run takes minutes.
red_alone() {
  local merge="$1" c rc unchecked=0
  for c in $(git -C "$ROOT" rev-list --reverse --no-merges "$merge^1..$merge^2"); do
    if ! git -C "$ROOT" cat-file -e "$c:scripts/check-all.sh" 2>/dev/null; then
      unchecked=$((unchecked + 1))
      continue
    fi
    wt="$tmp/$c"
    if ! git -C "$ROOT" worktree add -q --detach "$wt" "$c" > /dev/null 2>&1; then
      wt=""
      unchecked=$((unchecked + 1))
      continue
    fi
    echo "red commits sensor: checking $(git -C "$ROOT" log -1 --format=%h "$c") …" >&2
    (cd "$wt" && exec bash scripts/check-all.sh > /dev/null 2>&1) &
    child=$!
    wait "$child"; rc=$?
    child=""
    if ((rc == 1)); then
      echo "    - red alone: $(git -C "$ROOT" log -1 --format='%h %s' "$c")"
    elif ((rc != 0)); then
      echo "    - not decided, its checks exited $rc: $(git -C "$ROOT" log -1 --format='%h %s' "$c")"
    fi
    git -C "$ROOT" worktree remove --force "$wt" > /dev/null 2>&1
    wt=""
  done
  ((unchecked == 0)) \
    || echo "    - not run, without scripts/check-all.sh or a worktree: $unchecked commit$( ((unchecked == 1)) || echo s)"
}
# On exit the checks still running stop, and only the worktree this run made is removed, with
# its entry: an interrupted run leaves none behind, and the worktrees of others stay as they are.
# The checks run in the background, so that a signal stops the wait for them at once.
# shellcheck disable=SC2317,SC2329 # run by the trap
drop_worktree() {
  [[ -z "${child:-}" ]] || { kill "$child" 2>/dev/null; wait "$child" 2>/dev/null; }
  [[ -z "${wt:-}" ]] || git -C "$ROOT" worktree remove --force "$wt" > /dev/null 2>&1
  rm -rf "$tmp"
}
wt="" child=""
tmp="$(mktemp -d)" || { echo "red commits sensor: no temporary directory." >&2; exit 2; }
trap drop_worktree EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for entry in "${merges[@]:0:read_n}"; do
  read -r merge pr branch <<< "$entry"
  echo "  - #$pr $branch"
  red_alone "$merge"
done
exit 0
