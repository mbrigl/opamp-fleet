#!/usr/bin/env bash
#
# Self-test of scripts/sensor-implements-adrs.sh.
#
# Verifies: ADR-0002
#
# Each case builds a throwaway git repository under a temporary directory with ADRs under
# docs/adr/, adds commits on a branch whose messages say what they implement, runs the sensor
# against the branch, and asserts the exit code and a line a reader would meet. The cases form a
# table: an accepted ADR, a proposed one, a superseded one, a rejected one, a number the head does
# not have, a draft that exists, a draft numbered by its map to an accepted ADR, a draft with
# neither file nor number, a commit that implements nothing, an ADR named outside an Implements
# line, nothing to compare, and a base that does not resolve.
#
# Inputs: scripts/sensor-implements-adrs.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-implements-adrs.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-implements-adrs.sh"
# The draft identifiers of the fixtures are built here, so that the documentation check, which
# holds every draft identifier against docs/adr/, does not read them as references.
D="draft"
# The numbers too, so that a normalization, which moves every literal ADR identifier and file
# name of the tree (scripts/test-normalize-remap.sh), leaves the fixtures as they are.
# id <n> — the identifier ADR-NNNN of n; file <n> <slug> — the file name NNNN-<slug>.md.
id() { printf 'ADR-%04d' "$1"; }
file() { printf '%04d-%s.md' "$1" "$2"; }
A1="$(id 1)" A2="$(id 2)" A3="$(id 3)" A4="$(id 4)" A5="$(id 5)" A9="$(id 9)"
NEW="ADR-$D-a-new-rule" OLD="ADR-$D-the-old-rule" LOST="ADR-$D-nobody-wrote-it"

# git_in <dir> <args...> — git in the fixture, with an author.
git_in() {
  fixture "$1"
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=fixture -c user.email=fixture@example.invalid "$@"
}

# adr <dir> <file> <status> — write an ADR with that status line.
adr() {
  fixture "$1"
  mkdir -p "$1/docs/adr"
  printf '# ADR\n\n- **Status:** %s\n- **Date:** 2026-01-01\n' "$3" > "$1/docs/adr/$2"
}

# repo <name> — a repository whose main holds four ADRs: 1 accepted, 2 superseded by 1, 3
# rejected, 4 proposed; then a branch feat/x, checked out. Printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  adr "$dir" "$(file 1 one)" "🟢 accepted"
  adr "$dir" "$(file 2 two)" "⚪ superseded by [$A1]($(file 1 one))"
  adr "$dir" "$(file 3 three)" "🔴 rejected"
  adr "$dir" "$(file 4 four)" "🟡 proposed"
  git_in "$dir" add docs
  git_in "$dir" commit -q -m "the ADRs"
  git_in "$dir" switch -q -c feat/x
  echo "$dir"
}

# commit <dir> <message> — an empty commit with that message.
commit() { git_in "$1" commit -q --allow-empty -m "$2"; }

# expect <exit code> <name> <dir> <needle> [absent] [base] — run the sensor from main, or the
# base given, to HEAD; its output must contain the needle, and must not contain the second
# needle when one is given.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4" absent="${5:-}" base="${6:-main}"
  local out rc
  out="$(bash "$SENSOR" --root "$dir" "$base" HEAD 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* && ( -z "$absent" || "$out" != *"$absent"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\"${absent:+ and without \"$absent\"}, got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

r="$(repo in-force)"
commit "$r" "feat: one"$'\n\n'"Implements $A1."
commit "$r" "feat: four"$'\n\n'"- Implements: $A4, its second rule"
expect 0 "accepted and proposed: counted, none listed" "$r" \
  "2 commit(s) name an ADR they implement; 0 name one not in force."

r="$(repo superseded)"
commit "$r" "fix: the old way"$'\n\n'"Implements $A2"
expect 0 "a superseded ADR: listed with its successor" "$r" \
  "fix: the old way — $A2: superseded by $A1"

r="$(repo rejected)"
commit "$r" "fix: the rejected way"$'\n\n'"Implements $A3"
expect 0 "a rejected ADR: listed" "$r" "— $A3: rejected"

r="$(repo missing)"
commit "$r" "fix: a number from elsewhere"$'\n\n'"Implements $A9"
expect 0 "a number the head does not have: listed" "$r" "— $A9: no ADR of that number"

r="$(repo draft)"
adr "$r" "$D-a-new-rule.md" "🟡 proposed"
git_in "$r" add docs
commit "$r" "feat: the new rule"$'\n\nImplements '"$NEW"
expect 0 "a draft that exists: in force" "$r" "0 name one not in force."

r="$(repo numbered)"
adr "$r" "$D-a-new-rule.md" "🟡 proposed"
git_in "$r" add docs
commit "$r" "feat: the new rule"$'\n\nImplements '"$NEW"
git_in "$r" mv "docs/adr/$D-a-new-rule.md" "docs/adr/$(file 5 a-new-rule)"
adr "$r" "$(file 5 a-new-rule)" "🟢 accepted"
git_in "$r" add docs
commit "$r" "docs(adr): number the drafts"$'\n\n'"$NEW → $A5"
expect 0 "a draft numbered to an accepted ADR: in force through the map" "$r" \
  "0 name one not in force."

r="$(repo numbered-superseded)"
commit "$r" "feat: an old draft"$'\n\nImplements '"$OLD"
commit "$r" "docs(adr): number the drafts"$'\n\n'"$OLD → $A2"
expect 0 "a draft numbered to a superseded ADR: listed through the map" "$r" \
  "$OLD: superseded by $A1 (numbered $A2)"

r="$(repo no-draft)"
commit "$r" "feat: a lost draft"$'\n\nImplements '"$LOST"
expect 0 "a draft with neither file nor number: listed" "$r" \
  "$LOST: no draft of that name and no number for it"

r="$(repo nothing)"
commit "$r" $'chore: tidy\n\nImplements: none'
commit "$r" "docs: $A2 is cited here, on a line that implements nothing"
expect 0 "no Implements line naming an ADR: nothing counted" "$r" \
  "0 commit(s) name an ADR they implement" "$A2"

r="$(repo same)"
expect 0 "base and head the same: says so" "$r" \
  "nothing to compare — base and head are the same commit." "" HEAD

r="$(repo badbase)"
commit "$r" "feat: x"
expect 2 "a base that does not resolve: exit 2" "$r" "'no-such-ref' does not resolve to a commit" \
  "" no-such-ref

summary "Implements sensor self-test"
