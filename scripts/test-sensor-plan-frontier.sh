#!/usr/bin/env bash
#
# Self-test of scripts/sensor-plan-frontier.sh. Verifies: ADR-0006
#
# Each case builds a throwaway git repository under a temporary directory with tickets under
# docs/plan/, runs the sensor against it, and asserts the exit code and a line a reader would
# meet. The cases cover an empty plan, the three lists and the mode a frontier ticket carries, a
# claim by a local and by a remote branch, a blocker closed by deleting its file, a path inside
# an HTML comment, the spec that is no ticket, and the tickets the sensor cannot place: without
# a section it reads, or with a blocker that is no path, and a plan read on a feat/ base. The
# claim script of the work-tickets skill is exercised with it: a claim in one repository, a claim
# on a remote that a second clone's claim of the same ticket loses, also when the branch there
# points at main itself, and a claim from a feat/ base, on the remote where it is there and by
# the local branch where it is not.
#
# Verifies: ADR-0002
# Inputs: scripts/sensor-plan-frontier.sh .agents/skills/work-tickets/claim.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-plan-frontier.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-plan-frontier.sh"
CLAIM="$ROOT/.agents/skills/work-tickets/claim.sh"

# repo <name> — an empty repository with one commit, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  git -C "${dir:?}" -c user.name=fixture -c user.email=fixture@example.invalid commit -q --allow-empty -m "start"
  echo "$dir"
}

# ticket <dir> <path> <blocked by> <mode> — write a ticket in the template's shape.
ticket() {
  fixture "$1"
  mkdir -p "$(dirname "$1/$2")"
  printf '# Ticket: fixture\n\n## What to build\n\nSomething.\n\n## Blocked by\n\n<!-- By path under docs/plan/,\n     such as docs/plan/s/09-never.md. -->\n\n- %s\n\n## Mode\n\n<!-- unattended only under AGENTS.md §4 -->\n\n%s\n' \
    "$3" "$4" > "$1/$2"
}

# expect <exit code> <name> <dir> <needle> [sensor args] — run the sensor; its output must
# contain the needle.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4"
  shift 4
  local out rc
  fixture "$dir"
  out="$(bash "$SENSOR" "$dir" "$@" 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

d="$(repo empty)"
expect 0 "no docs/plan/ is no open ticket" "$d" "no open tickets under docs/plan/"

d="$(repo lists)"
printf '# Feature spec: fixture\n' > "$d/spec.tmp"
mkdir -p "$d/docs/plan/s" && mv "$d/spec.tmp" "$d/docs/plan/s/spec.md"
ticket "$d" docs/plan/s/01-first.md "None (can start immediately)" "unattended: a test decides it"
ticket "$d" docs/plan/s/02-second.md "docs/plan/s/01-first.md" "unattended"
ticket "$d" docs/plan/s/03-third.md "None (can start immediately)" "with the human, because it touches a public interface"
expect 0 "the spec is no ticket, and the counts add up" "$d" "3 open, 2 on the frontier, 0 claimed, 1 blocked."
expect 0 "an unblocked ticket is on the frontier with its mode" "$d" "- docs/plan/s/01-first.md — unattended"
expect 0 "a ticket worked with the human says so" "$d" "- docs/plan/s/03-third.md — with the human"
expect 0 "a ticket whose blocker is open is blocked by it" "$d" "- docs/plan/s/02-second.md — by docs/plan/s/01-first.md"

git -C "${d:?}" branch ticket/s/01-first
expect 0 "a local branch claims its ticket" "$d" "- docs/plan/s/01-first.md — ticket/s/01-first"
expect 0 "a claimed ticket leaves the frontier" "$d" "3 open, 1 on the frontier, 1 claimed, 1 blocked."

git -C "${d:?}" update-ref refs/remotes/origin/ticket/s/03-third HEAD
expect 0 "a remote branch claims its ticket" "$d" "- docs/plan/s/03-third.md — ticket/s/03-third"

rm "$d/docs/plan/s/01-first.md"
git -C "${d:?}" branch -q -D ticket/s/01-first
expect 0 "a deleted blocker is closed" "$d" "- docs/plan/s/02-second.md — unattended"

d="$(repo comment)"
ticket "$d" docs/plan/s/09-never.md "None (can start immediately)" "with the human"
ticket "$d" docs/plan/s/01-first.md "None (can start immediately)" "unattended"
expect 0 "a path inside an HTML comment blocks nothing" "$d" "2 on the frontier, 0 claimed, 0 blocked."

d="$(repo unreadable)"
ticket "$d" docs/plan/s/01-first.md "None (can start immediately)" "unattended"
sed '/^## Mode$/,$d' "$d/docs/plan/s/01-first.md" > "$d/t.tmp" && mv "$d/t.tmp" "$d/docs/plan/s/01-first.md"
expect 1 "a ticket without a Mode section fails" "$d" "docs/plan/s/01-first.md: no '## Mode' section"

d="$(repo no-blocked-by)"
ticket "$d" docs/plan/s/01-first.md "None (can start immediately)" "unattended"
sed '/^## Blocked by$/,/^## Mode$/{/^## Mode$/!d;}' "$d/docs/plan/s/01-first.md" > "$d/t.tmp" && mv "$d/t.tmp" "$d/docs/plan/s/01-first.md"
expect 1 "a ticket without a Blocked by section fails" "$d" "docs/plan/s/01-first.md: no '## Blocked by' section"

d="$(repo link-blocker)"
ticket "$d" docs/plan/s/01-first.md "None (can start immediately)" "unattended"
ticket "$d" docs/plan/s/02-second.md "[01-first](01-first.md)" "unattended"
expect 1 "a blocker written as a link fails rather than freeing its ticket" "$d" "docs/plan/s/02-second.md: '## Blocked by' names '- [01-first](01-first.md)'"

# Verifies: ADR-0006
d="$(repo name-with-space)"
ticket "$d" "docs/plan/x/01-a b.md" "None (can start immediately)" "unattended"
expect 1 "a ticket whose name the claim refuses fails rather than reaching the frontier" "$d" \
  "docs/plan/x/01-a b.md: a name the claim refuses"

# --- claims: the skill's claim script, in one repository and across two clones -------------

# claim <dir> <ticket> [claim args] — run the claim script in a fixture, or in its subdirectory
# $SUBDIR when set; prints its exit code.
claim() {
  fixture "$1"
  local dir="$1"
  shift
  (cd "$dir/${SUBDIR:-}" && GIT_AUTHOR_NAME=fixture GIT_AUTHOR_EMAIL=fixture@example.invalid \
    GIT_COMMITTER_NAME=fixture GIT_COMMITTER_EMAIL=fixture@example.invalid \
    bash "$CLAIM" "$@" >/dev/null 2>"$work/claim.err")
  echo $?
}

# on_remote <remote> <branch> — whether the remote has the branch.
# shellcheck disable=SC2317 # run by check
on_remote() { git ls-remote --exit-code --heads "$1" "$2" >/dev/null 2>&1; }

# has_branch <dir> <branch> — whether the repository has the local branch.
# shellcheck disable=SC2317 # run by check
has_branch() { git -C "${1:?}" show-ref --verify --quiet "refs/heads/$2"; }

# not <command...> — the command fails.
# shellcheck disable=SC2317 # run by check
not() { ! "$@"; }

# expect_refusal <needle> <name> <dir> <ticket> [claim args] — the claim must exit 1 and say why.
expect_refusal() {
  local needle="$1" name="$2" rc
  shift 2
  rc="$(claim "$@")"
  check "$name (exit $rc: $(cat "$work/claim.err"))" \
    test "$rc" -eq 1 -a -n "$(grep -F -- "$needle" "$work/claim.err")"
}

# expect_claim <exit code> <name> <dir> <ticket> [claim args] — the claim must end with that
# code.
expect_claim() {
  local want="$1" name="$2" rc
  shift 2
  rc="$(claim "$@")"
  if [[ "$rc" -eq "$want" ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want, got exit $rc)" >&2
}

# commit_plan <dir> — commit docs/plan/ in a fixture, as a plan lands on main.
commit_plan() {
  fixture "$1"
  git -C "${1:?}" add docs
  git -C "${1:?}" -c user.name=fixture -c user.email=fixture@example.invalid commit -q -m "plan"
}

d="$(repo local-claim)"
ticket "$d" docs/plan/s/01-first.md "None (can start immediately)" "unattended"
expect_claim 1 "a ticket not yet on main is refused" "$d" docs/plan/s/01-first.md
commit_plan "$d"
mkdir -p "$d/docs/plan/s/sub"
SUBDIR=docs/plan/s/sub expect_claim 0 "a local claim succeeds from a subdirectory" "$d" docs/plan/s/01-first.md
expect_claim 2 "a second local claim is refused" "$d" docs/plan/s/01-first.md
expect 0 "a local claim takes the ticket off the frontier" "$d" "- docs/plan/s/01-first.md — ticket/s/01-first"
# A local claim is the branch and nothing else: it points at main, with no commit of its own.
if [[ "$(git -C "$d" rev-parse ticket/s/01-first)" == "$(git -C "$d" rev-parse main)" ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1)); echo "FAIL: a local claim creates a commit" >&2
fi
expect_claim 1 "a path that is no ticket is refused" "$d" docs/plan/s/spec.md

seed="$(repo seed)"
for n in 01-first 02-plain 03-narrow 04-same; do
  ticket "$seed" "docs/plan/s/$n.md" "None (can start immediately)" "unattended"
done
commit_plan "$seed"
git init -q --bare -b main "$work/remote.git"
git -C "${seed:?}" push -q "$work/remote.git" main main:dev
git clone -q "$work/remote.git" "$work/a" 2>/dev/null
git clone -q "$work/remote.git" "$work/b" 2>/dev/null
expect_claim 0 "a claim on the remote succeeds" "$work/a" docs/plan/s/01-first.md origin
expect_claim 2 "a second clone's claim on the remote is refused" "$work/b" docs/plan/s/01-first.md origin
expect 0 "before a fetch, the second clone still lists the claimed ticket on its frontier" "$work/b" "4 on the frontier, 0 claimed"
git -C "$work/b" fetch -q origin
expect 0 "after a fetch, the second clone sees the claim" "$work/b" "- docs/plan/s/01-first.md — ticket/s/01-first"

git -C "$work/a" push -q --no-verify origin main:refs/heads/ticket/s/02-plain 2>/dev/null
expect_claim 2 "a branch pushed at main without a claim commit still blocks a claim" "$work/b" docs/plan/s/02-plain.md origin

git clone -q --single-branch --branch dev "$work/remote.git" "$work/c" 2>/dev/null
expect_claim 0 "a single-branch clone of another branch claims from the remote's main" "$work/c" docs/plan/s/03-narrow.md origin

# Two claims with the same identity in the same second: only the nonce keeps their commits apart.
export GIT_AUTHOR_DATE="2026-01-01T00:00:00Z" GIT_COMMITTER_DATE="2026-01-01T00:00:00Z"
expect_claim 0 "a claim at a fixed date succeeds" "$work/a" docs/plan/s/04-same.md origin
expect_claim 2 "a second claim at the same date and identity is refused" "$work/b" docs/plan/s/04-same.md origin
unset GIT_AUTHOR_DATE GIT_COMMITTER_DATE

# --- a plan on a feat/ branch: read and claimed from that base (ADR-0006) -------------------

# Verifies: ADR-0006
d="$(repo feat-base)"
git -C "${d:?}" checkout -q -b feat/x
ticket "$d" docs/plan/f/01-first.md "None (can start immediately)" "with the human"
ticket "$d" docs/plan/f/02-second.md "docs/plan/f/01-first.md" "with the human"
commit_plan "$d"
git -C "${d:?}" checkout -q main
expect 0 "the checkout of main has none of the feat/ plan" "$d" "no open tickets under docs/plan/."
expect 0 "the frontier is read on a named feat/ base" "$d" \
  "on feat/x: 2 open, 1 on the frontier, 0 claimed, 1 blocked." --base feat/x
expect 1 "a base that does not exist is refused" "$d" "no branch 'feat/none'" --base feat/none
expect_refusal "claim it with --base feat/<name>" "a ticket only on a feat/ branch is refused \
from main, naming the way to claim it" "$d" docs/plan/f/01-first.md
expect_refusal "the base is main or a feat/ branch" \
  "a base that is neither main nor a feat/ branch is refused" "$d" docs/plan/f/01-first.md --base dev
expect_refusal "one ticket and one remote" "a third argument is refused" "$d" \
  docs/plan/f/01-first.md origin extra --base feat/x
expect_refusal "is no remote" "a remote that reads as an option is refused" "$d" \
  docs/plan/f/01-first.md --upload-pack=x --base feat/x
expect_refusal "cannot reach" "a remote that cannot be reached makes no local claim" "$d" \
  docs/plan/f/01-first.md "$work/no-such-remote.git" --base feat/x
check "the unreached remote left no local claim" not has_branch "$d" ticket/f/01-first
expect_claim 0 "a ticket is claimed from the feat/ branch that carries it" "$d" \
  docs/plan/f/01-first.md --base feat/x
check "a claim from a feat/ base starts at that base" \
  test "$(git -C "$d" rev-parse ticket/f/01-first)" == "$(git -C "$d" rev-parse feat/x)"
expect_claim 2 "a second claim from the feat/ base is refused" "$d" docs/plan/f/01-first.md --base feat/x
expect 0 "a claim from a feat/ base takes its ticket off that frontier" "$d" \
  "- docs/plan/f/01-first.md — ticket/f/01-first" --base feat/x

seed="$(repo feat-seed)"
git -C "${seed:?}" checkout -q -b feat/shared
ticket "$seed" docs/plan/f/01-first.md "None (can start immediately)" "with the human"
commit_plan "$seed"
git -C "${seed:?}" checkout -q -b feat/local
ticket "$seed" docs/plan/f/02-local.md "None (can start immediately)" "with the human"
commit_plan "$seed"
git init -q --bare -b main "$work/feat-remote.git"
git -C "${seed:?}" push -q "$work/feat-remote.git" main feat/shared
git clone -q "$work/feat-remote.git" "$work/fa" 2>/dev/null
git clone -q "$work/feat-remote.git" "$work/fb" 2>/dev/null
git -C "$work/fa" branch -q feat/shared origin/feat/shared
expect_claim 0 "a claim from a feat/ base on the remote is made there" "$work/fa" \
  docs/plan/f/01-first.md origin --base feat/shared
check "a claim from a feat/ base the remote has is pushed" \
  on_remote "$work/feat-remote.git" ticket/f/01-first
expect_claim 2 "a second clone's claim from that feat/ base on the remote is refused" "$work/fb" \
  docs/plan/f/01-first.md origin --base feat/shared
expect_claim 0 "a feat/ base the remote does not have is claimed by the local branch" "$seed" \
  docs/plan/f/02-local.md "$work/feat-remote.git" --base feat/local
check "a claim from a local feat/ base pushes nothing" \
  not on_remote "$work/feat-remote.git" ticket/f/02-local
check "a claim from a local feat/ base is the local branch" has_branch "$seed" ticket/f/02-local

# --- summary ---------------------------------------------------------------------------------

summary "Plan frontier sensor self-test"
