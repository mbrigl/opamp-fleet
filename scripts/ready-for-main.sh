#!/usr/bin/env bash
#
# Prepares a branch for its pull request into main to be made ready: step 7 of the README's
# Integration branches, in the order the step needs.
#
#   1. Fetches origin and refuses while the branch carries a fixup the rework has not folded in
#      (a subject starting 'fixup!', 'squash!', or 'amend!'), since main keeps what it merges.
#   2. Takes up origin/main by a merge, unless the branch already contains it. A merge that stops
#      on a conflict, or that the pre-merge-commit hook refuses, stops this script; once the merge
#      is concluded, it runs again.
#   3. Numbers the draft ADRs with scripts/number-adrs.sh (ADR-0002), which commits twice.
#
# It pushes nothing and touches no pull request: it ends by naming the push, with
# --force-with-lease where the branch was reworked after its last push, and 'gh pr ready'. The
# pre-push hook runs the checks on what goes out.
#
# It refuses the checkout of main, a detached HEAD, a working tree with changes or untracked
# files, a merge in progress, and a repository without origin or without origin/main.
#
# Pure bash + coreutils/grep, plus git.
#
# Usage:
#     scripts/ready-for-main.sh [--root <dir>]
# It prepares the repository it lives in; --root names another, as its self-test does with each
# fixture. Exit code 0 when the branch is ready to push, 1 when it refuses or a step stops, 2 on
# an unknown argument.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"
numbering="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/number-adrs.sh"

enter_root "ready-for-main" "${BASH_SOURCE[0]}" "$@"
shift "$ROOT_TAKEN"
if (($#)); then
  echo "ready-for-main: unknown argument '$1' — usage: scripts/ready-for-main.sh [--root <dir>]" >&2
  exit 2
fi

refuse() { echo "ready-for-main: refused — $1" >&2; exit 1; }

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
  echo "ready-for-main: refused — the branch carries ${#fixups[@]} fixup(s) its rework has not folded in (README, Integration branches, step 4):" >&2
  printf '  %s\n' "${fixups[@]}" >&2
  exit 1
fi

# 2. The remote's main, taken up.
if git merge-base --is-ancestor origin/main HEAD; then
  echo "ready-for-main: $branch already contains origin/main."
else
  git merge -q --no-edit origin/main \
    || refuse "the merge of origin/main stopped; resolve and conclude it with git commit, or abort it, then run this again"
  echo "ready-for-main: $branch took up origin/main."
fi

# 3. The drafts, numbered.
bash "$numbering" --root "$PWD" || exit 1

if upstream="$(git rev-parse -q --verify '@{upstream}' 2> /dev/null)"; then
  if git merge-base --is-ancestor "$upstream" HEAD; then
    push="git push"
  else
    push="git push --force-with-lease"
  fi
else
  push="git push -u origin $branch"
fi
echo "ready-for-main: ready. Next, on instruction: $push, then gh pr ready."
