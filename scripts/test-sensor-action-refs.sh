#!/usr/bin/env bash
#
# Self-test of scripts/sensor-action-refs.sh.
#
# Each case writes a fixture repository with one workflow, runs the sensor against it, and asserts
# the exit code and a line the reviewer would read. A workflow in one form reports nothing; every
# other case is one thing the sensor reports, or one it must stay silent on. The cases run with
# --require hold it as a check: the decided form passes, and every other form fails.
#
# Verifies: ADR-0002
# Inputs: scripts/sensor-action-refs.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-action-refs.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-action-refs.sh"

# repo <name> <uses line...> — a fixture whose one workflow runs the given steps, printed as its
# path.
repo() {
  local dir="$work/$1"; shift
  mkdir -p "$dir/.github/workflows"
  {
    echo "name: Checks"
    echo "on: [pull_request]"
    echo "jobs:"
    echo "  docs:"
    echo "    runs-on: ubuntu-latest"
    echo "    steps:"
    printf '      %s\n' "$@"
    echo "      - run: bash scripts/check-docs.sh"
  } > "$dir/.github/workflows/checks.yml"
  echo "$dir"
}

# expect <name> <dir> <needle> [absent] — run the sensor; it must exit 0 and its output contain the
# needle, and not the second needle when one is given.
expect() {
  local name="$1" dir="$2" needle="$3" absent="${4:-}"
  local out rc
  out="$(bash "$SENSOR" "$dir" 2>&1)"; rc=$?
  if ((rc == 0)) && [[ "$out" == *"$needle"* ]] && [[ -z "$absent" || "$out" != *"$absent"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit 0 with \"$needle\"${absent:+ and without \"$absent\"}, exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

SHA_A=0123456789abcdef0123456789abcdef01234567
SHA_B=89abcdef0123456789abcdef0123456789abcdef

expect "major tags only report nothing" \
  "$(repo tags "- uses: actions/checkout@v7" "- uses: actions/setup-node@v5")" \
  "2 on a major tag, 0 on a full version tag, 0 on a commit SHA — nothing to report"

expect "commit SHAs with version comments only report nothing" \
  "$(repo shas "- uses: actions/checkout@$SHA_A # v7.0.1" "- uses: owner/action@$SHA_B # v2.3.0")" \
  "0 on a major tag, 0 on a full version tag, 2 on a commit SHA — nothing to report"

expect "a major tag beside a commit SHA is a mix" \
  "$(repo mixed "- uses: actions/checkout@v7" "- uses: owner/action@$SHA_A")" \
  "mixed forms — 1 on a major tag, 0 on a full version tag, 1 on a commit SHA"

expect "a full version tag beside a major tag is a mix" \
  "$(repo full "- uses: actions/checkout@v7" "- uses: owner/action@v2.3.0")" \
  "mixed forms — 1 on a major tag, 1 on a full version tag"

expect "a branch is no release" \
  "$(repo branch "- uses: owner/action@main")" \
  "'owner/action@main' references 'main', which is neither a version tag nor a commit SHA"

expect "a short SHA is no full commit SHA" \
  "$(repo short-sha "- uses: owner/action@0a1b2c3")" \
  "references '0a1b2c3', which is neither"

expect "an action without a ref is reported" \
  "$(repo no-ref "- uses: owner/action")" \
  "'owner/action' names no ref"

expect "a docker image without a tag is reported" \
  "$(repo docker-untagged "- uses: docker://alpine")" \
  "'docker://alpine' carries no tag"

expect "a docker image on a registry port without a tag is reported" \
  "$(repo docker-port "- uses: docker://registry.example:5000/tool")" \
  "carries no tag"

expect "a tagged docker image and a local action are not counted" \
  "$(repo exempt "- uses: actions/checkout@v7" "- uses: docker://alpine:3.20" "- uses: ./.github/actions/local")" \
  "1 on a major tag, 0 on a full version tag, 0 on a commit SHA — nothing to report"

expect "a commented-out reference is not read" \
  "$(repo commented "- uses: actions/checkout@v7" "# - uses: owner/action@main")" \
  "nothing to report" "@main"

expect "a quoted reference with a trailing comment is read like any other" \
  "$(repo quoted "- uses: 'actions/checkout@v7' # pinned by major")" \
  "1 on a major tag, 0 on a full version tag, 0 on a commit SHA — nothing to report"

expect "a double-quoted reference is read like any other" \
  "$(repo double-quoted '- uses: "owner/action@main"')" \
  "'owner/action@main' references 'main'"

expect "a reusable workflow is counted by its ref" \
  "$(repo reusable "- uses: actions/checkout@v7" "- uses: owner/repo/.github/workflows/build.yml@v2")" \
  "2 on a major tag, 0 on a full version tag, 0 on a commit SHA — nothing to report"

expect "a tag without a leading v is a version tag" \
  "$(repo no-v "- uses: actions/checkout@v7" "- uses: owner/action@3")" \
  "2 on a major tag, 0 on a full version tag, 0 on a commit SHA — nothing to report"

empty="$work/empty"
mkdir -p "$empty"
expect "a repository without workflows has nothing to report" "$empty" "no workflow under .github/workflows"

# require <want exit> <name> <form> <dir> <needle> — run the sensor as a check with
# '--require <form>'; it must exit as wanted and its output contain the needle.
require() {
  local want="$1" name="$2" form="$3" dir="$4" needle="$5"
  local out rc
  out="$(bash "$SENSOR" --require "$form" "$dir" 2>&1)"; rc=$?
  if ((rc == want)) && [[ "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# dependabot <dir> — give the fixture the github-actions entry a major tag relies on.
dependabot() {
  printf 'version: 2\nupdates:\n  - package-ecosystem: github-actions\n    directory: /\n' > "$1/.github/dependabot.yml"
  echo "$1"
}

require 0 "major tags with Dependabot pass the major-tag check" major-tag \
  "$(dependabot "$(repo req-major "- uses: actions/checkout@v7")")" \
  "every reference is in the decided form"
require 1 "a major tag without Dependabot fails the major-tag check" major-tag \
  "$(repo req-major-nodep "- uses: actions/checkout@v7")" \
  "no 'github-actions' entry"
require 1 "a SHA fails the major-tag check" major-tag \
  "$(dependabot "$(repo req-major-sha "- uses: actions/checkout@v7" "- uses: owner/action@$SHA_A")")" \
  "is on a commit SHA, not on a major tag"
require 0 "SHAs pass the sha check without Dependabot" sha \
  "$(repo req-sha "- uses: actions/checkout@$SHA_A # v7.0.1" "- uses: owner/action@$SHA_B")" \
  "every reference is in the decided form"
require 1 "a major tag fails the sha check" sha \
  "$(repo req-sha-major "- uses: actions/checkout@v7")" \
  "is on a major tag, not on a commit SHA"
require 1 "a full version tag fails either check" sha \
  "$(repo req-full "- uses: owner/action@v7.0.1")" \
  "is on a full version tag"
require 1 "a branch fails either check" major-tag \
  "$(dependabot "$(repo req-branch "- uses: owner/action@main")")" \
  "references 'main'"
require 1 "an untagged image fails either check" sha \
  "$(repo req-docker "- uses: docker://alpine")" \
  "'docker://alpine' carries no tag"

out="$(bash "$SENSOR" --require tags "$work/req-major" 2>&1)"; rc=$?
if ((rc == 2)) && [[ "$out" == *"takes 'major-tag' or 'sha'"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: an unknown form (expected exit 2, exit $rc)" >&2
fi

out="$(bash "$SENSOR" "$work/does-not-exist" 2>&1)"; rc=$?
if ((rc == 2)) && [[ "$out" == *"is not a directory"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: a missing root (expected exit 2, exit $rc)" >&2
fi

summary "Action references sensor self-test"
