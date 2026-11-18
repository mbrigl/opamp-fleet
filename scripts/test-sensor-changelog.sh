#!/usr/bin/env bash
#
# Self-test of scripts/sensor-changelog.sh.
#
# Verifies: ADR-0002
#
# Each case builds a throwaway git repository under a temporary directory with a crate and a
# CHANGELOG.md on main, adds commits on a branch, runs the sensor against the branch, and asserts
# the exit code and a line a reader would meet. The cases form a table: a fix to a crate's src/
# without an entry, the same branch once a commit touches CHANGELOG.md, a feat to packaging/, a
# breaking change of another type by '!' and by its footer, commits of the types refactor, test,
# docs and chore, a fix that touches only tests, docs/ or scripts/, a merge that brings in a
# fix, nothing to compare, and a base or a head that does not resolve. Last, the pull request
# template's 'CHANGELOG.md:' line and the two lines of scripts/check-all.sh that run the sensor.
#
# Inputs: scripts/sensor-changelog.sh scripts/lib/ scripts/check-all.sh .github/PULL_REQUEST_TEMPLATE.md
#
# Usage:
#     scripts/test-sensor-changelog.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-changelog.sh"

# git_in <dir> <args...> — git in the fixture, with an author.
git_in() {
  fixture "$1"
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=fixture -c user.email=fixture@example.invalid "$@"
}

# repo <name> — a repository whose main holds a crate, a CHANGELOG.md, docs/ and scripts/; then
# a branch feat/x, checked out. Printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  mkdir -p "$dir/crates/opamp/src" "$dir/crates/opamp/tests" "$dir/docs" "$dir/scripts"
  echo "start" > "$dir/crates/opamp/src/lib.rs"
  echo "# Changelog" > "$dir/CHANGELOG.md"
  git_in "$dir" add .
  git_in "$dir" commit -q -m "start"
  git_in "$dir" switch -q -c feat/x
  echo "$dir"
}

# change <dir> <path> <message> — append a line to the file and commit it with that message.
change() {
  fixture "$1"
  mkdir -p "$(dirname "$1/$2")"
  echo "$3" >> "$1/$2"
  git_in "$1" add "$2"
  git_in "$1" commit -q -m "$3"
}

# expect <exit code> <name> <dir> <needle> [absent] [base] [head] — run the sensor from main, or
# the base given, to HEAD, or the head given; its output must contain the needle, and must not
# contain the second needle when one is given.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4" absent="${5:-}" base="${6:-main}" head="${7:-HEAD}"
  local out rc
  out="$(bash "$SENSOR" --root "$dir" "$base" "$head" 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* && ( -z "$absent" || "$out" != *"$absent"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\"${absent:+ and without \"$absent\"}, got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

r="$(repo fix)"
change "$r" crates/opamp/src/lib.rs "fix(opamp): hang up on a silent peer"
h="$(git_in "$r" rev-parse --short HEAD)"
expect 0 "a fix to a crate's src/ without an entry: listed" "$r" \
  "  - $h fix(opamp): hang up on a silent peer"
expect 0 "a fix to a crate's src/ without an entry: counted" "$r" \
  "1 commit(s) a deployment may see; the change does not touch CHANGELOG.md."
change "$r" CHANGELOG.md "docs(changelog): the silent peer"
expect 0 "the same branch with an entry: lists nothing" "$r" \
  "the change touches CHANGELOG.md." "  - "

r="$(repo packaging)"
change "$r" packaging/linux/postinst "feat(packaging): a new unit"
expect 0 "a feat to packaging/: listed" "$r" "  - " "" main HEAD
expect 0 "a feat to packaging/: its subject" "$r" "feat(packaging): a new unit"

r="$(repo bang)"
change "$r" crates/opamp/src/lib.rs "refactor(opamp)!: rename the wire field"
expect 0 "a breaking change by '!': listed" "$r" "refactor(opamp)!: rename the wire field"

r="$(repo footer)"
fixture "$r"
echo "more" >> "$r/crates/opamp/src/lib.rs"
git_in "$r" commit -q -a -m "refactor(opamp): rename the wire field" \
  -m "BREAKING CHANGE: the field is called agent_id now."
expect 0 "a breaking change by its footer: listed" "$r" "refactor(opamp): rename the wire field"

r="$(repo quiet)"
change "$r" crates/opamp/src/lib.rs "refactor(opamp): split the codec"
change "$r" crates/opamp/src/lib.rs "test(opamp): pin the codec"
change "$r" crates/opamp/src/lib.rs "docs(opamp): say what the codec does"
change "$r" crates/opamp/src/lib.rs "chore(opamp): format"
change "$r" crates/opamp/src/lib.rs "fixup the codec"
expect 0 "refactor, test, docs and chore commits: list nothing" "$r" \
  "0 commit(s) a deployment may see" "  - "

r="$(repo outside)"
change "$r" crates/opamp/tests/codec.rs "fix(opamp): the test's fixture"
change "$r" docs/guide.md "fix(docs): a broken link"
change "$r" scripts/check.sh "feat(scripts): a new check"
change "$r" crates/fleet-other/src/lib.rs "fix(other): a crate that ships nothing"
expect 0 "fixes that touch only tests, docs/ or scripts/: list nothing" "$r" \
  "0 commit(s) a deployment may see" "  - "

r="$(repo merged)"
git_in "$r" switch -q -c fix/side
change "$r" crates/opamp/src/lib.rs "fix(opamp): on the side"
git_in "$r" switch -q feat/x
git_in "$r" merge -q --no-ff -m "Merge branch 'fix/side'" fix/side
expect 0 "a merge that brings in a fix: the fix is listed, the merge is not" "$r" \
  "1 commit(s) a deployment may see" "Merge branch"

r="$(repo same)"
expect 0 "nothing to compare: says so" "$r" \
  "nothing to compare — base and head are the same commit." "" main main

r="$(repo badbase)"
expect 2 "a base that does not resolve: exit 2" "$r" "'no-such-ref' does not resolve to a commit" \
  "" no-such-ref
expect 2 "a head that does not resolve: exit 2" "$r" "'no-such-ref' does not resolve to a commit" \
  "" main no-such-ref

# The sensor's answer and its wiring, in this repository: the pull request template asks for the
# line the sensor's report names, and the local gate runs the sensor beside its self-test.
check "the pull request template has a 'CHANGELOG.md:' line under Documents and decisions" \
  grep -qx -- '- CHANGELOG.md:' <(sed -n '/^## Documents and decisions/,/^## /p' \
    "$ROOT/.github/PULL_REQUEST_TEMPLATE.md")
check "check-all.sh runs the sensor" grep -qx -- 'run "Changelog sensor" scripts/sensor-changelog.sh' \
  "$ROOT/scripts/check-all.sh"
check "check-all.sh runs the self-test" \
  grep -qx -- 'run "Changelog sensor self-test" scripts/test-sensor-changelog.sh' \
  "$ROOT/scripts/check-all.sh"

summary "Changelog sensor self-test"
