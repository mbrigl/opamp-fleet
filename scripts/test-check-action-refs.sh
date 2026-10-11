#!/usr/bin/env bash
#
# Self-test of scripts/check-action-refs.sh. Verifies: ADR-0007
#
# Each case builds a fixture under a temporary directory — one workflow with one 'uses:' line and
# a dependabot.yml with the github-actions ecosystem — runs the check against it, and asserts the
# exit code and, for a failing case, the message that names the violation. The baseline passes;
# every other case breaks one thing ADR-0007 decides, or writes a reference in a form the check
# has to read like the plain one.
#
# Usage:
#     scripts/test-check-action-refs.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECK="$ROOT/scripts/check-action-refs.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failed=0
passed=0

# workflow <dir> <uses line...> — write checks.yml with one job whose steps are the given lines.
workflow() {
  local dir="$1"; shift
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
}

# dependabot <dir> <ecosystem line> — write dependabot.yml with one update entry.
dependabot() {
  local dir="$1" ecosystem="$2"
  {
    echo "# Keeps the actions current (a comment the check does not read)."
    echo "version: 2"
    echo "updates:"
    echo "  $ecosystem"
    echo "    directory: /"
    echo "    schedule:"
    echo "      interval: monthly"
  } > "$dir/.github/dependabot.yml"
}

# repo <name> — a fixture that passes the check, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/.github/workflows"
  workflow "$dir" "- uses: actions/checkout@v7"
  dependabot "$dir" "- package-ecosystem: github-actions"
  echo "$dir"
}

# expect <pass|fail> <name> <dir> [needle] — run the check; when a needle is given, its output
# must contain it.
expect() {
  local want="$1" name="$2" dir="$3" needle="${4:-}"
  local out rc
  out="$(bash "$CHECK" "$dir" 2>&1)"; rc=$?
  if [[ "$want" == "pass" && $rc -eq 0 && ( -z "$needle" || "$out" == *"$needle"* ) ]] \
     || [[ "$want" == "fail" && $rc -ne 0 && ( -z "$needle" || "$out" == *"$needle"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected $want${needle:+ with needle \"$needle\"}, exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases: references ------------------------------------------------------------------------

d="$(repo baseline)"
expect pass "an action on its major version tag, Dependabot configured" "$d" "1 references on major version tags"

d="$(repo sha)"
workflow "$d" "- uses: actions/checkout@0e58ed8671d6b60d0890c21b07f8835ace038e67"
expect fail "a commit SHA" "$d" "is not a major version tag"

d="$(repo branch)"
workflow "$d" "- uses: actions/checkout@main"
expect fail "a branch" "$d" "is not a major version tag"

d="$(repo full-version)"
workflow "$d" "- uses: actions/checkout@v7.0.1"
expect fail "a full version tag" "$d" "is not a major version tag"

d="$(repo untagged)"
workflow "$d" "- uses: actions/checkout"
expect fail "no ref at all" "$d" "names no ref"

d="$(repo action-in-subdirectory)"
workflow "$d" "- uses: owner/repo/path/to/action@v2"
expect pass "an action inside a repository, on its major tag" "$d" "1 references"

d="$(repo local-action)"
workflow "$d" "- uses: ./.github/actions/setup"
expect pass "a local action is exempt" "$d" "1 references"

d="$(repo docker-tagged)"
workflow "$d" "- uses: docker://alpine:3.20"
expect pass "a docker image with a tag" "$d" "1 references"

d="$(repo docker-untagged)"
workflow "$d" "- uses: docker://alpine"
expect fail "a docker image without a tag" "$d" "carries no tag"

d="$(repo docker-registry-port)"
workflow "$d" "- uses: docker://registry.example:5000/tools/image"
expect fail "a docker image on a registry with a port, without a tag" "$d" "carries no tag"

d="$(repo docker-registry-port-tagged)"
workflow "$d" "- uses: docker://registry.example:5000/tools/image:1.2"
expect pass "a docker image on a registry with a port, with a tag" "$d" "1 references"

d="$(repo commented-out)"
workflow "$d" "# - uses: actions/checkout@main" "- uses: actions/checkout@v7"
expect pass "a commented-out reference is not read" "$d" "1 references"

d="$(repo trailing-comment)"
workflow "$d" "- uses: actions/checkout@v7 # the major tag, ADR-0007"
expect pass "a trailing comment after the reference" "$d" "1 references"

d="$(repo quoted)"
workflow "$d" "- uses: \"actions/checkout@v7\""
expect pass "a quoted reference" "$d" "1 references"

d="$(repo keyed-form)"
workflow "$d" "- name: Check out" "  uses: actions/checkout@v7"
expect pass "a reference written after a name key, not as the list item" "$d" "1 references"

d="$(repo keyed-form-branch)"
workflow "$d" "- name: Check out" "  uses: actions/checkout@main"
expect fail "the same form on a branch" "$d" "is not a major version tag"

d="$(repo two-workflows)"
cp "$d/.github/workflows/checks.yml" "$d/.github/workflows/nightly.yaml"
workflow "$d" "- uses: actions/checkout@v7"
expect pass "a second workflow with a .yaml extension is read" "$d" "2 references"

d="$(repo no-workflows)"
rm -r "$d/.github/workflows"
expect fail "no workflow at all" "$d" "no workflow found"

# --- cases: Dependabot ------------------------------------------------------------------------

d="$(repo dependabot-missing)"
rm "$d/.github/dependabot.yml"
expect fail "no dependabot.yml" "$d" "dependabot.yml: not found"

d="$(repo dependabot-other-ecosystem)"
dependabot "$d" "- package-ecosystem: npm"
expect fail "a dependabot.yml without the github-actions ecosystem" "$d" "has no 'package-ecosystem: github-actions' entry"

d="$(repo dependabot-commented)"
dependabot "$d" "# - package-ecosystem: github-actions"
expect fail "the github-actions ecosystem only in a comment" "$d" "has no 'package-ecosystem: github-actions' entry"

d="$(repo dependabot-quoted)"
dependabot "$d" "- package-ecosystem: \"github-actions\""
expect pass "the ecosystem value in quotes" "$d" "Dependabot configured"

# --- summary ----------------------------------------------------------------------------------

if ((failed)); then
  echo "Action reference self-test FAILED ($failed of $((passed + failed)) cases)." >&2
  exit 1
fi
echo "Action reference self-test passed ($passed cases)."
exit 0
