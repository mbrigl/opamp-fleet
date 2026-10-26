#!/usr/bin/env bash
#
# Self-test of scripts/check-all.sh, its --changed mode in particular.
#
# Each case runs a copy of this repository's check-all.sh in a throwaway git repository whose
# scripts are stubs: every script the copy runs is replaced by one that prints its own name, so
# the test sees which ran. The self-test stubs declare their inputs in an '# Inputs:' line — a
# file, a directory, a path that does not exist, or no line at all — and the cases change one
# path on a branch and assert which self-tests ran. The checks themselves run in every case.
#
# Inputs: scripts/check-all.sh scripts/lib/
#
# Usage:
#     scripts/test-check-all.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
RUNNER="$ROOT/scripts/check-all.sh"

# g <dir> <git args...> — git in the fixture with an identity, so that commits need no config.
g() {
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=t -c user.email=t@example.invalid "$@"
}

# The scripts the runner runs, in its order, read with the pattern check 9 of check-docs.sh uses.
scripts=()
while IFS= read -r line; do
  [[ "$line" =~ ^[[:space:]]*run[[:space:]]+\"[^\"]*\"[[:space:]]+(scripts/[A-Za-z0-9._-]+\.sh) ]] \
    && scripts+=("${BASH_REMATCH[1]}")
done < "$RUNNER"
tests=()
checks=()
for s in "${scripts[@]}"; do
  if [[ "$s" == scripts/test-*.sh ]]; then tests+=("$s"); else checks+=("$s"); fi
done
if ((${#tests[@]} < 4 || ${#checks[@]} < 1)); then
  echo "check-all self-test: the runner lists ${#tests[@]} self-tests and ${#checks[@]} checks; the cases need four and one" >&2
  exit 1
fi
# The four self-tests the cases are about; every other one names a file of its own under src/.
by_file="${tests[0]}" no_line="${tests[1]}" by_dir="${tests[2]}" missing="${tests[3]}"
others="${tests[*]:4}"

# stub <dir> <script> <inputs or empty> — a script that prints its name, with an Inputs line.
stub() {
  {
    printf '#!/usr/bin/env bash\n'
    [[ -n "$3" ]] && printf '# Inputs: %s\n' "$3"
    printf 'echo "RAN %s"\n' "$2"
  } > "$1/$2"
}

# repo <name> — the runner and its stubs committed on main, a branch 'feature' checked out.
# Prints its path.
repo() {
  local dir="$work/$1" s
  mkdir -p "$dir/scripts" "$dir/src" "$dir/lib"
  cp "$RUNNER" "$dir/scripts/check-all.sh"
  mkdir -p "$dir/scripts/lib" && cp "$ROOT/scripts/lib/git.sh" "$dir/scripts/lib/git.sh"
  for s in "${checks[@]}"; do stub "$dir" "$s" ""; done
  for s in "${tests[@]}"; do
    case "$s" in
      "$by_file") stub "$dir" "$s" "src/by-file.sh" ;;
      "$no_line") stub "$dir" "$s" "" ;;
      "$by_dir") stub "$dir" "$s" "lib/" ;;
      "$missing") stub "$dir" "$s" "src/by-file.sh gone/" ;;
      *) stub "$dir" "$s" "src/$(basename "$s")" ;;
    esac
    : > "$dir/src/$(basename "$s")"
  done
  : > "$dir/src/by-file.sh"
  : > "$dir/lib/x.sh"
  git -C "${dir:?}" init -q -b main
  g "$dir" add -A
  g "$dir" commit -q -m init
  git -C "${dir:?}" switch -q -c feature
  echo "$dir"
}

# expect <name> <dir> <ran> <skipped> [args...] — run the runner with the args; it must exit 0,
# every script of the list <ran> must have run, and none of the list <skipped>.
expect() {
  local name="$1" dir="$2" ran="$3" skipped="$4"
  shift 4
  local out rc s ok=1
  out="$(bash "$dir/scripts/check-all.sh" "$@" 2>&1)"; rc=$?
  ((rc == 0)) || ok=0
  for s in $ran; do [[ "$out" == *"RAN $s"* ]] || ok=0; done
  for s in $skipped; do [[ "$out" != *"RAN $s"* ]] || ok=0; done
  if ((ok)); then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# expect_exit <name> <code> <dir> [args...] — run the runner; it must exit with the code.
expect_exit() {
  local name="$1" want="$2" dir="$3"
  shift 3
  local rc
  bash "$dir/scripts/check-all.sh" "$@" > "$work/out" 2>&1; rc=$?
  if ((rc == want)); then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want, got $rc)" >&2
  sed 's/^/    /' "$work/out" >&2
}

# --- cases ---------------------------------------------------------------------------------

d="$(repo full)"
expect "without --changed everything runs" "$d" "${scripts[*]}" ""

# The bash floor (ADR-0002): below it the runner stops before any check and names both versions;
# at it, the guard says nothing.
# Verifies: ADR-0002
d="$(repo old-bash)"
out="$(CHECK_ALL_BASH_VERSION=3.2 bash "$d/scripts/check-all.sh" 2>&1)"; rc=$?
if ((rc == 2)) && [[ "$out" == *"need bash 4.4 or later"* && "$out" == *"this is bash 3.2"* && "$out" != *"RAN "* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1)); echo "FAIL: a bash below 4.4 stops the runner before any check (exit $rc)" >&2
fi
out="$(CHECK_ALL_BASH_VERSION=4.4 bash "$d/scripts/check-all.sh" 2>&1)"
if [[ "$out" != *"need bash"* && "$out" == *"RAN "* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1)); echo "FAIL: bash 4.4 passes the guard silently" >&2
fi

d="$(repo untouched)"
expect "--changed on an unchanged branch runs the checks and the self-tests without a line or with a missing input" \
  "$d" "${checks[*]} $no_line $missing" "$by_file $by_dir $others" --changed

d="$(repo file)"
echo change > "$d/src/by-file.sh"
expect "--changed runs a self-test whose input file changed uncommitted" \
  "$d" "$by_file" "$by_dir $others" --changed

d="$(repo committed)"
echo change > "$d/lib/x.sh"
g "$d" commit -q -am change
expect "--changed runs a self-test whose input directory changed in a commit" \
  "$d" "$by_dir" "$by_file $others" --changed

d="$(repo untracked)"
: > "$d/lib/new.sh"
expect "--changed counts an untracked file" "$d" "$by_dir" "$by_file $others" --changed

d="$(repo special-untracked)"
: > "$d/lib/ä.sh"
expect "--changed reads an untracked path with a special character" "$d" "$by_dir" "$by_file $others" --changed

d="$(repo special-committed)"
printf x > "$d/lib/a \"b\".sh"
g "$d" add -A
g "$d" commit -q -m quote
expect "--changed reads a committed path with a quote" "$d" "$by_dir" "$by_file $others" --changed

d="$(repo itself)"
echo "# edited" >> "$d/$by_file"
expect "--changed runs a self-test that itself changed" "$d" "$by_file" "$by_dir $others" --changed

d="$(repo prefix)"
: > "$d/library.sh"
expect "--changed reads a directory input as a directory, not as a prefix" "$d" "" "$by_dir" --changed

d="$(repo origin-main)"
git -C "${d:?}" update-ref refs/remotes/origin/main main
echo change > "$d/lib/x.sh"
g "$d" commit -q -am change
git -C "${d:?}" branch -f main feature
expect "--changed compares with origin/main, not with a local main moved to the branch" \
  "$d" "$by_dir" "$by_file $others" --changed

d="$(repo on-main)"
git -C "${d:?}" switch -q main
expect "--changed on main runs everything" "$d" "${scripts[*]}" "" --changed

# The checks run side by side; their output follows the order of the list, the first check
# finishing last included, and one at a time on CHECK_ALL_JOBS=1.
d="$(repo order)"
echo "sleep 0.5" >> "$d/${scripts[0]}"
want="$(printf 'RAN %s\n' "${scripts[@]}")"
for jobs in "" 1; do
  got="$(CHECK_ALL_JOBS="$jobs" bash "$d/scripts/check-all.sh" 2>&1 | grep '^RAN ')"
  check "the output follows the order of the list${jobs:+ on CHECK_ALL_JOBS=$jobs}" \
    test "$got" = "$want"
done

# at_once <dir> <jobs> — the most checks the runner had running at once with CHECK_ALL_JOBS at
# jobs: each stub notes a directory of its own while it runs, and how many there are.
at_once() {
  local s
  mkdir -p "$1/running"
  for s in "${scripts[@]}"; do
    # shellcheck disable=SC2016 # the stub's variables, not this script's
    printf 'mkdir "%s/running/$$"; ls "%s/running" | wc -l >> "%s/counts"; sleep 0.1; rmdir "%s/running/$$"\n' \
      "$1" "$1" "$1" "$1" >> "$1/$s"
  done
  : > "$1/counts"
  CHECK_ALL_JOBS="$2" bash "$1/scripts/check-all.sh" > /dev/null 2>&1
  sort -n "$1/counts" | tail -n 1 | tr -d ' '
}

d="$(repo capped)"
check "CHECK_ALL_JOBS=2 runs no more than two checks at once" test "$(at_once "$d" 2)" -le 2
d="$(repo uncapped)"
check "the checks run side by side" test "$(at_once "$d" "${#scripts[@]}")" -gt 2

d="$(repo jobs-zero)"
CHECK_ALL_JOBS=0 expect_exit "CHECK_ALL_JOBS=0 exits 2" 2 "$d"

# A runner stopped by TERM stops the checks it started and waits for them: none finishes after
# it, and one that cleans up on TERM has done so when the runner returns.
d="$(repo stopped)"
# shellcheck disable=SC2016 # the stub's variable, not this script's
printf 'trap '\''sleep 0.3; touch "%s/cleaned"; exit 1'\'' TERM; sleep 5 & wait $!\n' "$d" \
  >> "$d/${scripts[0]}"
for s in "${scripts[@]}"; do
  printf 'touch "%s/started"; sleep 1; touch "%s/finished"\n' "$d" "$d" >> "$d/$s"
done
bash "$d/scripts/check-all.sh" > /dev/null 2>&1 &
runner=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do [[ -e "$d/started" ]] && break; sleep 0.1; done
kill -TERM "$runner"
wait "$runner"; rc=$?
check "a runner stopped by TERM exits 143" test "$rc" -eq 143
check "and had started a check" test -e "$d/started"
check "and returns once a check that cleans up on TERM has done so" test -e "$d/cleaned"
sleep 1.2
check "and no check it started finishes after it" test ! -e "$d/finished"

d="$(repo unknown)"
expect_exit "an unknown argument exits 2" 2 "$d" --everything

d="$(repo red)"
printf 'exit 1\n' >> "$d/${checks[0]}"
expect_exit "--changed fails on a red check" 1 "$d" --changed

# --- summary -------------------------------------------------------------------------------

summary "check-all self-test"
