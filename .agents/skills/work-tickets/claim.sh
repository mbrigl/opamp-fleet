#!/usr/bin/env bash
#
# Claims a ticket under docs/plan/ by its branch, ticket/<spec>/<NN>-<slug> (ADR-0006).
#
# Without a remote, the claim is the local branch, created at main: it holds among the sessions
# that share this repository, its worktrees included, because git refuses a branch that exists.
#
# With a remote, the claim is made on the remote first, so that it holds among clones on other
# machines too. The push creates the branch and nothing else: '--force-with-lease=<ref>:' with
# an empty value lets it through only while the branch does not exist there. It pushes an empty
# commit on top of the remote's main rather than main itself, because a push of a commit the
# branch already points at reports success without checking the lease, and two clones claiming
# at the same main would both win; a nonce keeps two claims made in the same second apart. The
# push skips the pre-push hook: the commit carries the tree of the remote's main unchanged, so
# the checks have nothing to decide on it, and running the build for a claim costs minutes.
# Only the harness ADR grants this push (AGENTS.md, section 6). A claim given up is released by
# deleting its branch where it was made; on a remote that is a push the grant does not cover,
# so a human makes it.
#
# The ticket must be on main, local or the remote's: the worker's worktree starts from there,
# and other clones read the frontier from there.
#
# Pure bash + git. Usage, anywhere in the repository the dispatcher works in, with the ticket's
# path from the repository's root:
#     .agents/skills/work-tickets/claim.sh docs/plan/<spec>/<NN>-<slug>.md [remote]
# Prints the branch on success. Exit code 0 when this session holds the claim, 2 when another
# session was first, 1 on anything else.
set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=../../../scripts/lib/ticket.sh
source "$(dirname "${BASH_SOURCE[0]}")/../../../scripts/lib/ticket.sh"

ticket="${1:-}" remote="${2:-}"

top="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "claim: not inside a git repository" >&2; exit 1; }
cd "$top" || exit 1

if [[ ! "$ticket" =~ $TICKET_PATH_RE ]]; then
  echo "claim: '$ticket' is no ticket path, docs/plan/<spec>/<NN>-<slug>.md" >&2
  exit 1
fi
branch="$(ticket_branch "$ticket")"

if git show-ref --verify --quiet "refs/heads/$branch"; then
  echo "claim: $branch exists here — another session was first" >&2
  exit 2
fi

# The base is main as the claim is made from it: the local one, or the remote's as just fetched.
# FETCH_HEAD rather than <remote>/main, which a single-branch clone or a narrowed refspec never
# updates, and which a URL in place of a remote name does not have.
if [[ -z "$remote" ]]; then
  base="$(git rev-parse --verify -q "main^{commit}")" \
    || { echo "claim: there is no main to claim from" >&2; exit 1; }
else
  git fetch --quiet "$remote" main || exit 1
  base="$(git rev-parse --verify -q "FETCH_HEAD^{commit}")" || exit 1
fi
# The worker's worktree starts from the base, and other clones read the frontier from it, so a
# ticket that is not on main yet is not claimable.
if ! git cat-file -e "$base:$ticket" 2>/dev/null; then
  echo "claim: $ticket is not on main${remote:+ of $remote} — land the plan first" >&2
  exit 1
fi

if [[ -z "$remote" ]]; then
  git branch "$branch" "$base" || exit 1
  echo "$branch"
  exit 0
fi

nonce="$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
commit="$(git commit-tree "$base^{tree}" -p "$base" \
  -m "chore: claim $ticket" -m "Claim-Id: $nonce")" || exit 1

if ! git push --quiet --no-verify --force-with-lease="refs/heads/$branch:" \
  "$remote" "$commit:refs/heads/$branch"; then
  if git ls-remote --exit-code --heads "$remote" "refs/heads/$branch" >/dev/null 2>&1; then
    echo "claim: $branch exists on $remote — another session was first" >&2
    exit 2
  fi
  echo "claim: the push of $branch to $remote failed" >&2
  exit 1
fi

git branch "$branch" "$commit" || exit 1
echo "$branch"
exit 0
