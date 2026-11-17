#!/usr/bin/env bash
#
# Self-test of scripts/sensor-cited-commits.sh.
#
# Verifies: ADR-0002
#
# Each case builds a throwaway git repository under a temporary directory, writes hashes into a
# ticket under docs/plan/ or an entry under docs/backlog/, runs the sensor against it, and
# asserts the exit code and a line a reader would meet. The cases form a table: a repository
# with neither directory, a hash the head reaches, one a rewrite left behind that still resolves,
# one on another branch, one that resolves to no commit, words that only look like hashes, and a
# head that does not resolve.
#
# Inputs: scripts/sensor-cited-commits.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-cited-commits.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-cited-commits.sh"

# git_in <dir> <args...> — git in the fixture, with an author.
git_in() {
  fixture "$1"
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=fixture -c user.email=fixture@example.invalid "$@"
}

# repo <name> — a repository with one commit, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  git_in "$dir" commit -q --allow-empty -m "start"
  echo "$dir"
}

# commit <dir> <message> — an empty commit, printed as its abbreviated hash.
commit() {
  git_in "$1" commit -q --allow-empty -m "$2"
  git_in "$1" rev-parse --short=9 HEAD
}

# cite <dir> <path> <text> — write the text into the file under the fixture.
cite() {
  fixture "$1"
  mkdir -p "$(dirname "$1/$2")"
  printf '# A note\n\n%s\n' "$3" > "$1/$2"
}

# expect <exit code> <name> <dir> <needle> [absent] [head] — run the sensor; its output must
# contain the needle, and must not contain the second needle when one is given.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4" absent="${5:-}" head="${6:-}"
  local out rc
  out="$(bash "$SENSOR" --root "$dir" ${head:+"$head"} 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* && ( -z "$absent" || "$out" != *"$absent"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\"${absent:+ and without \"$absent\"}, got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

r="$(repo none)"
expect 0 "neither directory: says so" "$r" "no docs/plan/ and no docs/backlog/ — nothing to report"

r="$(repo reached)"
h="$(commit "$r" "the fix")"
cite "$r" docs/plan/x/01-ticket.md "Landed in $h."
expect 0 "a hash the head reaches: counted, not listed" "$r" \
  "1 hash(es) cited; 0 the head does not reach, 0 that resolve to no commit." "$h"

r="$(repo rewritten)"
old="$(commit "$r" "the fix")"
git_in "$r" commit -q --amend --allow-empty -m "the fix, reworded"
cite "$r" docs/backlog/entry.md "See $old for the cause."
expect 0 "a rewritten commit: still resolves, listed as not reached" "$r" \
  "  - docs/backlog/entry.md:3: $old"
expect 0 "a rewritten commit: counted as not reached" "$r" "1 the head does not reach, 0 that"

r="$(repo elsewhere)"
git_in "$r" switch -q -c feat/other
other="$(commit "$r" "on the other branch")"
git_in "$r" switch -q main
cite "$r" docs/plan/x/02-ticket.md "Builds on $other."
expect 0 "a commit of another branch: listed as not reached" "$r" \
  "  - docs/plan/x/02-ticket.md:3: $other"
expect 0 "a commit of another branch: reached from that branch" "$r" \
  "0 the head does not reach" "" feat/other

r="$(repo unknown)"
cite "$r" docs/backlog/entry.md "Upstream fixed it in 0123abc4567."
expect 0 "a hash that resolves to no commit: listed apart" "$r" \
  "Resolving to no commit — rewritten (in a fresh clone), another repository, or no hash:"
expect 0 "a hash that resolves to no commit: its line" "$r" "  - docs/backlog/entry.md:3: 0123abc4567"

r="$(repo lookalikes)"
cite "$r" docs/backlog/entry.md "A facade, a decade, issue 1234567, and the hex word deadbeefcafe."
expect 0 "words that only look like hashes: none counted" "$r" "0 hash(es) cited"

r="$(repo badhead)"
cite "$r" docs/backlog/entry.md "Nothing."
expect 2 "a head that does not resolve: exit 2" "$r" "'no-such-ref' does not resolve to a commit" \
  "" no-such-ref

summary "Cited commits sensor self-test"
