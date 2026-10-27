#!/usr/bin/env bash
#
# Self-test of scripts/ready-for-main.sh. Verifies: ADR-0002
#
# Each case builds a throwaway repository under a temporary directory, with a bare repository as
# its origin: a main with one numbered ADR, a branch feat/work that adds a draft, and an origin
# main that gains a number of its own meanwhile. The script runs on the branch, and the case
# asserts that it took up origin/main, numbered the draft after it, and named the push the
# branch needs. The refusals and the merge that stops on a conflict have a case each.
#
# Draft identifiers and ADR numbers inside fixtures are assembled, never written literally, so
# that scripts/check-docs.sh does not read this script's fixtures as references into this
# repository.
#
# Inputs: scripts/ready-for-main.sh scripts/number-adrs.sh scripts/lib/ .gitattributes .githooks/merge-adr-index
#
# Usage:
#     scripts/test-ready-for-main.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SCRIPT="$ROOT/scripts/ready-for-main.sh"

D="draft"
# id <number> — the identifier ADR-NNNN; draft <slug> — the draft identifier of a slug.
id() { printf 'ADR-%04d' "$1"; }
draft() { printf 'ADR-%s-%s' "$D" "$1"; }

# g <dir> <git args...> — git in a fixture.
g() {
  local dir="$1"; shift
  fixture "$dir"
  git -C "$dir" "$@"
}

# adr <file> <identifier> — write one ADR.
adr() {
  { echo "# $2: decision"; echo; echo "- **Status:** 🟡 proposed"
    echo; echo "## Decision"; echo; echo "We will."; } > "$1"
}

# repo <name> — the fixture: main with ADR 1 pushed to origin, the branch feat/work with one
# draft, and origin's main gaining ADR 2 meanwhile. A note both sides change, the branch to
# 'branch' and main to 'main', makes the take-up conflict when the second argument is 'conflict'.
repo() {
  local dir="$work/$1"
  git init -q --bare -b main "$work/$1-origin.git"
  mkdir -p "$dir/docs/adr"
  git init -q -b main "$dir"
  g "$dir" config user.name t
  g "$dir" config user.email t@t
  g "$dir" config commit.gpgsign false
  g "$dir" config merge.adr-index.driver "$ROOT/.githooks/merge-adr-index %O %A %B %S %X %Y"
  cp "$ROOT/.gitattributes" "$dir/.gitattributes"
  adr "$dir/docs/adr/0001-first.md" "$(id 1)"
  printf '| ADR |\n|-----|\n| [0001](0001-first.md) |\n' > "$dir/docs/adr/README.md"
  echo "note" > "$dir/note.md"
  g "$dir" add -A && g "$dir" commit -q -m main
  g "$dir" remote add origin "$work/$1-origin.git"
  g "$dir" push -q origin main

  g "$dir" switch -q -c feat/work
  adr "$dir/docs/adr/$D-work.md" "$(draft work)"
  echo "| [$D-work]($D-work.md) |" >> "$dir/docs/adr/README.md"
  [[ "${2:-}" == conflict ]] && echo "branch" > "$dir/note.md"
  g "$dir" add -A && g "$dir" commit -q -m "a draft"

  g "$dir" switch -q main
  adr "$dir/docs/adr/0002-second.md" "$(id 2)"
  echo "| [0002](0002-second.md) |" >> "$dir/docs/adr/README.md"
  [[ "${2:-}" == conflict ]] && echo "main" > "$dir/note.md"
  g "$dir" add -A && g "$dir" commit -q -m "another branch landed"
  g "$dir" push -q origin main
  g "$dir" switch -q feat/work
  echo "$dir"
}

# --- cases: ready -------------------------------------------------------------------------

d="$(repo ready)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "the script succeeds" test "$rc" -eq 0
check "the branch contains origin/main" g "$d" merge-base --is-ancestor origin/main HEAD
check "the take-up is a merge, before the two numbering commits" \
  test "$(g "$d" rev-list --parents -n 1 HEAD~2 | wc -w)" -eq 3
check "the draft takes the number after origin's" \
  grep -q "^# $(id 3): " "$d/docs/adr/0003-work.md"
check "no draft file is left" test -z "$(find "$d/docs/adr" -name "$D-*")"
check "the working tree is clean" test -z "$(g "$d" status --porcelain)"
check "a branch never pushed is pushed with its upstream" \
  grep -qF "Next, on instruction: git push -u origin feat/work, then gh pr ready." <<< "$out"

# Pushed before, and only added to since: a plain push.
d="$(repo pushed)"
g "$d" push -q -u origin feat/work
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a pushed branch succeeds" test "$rc" -eq 0
check "a pushed branch only added to is pushed plainly" \
  grep -qF "Next, on instruction: git push, then gh pr ready." <<< "$out"

# Pushed before, then reworked: a push with lease.
d="$(repo reworked)"
g "$d" push -q -u origin feat/work
g "$d" commit -q --amend -m "a draft, reworded"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a reworked branch succeeds" test "$rc" -eq 0
check "a reworked branch is pushed with lease" \
  grep -qF "Next, on instruction: git push --force-with-lease, then gh pr ready." <<< "$out"

# Already containing origin/main: no merge, only the numbering.
d="$(repo current)"
g "$d" merge -q --no-edit origin/main > /dev/null
start="$(g "$d" rev-parse HEAD)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a branch that contains origin/main succeeds" test "$rc" -eq 0
check "it says so" grep -qF "feat/work already contains origin/main." <<< "$out"
check "and adds only the two numbering commits" \
  test "$(g "$d" rev-list --count "$start..HEAD")" -eq 2

# --- cases: stops and refusals ------------------------------------------------------------

d="$(repo fixup)"
echo "fixed" >> "$d/docs/adr/$D-work.md"
g "$d" commit -q -a --fixup HEAD
start="$(g "$d" rev-parse HEAD)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a fixup not folded in refuses" test "$rc" -eq 1
check "the refusal names the fixup" grep -q "fixup! a draft" <<< "$out"
check "a refusal moves nothing" test "$(g "$d" rev-parse HEAD)" = "$start"

d="$(repo conflict conflict)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a take-up that conflicts stops" test "$rc" -eq 1
check "the stop says how to go on" grep -qF "the merge of origin/main stopped" <<< "$out"
check "the draft stays unnumbered" test -f "$d/docs/adr/$D-work.md"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a merge in progress refuses" test "$rc" -eq 1
check "the refusal names the merge in progress" grep -qF "a merge is in progress" <<< "$out"

d="$(repo onmain)"
g "$d" switch -q main
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "the checkout of main refuses" test "$rc" -eq 1
check "the refusal names main" grep -qF "the checkout is on main" <<< "$out"

d="$(repo dirty)"
echo "stray" > "$d/stray.md"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "an untracked file refuses" test "$rc" -eq 1
check "the refusal names the working tree" grep -qF "the working tree has changes" <<< "$out"

d="$(repo detached)"
g "$d" switch -q --detach
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a detached HEAD refuses" test "$rc" -eq 1
check "the refusal names the detached HEAD" grep -qF "HEAD is detached" <<< "$out"

# An origin that has no main: an empty one, and no origin/main left from an earlier fetch.
d="$(repo nomain)"
git init -q --bare -b main "$work/nomain-empty.git"
g "$d" remote set-url origin "$work/nomain-empty.git"
g "$d" update-ref -d refs/remotes/origin/main
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "an origin without main refuses" test "$rc" -eq 1
check "the refusal names origin's main" grep -qF "origin has no main" <<< "$out"

d="$(repo noremote)"
g "$d" remote remove origin
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a repository without origin refuses" test "$rc" -eq 1
check "the refusal names origin" grep -qF "no remote origin" <<< "$out"

bash "$SCRIPT" --bogus > /dev/null 2>&1; rc=$?
check "an unknown argument exits 2" test "$rc" -eq 2

summary "Readiness script self-test"
