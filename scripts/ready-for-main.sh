#!/usr/bin/env bash
#
# Prepares a branch for its pull request into main to be made ready: step 7 of
# Integration branches in CONTRIBUTING.md, in the order the step needs. Nobody rewrites what has
# reached main, so a fixup is folded in before (NUC-main-only-via-pr §4).
#
#   1. Fetches origin and refuses while the branch carries a fixup the rework has not folded in
#      (a subject starting 'fixup!', 'squash!', or 'amend!'), since main keeps what it merges.
#      It refuses as well while the branch carries a merge commit origin/main lacks, since a feat/
#      branch carries none (NUC-main-only-via-pr §5) and a rebase would flatten it.
#   2. Takes up origin/main by a rebase onto it, unless the branch already contains it, so that the
#      branch stays linear (NUC-main-only-via-pr §5). A rebase that stops on a conflict stops this
#      script; once the rebase is continued to its end, it runs again.
#
# It pushes nothing and touches no pull request: it ends by naming the review again where it
# rebased, then the push, with --force-with-lease where the branch was reworked or rebased after
# its last push, and 'gh pr ready'. The pre-push hook runs the checks on what goes out.
#
# It refuses the checkout of main, a detached HEAD, a working tree with changes or untracked
# files, a merge or a rebase in progress, and a repository without origin or without origin/main.
#
# Pure bash + coreutils/grep, plus git.
#
# Usage:
#     scripts/ready-for-main.sh [--root <dir>]
# It prepares the repository it lives in; --root names another, as its self-test does with each
# fixture. Exit code 0 when the branch is ready to push, 1 when it refuses or a step stops, 2 on
# an unknown argument.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/ready-for-main.sh [--root <dir>]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
set -- "${ARGS[@]}"
rest_args "${BASH_SOURCE[0]}" "$usage" 0 "$@"

refuse() { echo "ready-for-main: refused — $1" >&2; exit 1; }

# A stopped rebase detaches HEAD, so it is named before the detached HEAD.
for dir in rebase-merge rebase-apply; do
  [[ ! -d "$(git rev-parse --git-path "$dir")" ]] \
    || refuse "a rebase is in progress; continue it with git rebase --continue, or abort it, then run this again"
done

branch="$(git symbolic-ref --short -q HEAD)" \
  || refuse "HEAD is detached; switch to the branch of the pull request into main"
[[ "$branch" != main ]] \
  || refuse "the checkout is on main; run this on the branch of the pull request into main"
! git rev-parse -q --verify MERGE_HEAD > /dev/null \
  || refuse "a merge is in progress; conclude it with git commit, or abort it, then run this again"
[[ -z "$(git status --porcelain)" ]] \
  || refuse "the working tree has changes or untracked files; commit or stash them first"
git remote get-url origin > /dev/null 2>&1 \
  || refuse "the repository has no remote origin, whose main the pull request merges into"
git fetch -q origin || refuse "git fetch origin failed"
git rev-parse -q --verify 'origin/main^{commit}' > /dev/null \
  || refuse "origin has no main"

# 1. The fixups, folded in before anything else moves.
mapfile -t fixups < <(git log --format='%h %s' origin/main..HEAD | grep -E '^[0-9a-f]+ (fixup|squash|amend)! ')
if ((${#fixups[@]})); then
  echo "ready-for-main: refused — the branch carries ${#fixups[@]} fixup(s) its rework has not folded in (CONTRIBUTING.md, Integration branches, step 4):" >&2
  printf '  %s\n' "${fixups[@]}" >&2
  exit 1
fi

# A merge on the branch, refused rather than flattened by the rebase below.
mapfile -t merges < <(git log --merges --format='%h %s' origin/main..HEAD)
if ((${#merges[@]})); then
  echo "ready-for-main: refused — the branch carries ${#merges[@]} merge commit(s) origin/main lacks; rebuild it linearly, one commit per topic (NUC-main-only-via-pr §5; CONTRIBUTING.md, Integration branches, step 7):" >&2
  printf '  %s\n' "${merges[@]}" >&2
  exit 1
fi

# 2. The remote's main, taken up.
review=""
if git merge-base --is-ancestor origin/main HEAD; then
  echo "ready-for-main: $branch already contains origin/main."
else
  git rebase -q origin/main \
    || refuse "the rebase onto origin/main stopped; resolve, git add, and git rebase --continue, or git rebase --abort, then run this again"
  echo "ready-for-main: $branch is rebased onto origin/main."
  review="review the rebased branch at its head (CONTRIBUTING.md, Integration branches, step 5), then "
fi

if upstream="$(git rev-parse -q --verify '@{upstream}' 2> /dev/null)"; then
  if git merge-base --is-ancestor "$upstream" HEAD; then
    push="git push"
  else
    push="git push --force-with-lease"
  fi
else
  push="git push -u origin $branch"
fi
echo "ready-for-main: ready. Next, on instruction: ${review}$push, then gh pr ready."
