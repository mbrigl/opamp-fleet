# shellcheck shell=bash
# Shared by the self-tests under scripts/ (scripts/test-*.sh), sourced as their first act: the
# repository root, a temporary directory for the fixtures, the counters, the guard on a fixture's
# path, and the summary.
#
# The checks a self-test exercises build fixture repositories under $work. Git hands a hook, a
# linked worktree, and 'git -c' variables that bind every git command to one repository;
# inherited, a fixture's commands would act on this one and could push from it. They are unset
# here, so a self-test run from anywhere is as safe as one run by hand (see .githooks/pre-push).
#
# Usage, from scripts/test-<name>.sh:
#     # shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
#     source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
#     ...
#     summary "<Name> self-test"

# shellcheck disable=SC2034 # ROOT is the sourcing self-test's
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# shellcheck disable=SC2046 # one variable name per word
unset $(git rev-parse --local-env-vars)

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failed=0
passed=0

# fixture <dir> — refuse a path that is no fixture repository under $work: 'git -C ""' acts on
# this repository, and a path outside $work may be this repository or another one.
fixture() {
  [[ -n "${1:-}" && "$1" == "$work"/* && -d "$1/.git" ]] \
    || { echo "fixture: '${1:-}' is no fixture repository under $work" >&2; exit 1; }
}

# summary <name> — the result line, and the exit code: 0 when every case passed, 1 otherwise.
summary() {
  if ((failed)); then
    echo "$1 FAILED ($failed of $((passed + failed)) cases)." >&2
    exit 1
  fi
  echo "$1 passed ($passed cases)."
  exit 0
}
