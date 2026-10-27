#!/usr/bin/env bash
#
# Takes up a branch whose pull request into main has merged, and drops it: step 8 of the
# README's Integration branches.
#
#   1. Fetches origin, pruning, and refuses unless the branch's head is one origin/main has, so
#      that dropping it loses no commit.
#   2. Switches to main and takes up origin/main with 'git merge --ff-only origin/main'; a local
#      main that origin/main does not contain refuses before the switch.
#   3. Deletes the local branch with 'git branch -d'.
#
# It pushes nothing: it ends by naming the deletion of the remote's branch, when origin still has
# it and its head is one origin/main has; a remote branch that gained commits after the merge is
# named, not offered for deletion.
#
# It refuses the checkout of main, a detached HEAD, a working tree with changes or untracked
# files, a merge in progress, and a repository without origin or without origin/main.
#
# Pure bash + coreutils, plus git.
#
# Usage:
#     scripts/drop-merged-branch.sh [--root <dir>]
# It runs on the merged branch in the repository it lives in; --root names another, as its
# self-test does with each fixture. Exit code 0 when the branch is dropped, 1 when it refuses or a
# step stops, 2 on an unknown argument.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

enter_root "drop-merged-branch" "${BASH_SOURCE[0]}" "$@"
shift "$ROOT_TAKEN"
if (($#)); then
  echo "drop-merged-branch: unknown argument '$1' — usage: scripts/drop-merged-branch.sh [--root <dir>]" >&2
  exit 2
fi

refuse() { echo "drop-merged-branch: refused — $1" >&2; exit 1; }

branch="$(git symbolic-ref --short -q HEAD)" \
  || refuse "HEAD is detached; switch to the branch whose pull request merged"
[[ "$branch" != main ]] \
  || refuse "the checkout is on main; run this on the branch whose pull request merged"
! git rev-parse -q --verify MERGE_HEAD > /dev/null \
  || refuse "a merge is in progress; conclude it with git commit, or abort it, then run this again"
[[ -z "$(git status --porcelain)" ]] \
  || refuse "the working tree has changes or untracked files; commit or stash them first"
git remote get-url origin > /dev/null 2>&1 \
  || refuse "the repository has no remote origin, whose main the pull request merged into"
git fetch -q --prune origin || refuse "git fetch origin failed"
git rev-parse -q --verify 'origin/main^{commit}' > /dev/null \
  || refuse "origin has no main"

# 1. The branch, merged.
git merge-base --is-ancestor HEAD origin/main \
  || refuse "origin/main does not contain $branch; its pull request has not merged, or the branch gained commits after it"

# 2. The remote's main, taken up.
if git rev-parse -q --verify 'refs/heads/main' > /dev/null; then
  git merge-base --is-ancestor main origin/main \
    || refuse "the local main has commits origin/main lacks; it only takes up the remote's (AGENTS.md section 6)"
fi
git switch -q main || refuse "git switch main failed"
git merge -q --ff-only origin/main || refuse "git merge --ff-only origin/main failed"
echo "drop-merged-branch: main took up origin/main."

# 3. The branch, dropped.
git branch -q -d "$branch" || refuse "git branch -d $branch failed"
echo "drop-merged-branch: deleted the local $branch."

if remote="$(git rev-parse -q --verify "refs/remotes/origin/$branch" 2> /dev/null)"; then
  if git merge-base --is-ancestor "$remote" origin/main; then
    echo "drop-merged-branch: done. Next, on instruction: git push origin --delete $branch."
  else
    echo "drop-merged-branch: done. origin/$branch has commits origin/main lacks; it stays."
  fi
else
  echo "drop-merged-branch: done. origin has no $branch left."
fi
