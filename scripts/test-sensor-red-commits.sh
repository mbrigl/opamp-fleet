#!/usr/bin/env bash
#
# Self-test of scripts/sensor-red-commits.sh.
#
# Each case builds a fixture repository whose main holds merges of pull requests, each commit
# with a stub scripts/check-all.sh, and runs the sensor on it. A gh first on PATH leaves a mark
# when it is called, so that each run shows the sensor calls none. The cases assert the commits
# it finds red alone, the commits it runs, those whose checks decided nothing and those without
# checks, the cap of ten merges read, the cleanup of its worktrees when a run is stopped, and
# the exit codes.
#
# Verifies: NUC-checks-are-bash
# Inputs: scripts/sensor-red-commits.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-red-commits.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-red-commits.sh"

# A gh that marks each call: the sensor calls no gh.
mkdir -p "$work/bin"
GH_CALLED="$work/gh-called"
printf '#!/usr/bin/env bash\ntouch %q\nexit 1\n' "$GH_CALLED" > "$work/bin/gh"
chmod +x "$work/bin/gh"
export PATH="$work/bin:$PATH"

# at <date> <dir> <git args...> — git in the fixture with an identity, its commit made at the date.
at() {
  local when="$1" dir="$2"; shift 2
  GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" \
    git -C "${dir:?}" -c user.name=t -c user.email=t@example.invalid "$@"
}

# repo <name> — a fixture with one commit on main, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "$dir" init -q -b main
  echo base > "$dir/base.txt"
  at "2026-11-01T08:00:00Z" "$dir" add -A
  at "2026-11-01T08:00:00Z" "$dir" commit -q -m base
  echo "$dir"
}

# merged <dir> <n> <branch> <first commit date> <merge date> — a branch of two commits, the first
# at its date, merged into main as GitHub merges pull request <n>.
merged() {
  local dir="$1" n="$2" branch="$3" first="$4" merge="$5"
  git -C "$dir" switch -q -c "$branch" main
  echo "$n a" > "$dir/$n.txt"
  at "$first" "$dir" add -A
  at "$first" "$dir" commit -q -m "work $n"
  echo "$n b" >> "$dir/$n.txt"
  at "$merge" "$dir" commit -q -am "fix $n"
  git -C "$dir" switch -q main
  at "$merge" "$dir" merge -q --no-ff -m "Merge pull request #$n from owner/$branch" "$branch"
}

# expect <name> <dir> <rc> <needle> [args...] — run the sensor; it must exit <rc>, its output
# hold the needle as a whole line, and gh not be called.
expect() {
  local name="$1" dir="$2" want="$3" needle="$4" out rc
  shift 4
  rm -f "$GH_CALLED"
  out="$(bash "$SENSOR" --root "$dir" "$@" 2>&1)"; rc=$?
  if [[ "$rc" == "$want" && ! -e "$GH_CALLED" ]] && grep -qxF -- "$needle" <<< "$out"; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: $name (expected exit $want, the line '$needle', and no call of gh; got exit $rc)" >&2
    while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
  fi
}

# Each commit of a merged branch runs its own scripts/check-all.sh: the one on which it failed is
# named, the later one that made it pass is not. The stub counts its runs in the file RUNS names.
d4="$(repo red)"
mkdir -p "$d4/scripts"
# shellcheck disable=SC2016 # the stub's line is its own
printf '#!/usr/bin/env bash\necho run >> "$RUNS"\n! grep -qs red state.txt\n' > "$d4/scripts/check-all.sh"
at "2026-11-01T09:00:00Z" "$d4" add -A
at "2026-11-01T09:00:00Z" "$d4" commit -q -m "the checks"
git -C "$d4" switch -q -c ticket/red main
echo red > "$d4/state.txt"
at "2026-11-02T08:00:00Z" "$d4" add -A
at "2026-11-02T08:00:00Z" "$d4" commit -q -m "work red alone"
echo green > "$d4/state.txt"
at "2026-11-02T10:00:00Z" "$d4" commit -q -am "work made green"
git -C "$d4" switch -q main
at "2026-11-02T11:00:00Z" "$d4" merge -q --no-ff \
  -m "Merge pull request #9 from owner/ticket/red" ticket/red
export RUNS="$work/runs"
expect "the commit that was red alone is named, without gh" "$d4" 0 \
  "    - red alone: $(git -C "$d4" log -1 --format=%h ticket/red~1) work red alone"
check "the checks of each commit of the branch run" test "$(wc -l < "$RUNS")" = 2
expect "the merge is named by its pull request and branch" "$d4" 0 "  - #9 ticket/red"
out="$(bash "$SENSOR" --root "$d4" 2>/dev/null)"
check "no commit that passed is named" test -z "$(grep -F 'work made green' <<< "$out")"
check "no worktree is left behind" \
  test "$(git -C "$d4" worktree list | wc -l)" = 1

# A commit whose checks exit with neither 0 nor 1 decided nothing; one without check-all.sh is
# not run.
git -C "$d4" switch -q -c fix/odd main
printf '#!/usr/bin/env bash\nexit 2\n' > "$d4/scripts/check-all.sh"
at "2026-11-03T08:00:00Z" "$d4" commit -q -am "checks that refuse"
git -C "$d4" switch -q main
at "2026-11-03T09:00:00Z" "$d4" merge -q --no-ff \
  -m "Merge pull request #10 from owner/fix/odd" fix/odd
expect "a commit whose checks exited 2 is not decided" "$d4" 0 \
  "    - not decided, its checks exited 2: $(git -C "$d4" log -1 --format=%h fix/odd) checks that refuse"
git -C "$d4" switch -q -c fix/none "$(git -C "$d4" rev-list --max-parents=0 main)"
echo none > "$d4/none.txt"
at "2026-11-04T08:00:00Z" "$d4" add -A
at "2026-11-04T08:00:00Z" "$d4" commit -q -m "no checks"
git -C "$d4" switch -q main
at "2026-11-04T09:00:00Z" "$d4" merge -q --no-ff \
  -m "Merge pull request #11 from owner/fix/none" fix/none
expect "a commit without check-all.sh is counted as not run" "$d4" 0 \
  "    - not run, without scripts/check-all.sh or a worktree: 1 commit"

# Only the ten newest merges are read; the eleventh, the oldest, is counted and not run.
d3="$(repo many)"
for n in 1 2 3 4 5 6 7 8 9 10 11; do
  day="$(printf '2026-11-%02d' "$n")"
  merged "$d3" "$n" "fix/m$n" "${day}T08:00:00Z" "${day}T09:00:00Z"
done
expect "eleven merges: the newest ten are read" "$d3" 0 \
  "Red commits sensor: 11 merges of a pull request on main, the newest 10 read."
out="$(bash "$SENSOR" --root "$d3" 2>&1)"
check "the oldest of eleven merges is not listed" \
  test "$(grep -c '^  - #' <<< "$out")" = 10 -a -z "$(grep -Fx '  - #1 fix/m1' <<< "$out")"

# A run stopped while a commit's checks run stops them and leaves no worktree behind, not even
# an entry git would call prunable.
d5="$(repo stopped)"
mkdir -p "$d5/scripts"
# shellcheck disable=SC2016 # the stub's line is its own
printf '#!/usr/bin/env bash\ntouch "$STARTED"\nexec sleep 30\n' > "$d5/scripts/check-all.sh"
merged "$d5" 12 fix/slow "2026-11-05T08:00:00Z" "2026-11-05T09:00:00Z"
export STARTED="$work/started"
bash "$SENSOR" --root "$d5" > /dev/null 2>&1 &
pid=$!
for _ in $(seq 100); do [[ -e "$STARTED" ]] && break; sleep 0.1; done
SECONDS=0
kill -TERM "$pid"
wait "$pid"; rc=$?
check "a run stopped by TERM exits 143 at once" test "$rc" = 143 -a "$SECONDS" -lt 10
check "a stopped run leaves no worktree behind" \
  test "$(git -C "$d5" worktree list --porcelain | grep -c '^worktree ')" = 1

# Nothing merged, no repository, and an unknown argument.
d2="$(repo empty)"
expect "no merge of a pull request says so" "$d2" 0 \
  "Red commits sensor: no merge of a pull request on main — nothing to report."
expect "a root that does not exist exits 2" "$work/none" 2 \
  "sensor-red-commits.sh: '$work/none' is not a directory — usage: scripts/sensor-red-commits.sh [--root <dir>]"
mkdir -p "$work/plain"
expect "a root that is no git repository exits 2" "$work/plain" 2 \
  "red commits sensor: '$work/plain' is no git repository."
expect "an unknown argument exits 2" "$d2" 2 \
  "sensor-red-commits.sh: unknown argument '--red' — usage: scripts/sensor-red-commits.sh [--root <dir>]" \
  --red

summary "Red commits sensor self-test"
