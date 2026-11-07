#!/usr/bin/env bash
#
# Self-test of scripts/sensor-backlog.sh. Verifies: ADR-0054
#
# Verifies: ADR-0002
#
# Each case builds a throwaway git repository under a temporary directory, records backlog
# entries under docs/backlog/ on main or on branches, runs the sensor against it, and asserts the
# exit code and a line a reader would meet. The cases form a table: a repository without a
# backlog, an entry on main, an entry only on an unmerged branch, an entry a merged branch added,
# an entry main deleted that an older branch still carries, and a root that is no repository. The
# frontier sensor is run beside it, over a plan with one ticket: an entry is no ticket.
#
# Inputs: scripts/sensor-backlog.sh scripts/sensor-plan-frontier.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-backlog.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-backlog.sh"
FRONTIER="$ROOT/scripts/sensor-plan-frontier.sh"

# git_in <dir> <args...> — git in the fixture, with an author.
git_in() {
  fixture "$1"
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=fixture -c user.email=fixture@example.invalid "$@"
}

# repo <name> — an empty repository with one commit, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  git_in "$dir" commit -q --allow-empty -m "start"
  echo "$dir"
}

# entry <dir> <slug> <title> — record a backlog entry in a commit of its own.
entry() {
  fixture "$1"
  mkdir -p "$1/docs/backlog"
  printf '# %s\n\nObserved: something.\n\nWhere: scripts/x.sh\n' "$3" > "$1/docs/backlog/$2.md"
  git_in "$1" add "docs/backlog/$2.md"
  git_in "$1" commit -q -m "backlog: $2"
}

# expect <exit code> <name> <dir> <needle> [absent] — run the sensor; its output must contain the
# needle, and must not contain the second needle when one is given.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4" absent="${5:-}"
  local out rc
  out="$(bash "$SENSOR" "$dir" 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* && ( -z "$absent" || "$out" != *"$absent"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\"${absent:+ and without \"$absent\"}, got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

r="$(repo empty)"
expect 0 "no backlog: says so" "$r" "no entries under docs/backlog/ — nothing to report"

r="$(repo on-main)"
entry "$r" slow-start "The first frame is slow"
expect 0 "an entry on main: counted and listed with its title" "$r" "1 on main, 0 only on unmerged"
expect 0 "an entry on main: its line" "$r" "- docs/backlog/slow-start.md — The first frame is slow"

r="$(repo on-branch)"
entry "$r" kept "Kept on main"
git_in "$r" switch -q -c feat/x
entry "$r" doubt "A doubt found on a branch"
git_in "$r" switch -q main
expect 0 "an entry only on an unmerged branch: listed with the branch" "$r" \
  "- docs/backlog/doubt.md — A doubt found on a branch (on feat/x)"
expect 0 "an entry only on an unmerged branch: counted apart" "$r" "1 on main, 1 only on unmerged"

r="$(repo merged)"
git_in "$r" switch -q -c feat/y
entry "$r" merged-one "Merged already"
git_in "$r" switch -q main
git_in "$r" merge -q --no-ff -m "merge feat/y" feat/y
expect 0 "a merged branch: its entry counts on main only" "$r" "1 on main, 0 only on unmerged" "(on feat/y)"

r="$(repo deleted)"
entry "$r" old "Old finding"
git_in "$r" switch -q -c feat/older
git_in "$r" commit -q --allow-empty -m "unrelated work"
git_in "$r" switch -q main
git_in "$r" rm -q docs/backlog/old.md
git_in "$r" commit -q -m "ticket from old; the entry goes"
expect 0 "an entry main deleted: an older branch does not bring it back" "$r" \
  "no entries under docs/backlog/" "old.md"

r="$(repo with-plan)"
entry "$r" not-a-ticket "Not a ticket"
mkdir -p "$r/docs/plan/s"
printf '# Ticket: fixture\n\n## Blocked by\n\n- None (can start immediately)\n\n## Mode\n\nwith the human\n' \
  > "$r/docs/plan/s/01-real.md"
expect 0 "an entry is no ticket: the frontier sensor ignores it" "$r" "not-a-ticket.md"
out="$(bash "$FRONTIER" "$r" 2>&1)"; rc=$?
check "an entry is no ticket: the frontier sensor reads the ticket and no entry" \
  test "$rc" -eq 0 -a "${out/not-a-ticket/}" == "$out" -a "${out/01-real/}" != "$out"

mkdir -p "$work/plain"
expect 2 "a root that is no repository: exit 2" "$work/plain" "is no git repository"

summary "Backlog sensor self-test"
