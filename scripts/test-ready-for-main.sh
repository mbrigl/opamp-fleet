#!/usr/bin/env bash
#
# Self-test of scripts/ready-for-main.sh.
# Verifies: NUC-main-only-via-pr NUC-checks-are-bash
#
# Each case builds a throwaway repository under a temporary directory, with a bare repository as
# its origin: a main with one ADR, a branch feat/work that adds another, and an origin main that
# gains a third meanwhile. The script runs on the branch, and the case asserts that it rebased the
# branch onto origin/main, left the identifiers as they were, and named the push the branch needs.
# The refusals and the rebase that stops on a conflict have a case each.
#
# Identifiers inside fixtures are assembled, never written literally, so that
# scripts/check-docs.sh does not read this script's fixtures as references into this repository.
#
# Inputs: scripts/ready-for-main.sh scripts/lib/ .gitattributes .githooks/merge-adr-index
#
# Usage:
#     scripts/test-ready-for-main.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SCRIPT="$ROOT/scripts/ready-for-main.sh"

# id <slug> — the identifier of an ADR of the new form.
id() { printf '%s-%s' OPS "$1"; }

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

# repo <name> — the fixture: main with one ADR pushed to origin, the branch feat/work with
# another, and origin's main gaining a third meanwhile. A note both sides change, the branch to
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
  adr "$dir/docs/adr/$(id first).md" "$(id first)"
  printf '| ADR |\n|-----|\n| [%s](%s.md) |\n' "$(id first)" "$(id first)" > "$dir/docs/adr/README.md"
  echo "note" > "$dir/note.md"
  g "$dir" add -A && g "$dir" commit -q -m main
  g "$dir" remote add origin "$work/$1-origin.git"
  g "$dir" push -q origin main

  g "$dir" switch -q -c feat/work
  adr "$dir/docs/adr/$(id work).md" "$(id work)"
  echo "| [$(id work)]($(id work).md) |" >> "$dir/docs/adr/README.md"
  [[ "${2:-}" == conflict ]] && echo "branch" > "$dir/note.md"
  g "$dir" add -A && g "$dir" commit -q -m "an ADR"

  g "$dir" switch -q main
  adr "$dir/docs/adr/$(id second).md" "$(id second)"
  echo "| [$(id second)]($(id second).md) |" >> "$dir/docs/adr/README.md"
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
check "the take-up is a rebase: the branch carries no merge" \
  test -z "$(g "$d" rev-list --merges origin/main..HEAD)"
check "the branch's commit sits on origin/main" \
  test "$(g "$d" rev-parse HEAD~1)" = "$(g "$d" rev-parse origin/main)"
check "the branch's ADR keeps its identifier" grep -q "^# $(id work): " "$d/docs/adr/$(id work).md"
check "the index lists all three" test "$(grep -c '^| \[' "$d/docs/adr/README.md")" -eq 3
check "the working tree is clean" test -z "$(g "$d" status --porcelain)"
check "a branch never pushed is pushed with its upstream" \
  grep -qF "Next, on instruction: review the rebased branch at its head (CONTRIBUTING.md, Integration branches, step 5), then git push -u origin feat/work, then gh pr ready." <<< "$out"

# Pushed before, and only added to since: a plain push.
d="$(repo pushed)"
g "$d" rebase -q origin/main
g "$d" push -q -u origin feat/work
echo "more" >> "$d/note.md"
g "$d" commit -q -a -m "more"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a pushed branch succeeds" test "$rc" -eq 0
check "a pushed branch only added to is pushed plainly" \
  grep -qF "Next, on instruction: git push, then gh pr ready." <<< "$out"

# Pushed before, then reworked: a push with lease.
d="$(repo reworked)"
g "$d" push -q -u origin feat/work
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a reworked branch succeeds" test "$rc" -eq 0
check "a branch the take-up rebased is pushed with lease" \
  grep -qF "Next, on instruction: review the rebased branch at its head (CONTRIBUTING.md, Integration branches, step 5), then git push --force-with-lease, then gh pr ready." <<< "$out"

# Rebased and pushed, then reworked by an amend: a push with lease.
d="$(repo amended)"
g "$d" rebase -q origin/main
g "$d" push -q -u origin feat/work
g "$d" commit -q --amend -m "an ADR, reworded"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "an amended branch succeeds" test "$rc" -eq 0
check "an amended branch is pushed with lease" \
  grep -qF "Next, on instruction: git push --force-with-lease, then gh pr ready." <<< "$out"

# Already containing origin/main: nothing to do.
d="$(repo current)"
g "$d" rebase -q origin/main
start="$(g "$d" rev-parse HEAD)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a branch that contains origin/main succeeds" test "$rc" -eq 0
check "it says so" grep -qF "feat/work already contains origin/main." <<< "$out"
check "and adds no commit" test "$(g "$d" rev-parse HEAD)" = "$start"

# --- cases: stops and refusals ------------------------------------------------------------

d="$(repo fixup)"
echo "fixed" >> "$d/docs/adr/$(id work).md"
g "$d" commit -q -a --fixup HEAD
start="$(g "$d" rev-parse HEAD)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a fixup not folded in refuses" test "$rc" -eq 1
check "the refusal names the fixup" grep -q "fixup! an ADR" <<< "$out"
check "a refusal moves nothing" test "$(g "$d" rev-parse HEAD)" = "$start"

d="$(repo merged)"
g "$d" merge -q --no-edit origin/main > /dev/null
start="$(g "$d" rev-parse HEAD)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a merge on the branch refuses" test "$rc" -eq 1
check "the refusal names the merge" grep -qF "$(g "$d" rev-parse --short HEAD) Merge" <<< "$out"
check "the refusal says to rebuild linearly" grep -qF "rebuild it linearly" <<< "$out"
check "a refused merge moves nothing" test "$(g "$d" rev-parse HEAD)" = "$start"

d="$(repo conflict conflict)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a take-up that conflicts stops" test "$rc" -eq 1
check "the stop says how to go on" grep -qF "the rebase onto origin/main stopped" <<< "$out"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a rebase in progress refuses" test "$rc" -eq 1
check "the refusal names the rebase in progress" grep -qF "a rebase is in progress" <<< "$out"

d="$(repo merging conflict)"
g "$d" merge -q --no-edit origin/main > /dev/null 2>&1
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
