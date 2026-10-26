#!/usr/bin/env bash
#
# Self-test of the git hooks under .githooks/: every refusal and every passage the table under
# "Git hooks" in the README lists (AGENTS.md, sections 5 and 6).
#
# Each case builds a throwaway repository under a temporary directory with core.hooksPath pointing
# at this repository's .githooks/, runs a real 'git commit' or 'git push' in it — the push goes to
# a bare repository next to it — and asserts the exit code and, for a refused command, the message.
# A push case ships its own scripts/check-all.sh, green or red, so the test decides what the hook
# finds.
#
# Verifies: ADR-0002
# Inputs: .githooks/ scripts/lib/
#
# Usage:
#     scripts/test-git-hooks.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
HOOKS="$ROOT/.githooks"

# g <dir> <git args...> — git in the fixture with an identity, so that commits need no config.
g() {
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=t -c user.email=t@example.invalid "$@"
}

# repo <name> — a fixture repository with one commit on main, the hooks and the Dev Container's
# merge settings enabled after that commit, and a bare remote 'origin' to push to. Prints its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  g "$dir" commit -q --allow-empty -m init
  git -C "${dir:?}" config core.hooksPath "$HOOKS"
  git -C "${dir:?}" config merge.ff false
  git -C "${dir:?}" config pull.ff only
  git init -q --bare "$dir.git"
  git -C "${dir:?}" remote add origin "$dir.git"
  echo "$dir"
}

# checks <dir> <exit code> — give the fixture a scripts/check-all.sh that exits as told.
checks() {
  mkdir -p "$1/scripts"
  printf '#!/usr/bin/env bash\necho "check output for the test, args: $*"\nexit %s\n' "$2" > "$1/scripts/check-all.sh"
}

# docs <dir> <exit code> — give the fixture a scripts/check-docs.sh that exits as told.
docs() {
  mkdir -p "$1/scripts"
  printf '#!/usr/bin/env bash\necho "docs output for the test"\nexit %s\n' "$2" > "$1/scripts/check-docs.sh"
}

# expect <pass|refuse> <name> <dir> <needle> <git args...> — run the git command; a refused one
# must exit non-zero with the needle in its output, a passing one exit zero.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4"
  shift 4
  local out rc
  out="$(g "$dir" "$@" 2>&1)"; rc=$?
  if [[ "$want" == "pass" && $rc -eq 0 ]] \
     || [[ "$want" == "refuse" && $rc -ne 0 && "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected $want${needle:+ with \"$needle\"}, exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases ---------------------------------------------------------------------------------

d="$(repo on-main)"
expect refuse "a commit on main" "$d" "on 'main'" commit -q --allow-empty -m x
expect refuse "a push to the remote's main" "$d" "through a pull request, not a push" push -q origin main
expect pass "a push of main to a branch of its own with no check script to run" "$d" "" push -q origin main:other

d="$(repo on-branch)"
git -C "${d:?}" switch -q -c feature
expect pass "a commit on a branch" "$d" "" commit -q --allow-empty -m x

d="$(repo red)"
git -C "${d:?}" switch -q -c feature
checks "$d" 1
expect refuse "a push while the checks are red" "$d" "the checks are red" push -q origin feature
expect refuse "the refusal shows the check output" "$d" "check output for the test" push -q origin feature
expect refuse "the hook runs the checks with --changed" "$d" "args: --changed" push -q origin feature
expect pass "a commit is not gated by the checks" "$d" "" commit -q --allow-empty -m x

# A deletion sends nothing to check: it passes while the checks are red, and a push that sends a
# branch besides is checked as any other.
d="$(repo delete-red)"
git -C "${d:?}" switch -q -c feature
g "$d" push -q --no-verify origin main feature feature:other
checks "$d" 1
expect pass "a deletion of a branch while the checks are red" "$d" "" push -q origin --delete feature
expect refuse "a push of a branch while the checks are red, after a deletion passed" "$d" "the checks are red" push -q origin feature
expect refuse "a deletion together with a push of a branch is checked" "$d" "the checks are red" push -q origin :other feature
expect refuse "a deletion of the remote's main while the checks are red" "$d" "through a pull request, not a push" push -q origin --delete main

d="$(repo green)"
git -C "${d:?}" switch -q -c feature
checks "$d" 0
expect pass "a push while the checks are green" "$d" "" push -q origin feature
out="$(g "$d" push -q origin feature 2>&1)"
check "a push without a draft ADR says nothing of numbering" test -z "$(grep 'draft ADR' <<< "$out")"

# A push that carries a draft ADR is told how many it carries and where they take their numbers;
# the notice passes the push, and a file named like a draft that is no Markdown is no draft.
mkdir -p "$d/docs/adr" && : > "$d/docs/adr/draft-x.md" && : > "$d/docs/adr/0001-y.md"
: > "$d/docs/adr/draft-notes.txt"
g "$d" add docs && g "$d" commit -q -m draft
out="$(g "$d" push -q origin feature 2>&1)"; rc=$?
check "a push that carries a draft ADR passes" test "$rc" -eq 0
check "and counts the drafts and names the script that numbers them" \
  grep -qF "feature carries 1 draft ADR(s); they take their numbers on the branch of the pull request into main, by scripts/ready-for-main.sh (README, Integration branches, step 7)." <<< "$out"

# A tree the checks passed on passes again without them: the committed check script reads its
# verdict from outside the tree, so the same tree can turn red. A rework that keeps the tree, an
# amend here, passes on the record; local changes, an untracked file, or a new tree run them.
d="$(repo green-tree)"
git -C "${d:?}" switch -q -c feature
mkdir -p "$d/scripts"
# shellcheck disable=SC2016  # the command substitution is the fixture script's, not this one's
printf '#!/usr/bin/env bash\necho "check output for the test"\nexit "$(cat "%s")"\n' "$work/verdict" \
  > "$d/scripts/check-all.sh"
g "$d" add scripts && g "$d" commit -q -m checks
echo 0 > "$work/verdict"
expect pass "a push of a clean tree while the checks are green" "$d" "" push -q origin feature
echo 1 > "$work/verdict"
g "$d" commit -q --amend -m regrouped
expect pass "a reworked branch on a tree that passed before, while the checks are red" "$d" "" \
  push -q --force origin feature
touch "$d/untracked"
expect refuse "the same tree with an untracked file is checked again" "$d" "the checks are red" \
  push -q --force origin feature
rm "$d/untracked"
echo x > "$d/new" && g "$d" add new && g "$d" commit -q -m new
expect refuse "a new tree is checked again" "$d" "the checks are red" push -q origin feature

# In a linked worktree git hands the hook GIT_DIR, and 'git -c' adds GIT_CONFIG_PARAMETERS. The
# checks build fixture repositories of their own; with either inherited, those land in the
# repository being pushed and carry its credentials. This check script does what a self-test
# does and fails on any variable that binds git to a repository.
d="$(repo worktree)"
git -C "${d:?}" worktree add -q "$work/worktree-linked" -b feature
mkdir -p "$work/worktree-linked/scripts"
cat > "$work/worktree-linked/scripts/check-all.sh" <<EOF
#!/usr/bin/env bash
for v in \$(git rev-parse --local-env-vars); do
  [[ -v \$v ]] && { echo "leaked \$v"; exit 1; }
done
git init -q "$work/worktree-fixture" && git -C "$work/worktree-fixture" config hooktest.leak yes
EOF
expect pass "a push from a linked worktree hands the checks no git environment" \
  "$work/worktree-linked" "" push -q origin feature
if [[ -n "$(git -C "$d" config hooktest.leak)" ]]; then
  failed=$((failed + 1))
  echo "FAIL: a fixture the checks build in a linked worktree wrote into the pushed repository" >&2
fi

# The shared library of the self-tests guards the same: a self-test that sources it inherits no
# variable that binds git to a repository, and its fixture guard refuses any path but a fixture
# repository under the test's own directory, this repository's root included.
lib="$ROOT/scripts/lib/selftest.sh"
# shellcheck disable=SC2016 # expanded by the inner shell
leaked="$(GIT_DIR="$ROOT/.git" GIT_WORK_TREE="$ROOT" bash -c 'source "$1"; echo "${GIT_DIR-}${GIT_WORK_TREE-}"' _ "$lib")"
if [[ -z "$leaked" ]]; then passed=$((passed + 1)); else
  failed=$((failed + 1)); echo "FAIL: a self-test sourcing the library keeps GIT_DIR or GIT_WORK_TREE" >&2; fi
# '$outside' is shaped like a fixture but lies outside $work: it decides the clause on $work
# where '$ROOT' cannot, in a linked worktree, whose .git is a file.
outside="$(mktemp -d)"
# The library's trap removes $work only; this one removes $outside too, whatever ends the test.
trap 'rm -rf "$work" "$outside"' EXIT
mkdir -p "$outside/.git"
# shellcheck disable=SC2016 # expanded by the inner shell
for target in '' '$ROOT' '$outside' '$work/plain' '$work/fixture'; do
  out="$(outside="$outside" bash -c 'source "$1"; mkdir -p "$work/plain" "$work/fixture/.git"; fixture "'"$target"'" && echo accepted' _ "$lib" 2>&1)"
  want="refused"; [[ "$target" == '$work/fixture' ]] && want="accepted"
  if [[ ("$want" == "accepted" && "$out" == "accepted") || ("$want" == "refused" && "$out" == *"no fixture repository"*) ]]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1)); echo "FAIL: the fixture guard on '$target' (expected $want): $out" >&2
  fi
done

# A clone without the merge settings still commits and pushes; the push gates what a
# fast-forward brought past pre-merge-commit.
d="$(repo unconfigured)"
git -C "${d:?}" switch -q -c feature
git -C "${d:?}" config --unset merge.ff
git -C "${d:?}" config --unset pull.ff
expect pass "a commit in a clone without the merge settings" "$d" "" commit -q --allow-empty -m x
expect pass "a push from a clone without the merge settings" "$d" "" push -q origin feature

d="$(repo detached)"
git -C "${d:?}" checkout -q --detach
expect pass "a commit on a detached HEAD is not on main" "$d" "" commit -q --allow-empty -m x

# merging <name> <docs exit code> <checks exit code> — a fixture with documentation check and
# check output as told, a branch 'feature' one commit ahead of main that a fast-forward could
# take, and the integration branch 'feat/x' at main checked out.
merging() {
  local d
  d="$(repo "$1")"
  docs "$d" "$2"
  checks "$d" "$3"
  git -C "${d:?}" switch -q -c feature
  echo feature > "$d/f"
  g "$d" add f
  g "$d" commit -q -m feature
  git -C "${d:?}" switch -q -c feat/x main
  echo "$d"
}

d="$(merging merge-red 1 0)"
expect refuse "a merge into a branch while the documentation check is red" "$d" "the documentation check is red on the merge" merge -q -m m feature

d="$(merging merge-red-output 1 0)"
expect refuse "the refusal shows the documentation check's output" "$d" "docs output for the test" merge -q -m m feature

d="$(merging merge-suite 0 1)"
expect pass "a clean merge is not gated by the rest of the checks" "$d" "" merge -q -m m feature

d="$(merging merge-ff 1 0)"
expect refuse "merge.ff false keeps a fast-forward from passing the hook" "$d" "pre-merge-commit: refused" merge -q feature

d="$(merging merge-green 0 0)"
expect pass "a merge into a branch while the checks are green" "$d" "" merge -q -m m feature

d="$(merging merge-main 0 0)"
git -C "${d:?}" switch -q main
expect refuse "a merge on main, though the checks are green" "$d" "on 'main'" merge -q -m m feature

# conflict <name> <checks exit code> <branch> — a merge into the branch stopped on a conflict,
# resolved and staged, with a green documentation check.
conflict() {
  local d
  d="$(merging "$1" 0 "$2")"
  git -C "${d:?}" switch -q "$3"
  echo other > "$d/f"
  g "$d" add f
  g "$d" commit -q --no-verify -m other
  g "$d" merge -q -m m feature >/dev/null 2>&1
  echo resolved > "$d/f"
  g "$d" add f
  echo "$d"
}

d="$(conflict conflict-red 1 feat/x)"
expect refuse "concluding a conflicted merge while the checks are red" "$d" "pre-commit: refused — the checks are red" commit -q --no-edit

d="$(conflict conflict-changed 1 feat/x)"
expect refuse "concluding a conflicted merge runs the checks with --changed" "$d" "args: --changed" commit -q --no-edit

d="$(conflict conflict-green 0 feat/x)"
expect pass "concluding a conflicted merge while the checks are green" "$d" "" commit -q --no-edit

d="$(conflict conflict-main 0 main)"
expect refuse "concluding a conflicted merge on main, though the checks are green" "$d" "on 'main'" commit -q --no-edit

# edit_index <index> <rule> <row> <after> — set the rule above the table, append the row to it,
# and set the line right below it.
edit_index() {
  awk -v rule="$2" -v row="$3" -v after="$4" '
    /^Rule:/ { print "Rule: " rule "."; next }
    /^After:/ { print row; print "After: " after "."; next }
    { print }
  ' "$1" > "$1.tmp" && mv "$1.tmp" "$1"
}

# indexed <name> <rule on feature> <rule on feat/x> <line below the table on feat/x> [driver] — a
# fixture whose ADR index holds a rule, a row, and a line right below the table, with the
# repository's .gitattributes and green checks; 'feature' and 'feat/x' each append a row and set
# the rule as told, and feat/x the line below. The merge driver is configured as the README says,
# unless the fifth argument is 'none'.
indexed() {
  local d index
  d="$(merging "$1" 0 0)"
  index="$d/docs/adr/README.md"
  [[ "${5:-}" == none ]] \
    || git -C "${d:?}" config merge.adr-index.driver "$HOOKS/merge-adr-index %O %A %B %S %X %Y"
  cp "$ROOT/.gitattributes" "$d/.gitattributes"
  mkdir -p "$d/docs/adr"
  git -C "${d:?}" switch -q main
  printf 'Rule: one.\n\n| ADR |\n|-----|\n| [0001](0001-a.md) |\nAfter: one.\n' > "$index"
  g "$d" add -A
  g "$d" commit -q --no-verify -m index
  git -C "${d:?}" switch -q feature
  g "$d" merge -q --no-verify -m main main
  edit_index "$index" "$2" "| [0002](0002-b.md) |" one
  g "$d" commit -q --no-verify -am row
  git -C "${d:?}" switch -q -C feat/x main
  edit_index "$index" "$3" "| [draft-c](draft-c.md) |" "$4"
  g "$d" commit -q --no-verify -am row
  echo "$d"
}

# rows <dir> — the rule, the rows, and the line below the table after the merge, on one line.
rows() {
  grep -E '^(\| \[|Rule:|After:)' "$1/docs/adr/README.md" | tr '\n' ' '
}

d="$(indexed index-rows one one one)"
expect pass "the driver keeps the rows both sides appended to the index" "$d" "" merge -q -m m feature
check "both rows stand in the index after the merge" \
  test "$(rows "$d")" = "Rule: one. | [0001](0001-a.md) | | [draft-c](draft-c.md) | | [0002](0002-b.md) | After: one. "

d="$(indexed index-prose-one two one one)"
expect pass "the driver takes a rule one side changed" "$d" "" merge -q -m m feature
check "the rule one side changed stands once" \
  test "$(rows "$d")" = "Rule: two. | [0001](0001-a.md) | | [draft-c](draft-c.md) | | [0002](0002-b.md) | After: one. "

d="$(indexed index-prose-both two three one)"
expect refuse "the driver stops on a rule both sides changed" "$d" "CONFLICT (content): Merge conflict in docs/adr/README.md" merge -q -m m feature
check "the conflict is marked in the index" grep -q '^<<<<<<< HEAD' "$d/docs/adr/README.md"

d="$(indexed index-prose-adjacent one one two)"
expect refuse "the driver stops on a line changed right below a row the other side appended" "$d" "CONFLICT (content): Merge conflict in docs/adr/README.md" merge -q -m m feature

d="$(indexed index-no-driver one one one none)"
expect refuse "without the driver configured the index merges as text" "$d" "CONFLICT (content): Merge conflict in docs/adr/README.md" merge -q -m m feature

# moves <name> — a fixture whose remote has main, fetched as origin/main, and a branch 'ahead'
# one commit past main that the remote does not have, with main checked out.
moves() {
  local d
  d="$(repo "$1")"
  g "$d" push -q --no-verify origin main
  g "$d" fetch -q origin
  git -C "${d:?}" switch -q -c ahead
  echo ahead > "$d/a"
  g "$d" add a
  g "$d" commit -q -m ahead
  git -C "${d:?}" switch -q main
  echo "$d"
}

d="$(moves moves)"
expect refuse "a fast-forward of main to a local branch" "$d" "main moves only to what origin/main has" merge -q --ff-only ahead
g "$d" restore --source=HEAD --staged --worktree .
expect pass "a refused fast-forward leaves main where it was" "$d" "" diff --quiet origin/main main
expect pass "the documented restore takes the index and the working tree back" "$d" "" diff --quiet HEAD
if [[ -e "$d/a" ]]; then
  failed=$((failed + 1))
  echo "FAIL: the documented restore leaves the file the refused fast-forward brought" >&2
fi
expect refuse "a commit on main that skips pre-commit" "$d" "main moves only to what origin/main has" commit -q --no-verify --allow-empty -m x
expect refuse "a cherry-pick onto main" "$d" "main moves only to what origin/main has" cherry-pick ahead
g "$d" cherry-pick --abort >/dev/null 2>&1
expect refuse "a reset of main to a local branch" "$d" "main moves only to what origin/main has" reset -q --hard ahead
expect refuse "an update of main by its ref" "$d" "main moves only to what origin/main has" update-ref refs/heads/main ahead
git -C "${d:?}" switch -q ahead
expect pass "a commit on another branch is no move of main" "$d" "" commit -q --allow-empty -m on-ahead
expect refuse "git branch -f of main from another branch" "$d" "main moves only to what origin/main has" branch -f main ahead
# Pushed by path, the remote's main moves while origin/main stays behind, as when another clone
# moved it; the fetch then moves origin/main and main in one transaction.
g "$d" push -q --no-verify "$d.git" ahead:main
expect pass "a fetch that moves main with origin/main" "$d" "" fetch -q origin main:main
git -C "${d:?}" switch -q main
expect pass "main takes up the remote's main" "$d" "" merge -q --ff-only origin/main
expect pass "main is reset to the remote's main" "$d" "" reset -q --hard origin/main
expect refuse "a deletion of the remote's main" "$d" "through a pull request, not a push" push -q origin --delete main

# A clone whose main is ahead of the remote's, as one that took up these hooks with unpushed work.
d="$(moves ahead-of-remote)"
g "$d" -c core.hooksPath=/dev/null commit -q --allow-empty -m unpushed
expect pass "pack-refs writes main where it already is" "$d" "" pack-refs --all
expect pass "a reset of main to where it is moves nothing" "$d" "" reset -q --hard HEAD
expect refuse "main ahead still moves no further" "$d" "main moves only to what origin/main has" commit -q --no-verify --allow-empty -m more
expect refuse "the refusal says how to keep the commits of a main ahead" "$d" "git switch -c feat/<topic> main, then git branch -f main origin/main" commit -q --no-verify --allow-empty -m more
expect pass "the commits of a main ahead move to a branch" "$d" "" switch -q -c feat/kept main
expect pass "main is taken back to the remote's main" "$d" "" branch -f main origin/main
if [[ "$(git -C "$d" log -1 --format=%s feat/kept)" != "unpushed" ]]; then
  failed=$((failed + 1))
  echo "FAIL: the branch does not keep the commit the main ahead had" >&2
fi
d="$(moves delete-main)"
git -C "${d:?}" switch -q ahead
expect pass "a deletion of the local main" "$d" "" branch -q -D main

d="$(moves behind-remote-hint)"
expect refuse "a main not ahead is told to take up the remote's main" "$d" "Take up the remote's main" merge -q --ff-only ahead

d="$(repo no-remote-main)"
expect pass "without origin/main nothing is decided" "$d" "" commit -q --no-verify --allow-empty -m x

# --- summary -------------------------------------------------------------------------------

summary "git hooks self-test"
