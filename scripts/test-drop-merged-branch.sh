#!/usr/bin/env bash
#
# Self-test of scripts/drop-merged-branch.sh. Verifies: ADR-0002
#
# Each case builds a throwaway repository under a temporary directory, with a bare repository as
# its origin: a main pushed to origin, a branch feat/work pushed with one commit, and origin's
# main merging it as a merge commit, as a pull request does. The script runs on the branch, and
# the case asserts that main took up origin/main, the local branch is gone, and the deletion of
# the remote's branch is named or not. The refusals have a case each.
#
# Inputs: scripts/drop-merged-branch.sh scripts/lib/
#
# Usage:
#     scripts/test-drop-merged-branch.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SCRIPT="$ROOT/scripts/drop-merged-branch.sh"

# g <dir> <git args...> — git in a fixture.
g() {
  local dir="$1"; shift
  fixture "$dir"
  git -C "$dir" "$@"
}

# absent <dir> <ref> — the ref does not exist in the fixture.
# shellcheck disable=SC2317,SC2329 # run by check
absent() { ! g "$1" rev-parse -q --verify "$2" > /dev/null; }

# repo <name> [unmerged] — the fixture: main pushed to origin, feat/work pushed with one commit,
# and origin's main merging it by a merge commit, made in a second clone, unless the second
# argument is 'unmerged'. The checkout stays on feat/work, its local main behind origin's.
repo() {
  local dir="$work/$1" other="$work/$1-other"
  git init -q --bare -b main "$work/$1-origin.git"
  git init -q -b main "$dir"
  g "$dir" config user.name t
  g "$dir" config user.email t@t
  g "$dir" config commit.gpgsign false
  echo "note" > "$dir/note.md"
  g "$dir" add -A && g "$dir" commit -q -m main
  g "$dir" remote add origin "$work/$1-origin.git"
  g "$dir" push -q origin main

  g "$dir" switch -q -c feat/work
  echo "work" > "$dir/work.md"
  g "$dir" add -A && g "$dir" commit -q -m work
  g "$dir" push -q -u origin feat/work

  if [[ "${2:-}" != unmerged ]]; then
    git clone -q "$work/$1-origin.git" "$other"
    g "$other" config user.name t
    g "$other" config user.email t@t
    g "$other" config commit.gpgsign false
    g "$other" merge -q --no-ff -m "Merge pull request" origin/feat/work
    g "$other" push -q origin main
  fi
  echo "$dir"
}

# --- cases: dropped -----------------------------------------------------------------------

d="$(repo merged)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "the script succeeds" test "$rc" -eq 0
check "the checkout is on main" test "$(g "$d" symbolic-ref --short HEAD)" = main
check "main is origin/main" test "$(g "$d" rev-parse main)" = "$(g "$d" rev-parse origin/main)"
check "the local branch is gone" absent "$d" refs/heads/feat/work
check "the remote's branch is named for deletion" \
  grep -qF "Next, on instruction: git push origin --delete feat/work." <<< "$out"
check "the remote's branch stays" test -n "$(g "$d" rev-parse -q --verify refs/remotes/origin/feat/work)"

# The remote's branch deleted on merge, as GitHub may: nothing left to push.
d="$(repo pruned)"
g "$work/pruned-other" push -q origin --delete feat/work
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a branch the remote dropped succeeds" test "$rc" -eq 0
check "it names no push" grep -qF "origin has no feat/work left." <<< "$out"
check "the fetch pruned its remote-tracking ref" \
  absent "$d" refs/remotes/origin/feat/work

# The remote's branch gained a commit after the merge: kept, and said so.
d="$(repo grown)"
g "$work/grown-other" switch -q -c feat/work origin/feat/work
echo "late" > "$work/grown-other/late.md"
g "$work/grown-other" add -A && g "$work/grown-other" commit -q -m late
g "$work/grown-other" push -q origin feat/work
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a remote branch grown after the merge still drops the local one" test "$rc" -eq 0
check "it is not offered for deletion" grep -qF "origin/feat/work has commits origin/main lacks; it stays." <<< "$out"

# --- cases: refusals ----------------------------------------------------------------------

d="$(repo unmerged unmerged)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a branch origin/main lacks refuses" test "$rc" -eq 1
check "the refusal names it" grep -qF "origin/main does not contain feat/work" <<< "$out"
check "a refusal keeps the branch and the checkout" \
  test "$(g "$d" symbolic-ref --short HEAD)" = feat/work

d="$(repo late)"
echo "late" > "$d/late.md"
g "$d" add -A && g "$d" commit -q -m late
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a commit after the merge refuses" test "$rc" -eq 1
check "the refusal names the commits after it" grep -qF "gained commits after it" <<< "$out"

d="$(repo ahead)"
g "$d" switch -q main
echo "local" > "$d/local.md"
g "$d" add -A && g "$d" commit -q -m local
g "$d" switch -q feat/work
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a local main ahead of origin's refuses" test "$rc" -eq 1
check "the refusal names the local main" grep -qF "the local main has commits origin/main lacks" <<< "$out"
check "it refuses before the switch" test "$(g "$d" symbolic-ref --short HEAD)" = feat/work

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

d="$(repo merging)"
g "$d" fetch -q origin
g "$d" merge -q --no-commit --no-ff origin/main > /dev/null 2>&1
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a merge in progress refuses" test "$rc" -eq 1
check "the refusal names the merge in progress" grep -qF "a merge is in progress" <<< "$out"

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

summary "Drop script self-test"
