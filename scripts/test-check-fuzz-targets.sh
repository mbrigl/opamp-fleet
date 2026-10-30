#!/usr/bin/env bash
#
# Self-test of scripts/check-fuzz-targets.sh.
#
# Each case writes a fuzz/Cargo.toml, a .github/workflows/fuzz.yml and fuzz/seeds/ of its own
# under a temporary directory, runs the check against it, and asserts the exit code and, for a
# failing case, the target and the place the message names. Two targets in all three places pass;
# every other case leaves one of them out of one place.
#
# Inputs: scripts/check-fuzz-targets.sh scripts/lib/
#
# Usage:
#     scripts/test-check-fuzz-targets.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"

CHECK="$ROOT/scripts/check-fuzz-targets.sh"

# repo <name> <bins> <matrix> <seeds> — a repository whose fuzz/Cargo.toml declares the
# space-separated bins, whose fuzz.yml matrix lists the targets, and whose fuzz/seeds/ holds a
# directory per seed name, printed as its path.
repo() {
  local d="$work/$1" t
  mkdir -p "$d/fuzz/seeds" "$d/.github/workflows"
  {
    printf '[package]\nname = "fuzz"\n\n[dependencies]\nlibfuzzer-sys = "0.4"\n'
    for t in $2; do
      printf '\n[[bin]]\nname = "%s"\npath = "fuzz_targets/%s.rs"\ntest = false\n' "$t" "$t"
    done
  } > "$d/fuzz/Cargo.toml"
  {
    printf 'jobs:\n  fuzz:\n    strategy:\n      fail-fast: false\n      matrix:\n        target:\n'
    for t in $3; do printf '          - %s\n' "$t"; done
    printf '    env:\n      SECONDS_PER_TARGET: 60\n    steps:\n      - run: echo\n'
  } > "$d/.github/workflows/fuzz.yml"
  for t in $4; do mkdir -p "$d/fuzz/seeds/$t"; done
  echo "$d"
}

# expect <pass|fail> <name> <dir> [needle] — run the check on the fixture; a failing case must
# name the needle.
expect() {
  local want="$1" name="$2" dir="$3" needle="${4:-}" out rc ok=0
  out="$(bash "$CHECK" "$dir" 2>&1)"; rc=$?
  if [[ "$want" == pass ]]; then
    ((rc == 0)) && ok=1
  else
    ((rc == 1)) && [[ "$out" == *"$needle"* ]] && ok=1
  fi
  check "$name" test "$ok" -eq 1
  ((ok)) || while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

d="$work/no-fuzz"
mkdir -p "$d"
expect pass "a repository without fuzz targets" "$d"

d="$(repo same "pem frame" "frame pem" "pem frame")"
expect pass "the same targets in all three places" "$d"

d="$(repo no-matrix "pem frame" "pem" "pem frame")"
expect fail "a target missing from the matrix" "$d" \
  "frame: missing from the matrix of .github/workflows/fuzz.yml"

d="$(repo no-seeds "pem frame" "pem frame" "pem")"
expect fail "a target without seeds" "$d" "frame: no directory fuzz/seeds/frame"

d="$(repo no-bin "pem" "pem frame" "pem frame")"
expect fail "a matrix and seeds for a target Cargo.toml lacks" "$d" \
  "frame: no [[bin]] in fuzz/Cargo.toml"

d="$(repo empty-matrix "pem" "" "pem")"
expect fail "a workflow whose matrix lists no target" "$d" "no matrix 'target:' list found"

d="$(repo no-workflow "pem" "pem" "pem")"
rm "$d/.github/workflows/fuzz.yml"
expect fail "a repository without the fuzz workflow" "$d" ".github/workflows/fuzz.yml: not found"

summary "Fuzz target check self-test"
