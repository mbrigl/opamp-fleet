#!/usr/bin/env bash
#
# Self-test of scripts/sensor-work-measures.sh.
#
# Each case builds a fixture repository whose main holds merges of pull requests, made at fixed
# dates, and runs the sensor on it. The sensor reads the descriptions of the pull requests through
# gh; here a stand-in named by GH answers instead, with the description kept in a fixture file per
# pull request. The cases assert the time from a branch's first commit to its merge, the rounds of
# review from the Measures line, the cap of ten merges read, the option --red it no longer reads,
# and the exit codes.
#
# Verifies: NUC-checks-are-bash
# Inputs: scripts/sensor-work-measures.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-work-measures.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-work-measures.sh"

# The stand-in for gh: 'pr view <n> …' prints the fixture file of that pull request, and fails
# where there is none, as gh does on an error.
fake="$work/gh"
cat > "$fake" <<'EOF'
#!/usr/bin/env bash
[[ "$1 $2" == "pr view" && -f "$FIX/pr-$3" ]] || exit 1
cat "$FIX/pr-$3"
EOF
chmod +x "$fake"

# at <date> <dir> <git args...> — git in the fixture with an identity, its commit made at the date.
at() {
  local when="$1" dir="$2"; shift 2
  GIT_AUTHOR_DATE="$when" GIT_COMMITTER_DATE="$when" \
    git -C "${dir:?}" -c user.name=t -c user.email=t@example.invalid "$@"
}

# repo <name> — a fixture with one commit on main, printed as its path; its gh answers under fix/.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/fix"
  git -C "$dir" init -q -b main
  echo fix/ > "$dir/.gitignore"
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

# expect <name> <dir> <rc> <needle> [args...] — run the sensor; it must exit <rc> and its output
# hold the needle as a whole line.
expect() {
  local name="$1" dir="$2" want="$3" needle="$4" out rc
  shift 4
  out="$(GH="${GH_CMD:-$fake}" FIX="$dir/fix" bash "$SENSOR" --root "$dir" "$@" 2>&1)"; rc=$?
  if [[ "$rc" == "$want" ]] && grep -qxF -- "$needle" <<< "$out"; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: $name (expected exit $want and the line '$needle', got exit $rc)" >&2
    while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
  fi
}

# A claimed ticket, reviewed twice, merged: 26 hours from its first commit to its merge, and the
# two rounds of its Measures line.
d="$(repo ticket)"
merged "$d" 5 ticket/spec/01-one "2026-11-02T08:00:00Z" "2026-11-03T10:00:00Z"
printf '## Proof\n\n- Measures: tokens 1200 | checks 24 s | review rounds 2\n\n## Review\n\n- Reviewed at: abc\n' \
  > "$d/fix/pr-5"
expect "a ticket reviewed twice: its time and its rounds" "$d" 0 \
  "  - #5 ticket/spec/01-one: 26 h from its first commit to its merge, 2 rounds of review"
expect "the headline counts the merges read" "$d" 0 \
  "Work measures sensor: 1 merge of a pull request on main, the newest 1 read."

# The rounds come from the Measures line alone: Reviewed at: lines, one that moved after a second
# round or one left empty, count none, and a '-' in the line is unknown too.
merged "$d" 6 fix/two "2026-11-04T08:00:00Z" "2026-11-04T08:30:00Z"
printf '## Review\n\n- Reviewed at: aaa\n- Reviewed at:\n' > "$d/fix/pr-6"
merged "$d" 7 docs/three "2026-11-05T08:00:00Z" "2026-11-07T09:00:00Z"
printf -- '- Measures: tokens - | checks - | review rounds -\n' > "$d/fix/pr-7"
expect "Reviewed at: lines without a Measures line leave the rounds unknown" "$d" 0 \
  "  - #6 fix/two: 0 h from its first commit to its merge, rounds of review unknown"
expect "a Measures line whose rounds are - leaves them unknown" "$d" 0 \
  "  - #7 docs/three: 49 h from its first commit to its merge, rounds of review unknown"

# A pull request gh cannot read keeps its time, and its rounds are unread.
merged "$d" 8 fix/four "2026-11-08T08:00:00Z" "2026-11-08T12:00:00Z"
expect "a pull request gh cannot read keeps its time" "$d" 0 \
  "  - #8 fix/four: 4 h from its first commit to its merge, rounds of review not read"

# Only the ten newest merges are read; the eleventh, the oldest, is counted and not listed.
d3="$(repo many)"
for n in 1 2 3 4 5 6 7 8 9 10 11; do
  day="$(printf '2026-11-%02d' "$n")"
  merged "$d3" "$n" "fix/m$n" "${day}T08:00:00Z" "${day}T09:00:00Z"
done
expect "eleven merges: the newest ten are read" "$d3" 0 \
  "Work measures sensor: 11 merges of a pull request on main, the newest 10 read."
out="$(GH="$fake" FIX="$d3/fix" bash "$SENSOR" --root "$d3" 2>&1)"
check "the oldest of eleven merges is not listed" \
  test "$(grep -c '^  - #' <<< "$out")" = 10 -a -z "$(grep -F '#1 fix/m1:' <<< "$out")"

# Nothing merged, no repository, no gh, and an unknown argument.
d2="$(repo empty)"
expect "no merge of a pull request says so" "$d2" 0 \
  "Work measures sensor: no merge of a pull request on main — nothing to report."
expect "a root that does not exist exits 2" "$work/none" 2 \
  "sensor-work-measures.sh: '$work/none' is not a directory — usage: scripts/sensor-work-measures.sh [--root <dir>]"
mkdir -p "$work/plain"
expect "a root that is no git repository exits 2" "$work/plain" 2 \
  "work measures sensor: '$work/plain' is no git repository."
out="$(bash "$SENSOR" --root 2>&1)"; rc=$?
check "--root without a directory exits 2 and reads no repository" \
  test "$rc" = 2 -a "$out" = "sensor-work-measures.sh: '--root' needs a directory — usage: scripts/sensor-work-measures.sh [--root <dir>]"
GH_CMD="$work/no-such-gh" expect "no gh exits 2" "$d" 2 \
  "work measures sensor: the GitHub CLI ($work/no-such-gh) is not installed."
expect "an unknown argument exits 2" "$d" 2 \
  "sensor-work-measures.sh: unknown argument '--verbose' — usage: scripts/sensor-work-measures.sh [--root <dir>]" \
  --verbose

# --red left for scripts/sensor-red-commits.sh.
expect "--red is an unknown argument" "$d" 2 \
  "sensor-work-measures.sh: unknown argument '--red' — usage: scripts/sensor-work-measures.sh [--root <dir>]" \
  --red

summary "Work measures sensor self-test"
