#!/usr/bin/env bash
#
# Self-test of scripts/check-manual.sh.
#
# Each case writes a docs/manual/ of its own under a temporary directory, runs the check against
# it, and asserts the exit code and, for a failing case, the file and line the message names. The
# manual that cites nothing passes; every other case cites the record in one of the forms a
# citation takes.
#
# Inputs: scripts/check-manual.sh scripts/lib/
#
# Usage:
#     scripts/test-check-manual.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"

CHECK="$ROOT/scripts/check-manual.sh"

# manual <name> <text> — a repository whose docs/manual/README.md holds the text, printed as its
# path.
manual() {
  local d="$work/$1"
  mkdir -p "$d/docs/manual"
  printf '%s\n' "$2" > "$d/docs/manual/README.md"
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

d="$work/no-manual"
mkdir -p "$d"
expect pass "a repository without a manual" "$d"

d="$(manual plain $'# Manual\n\nStart the Server, then the Client.')"
expect pass "a manual that cites no ADR" "$d"

d="$(manual cites-number $'# Manual\n\nThe Client verifies every package (ADR-0001).')"
expect fail "a manual that names an ADR" "$d" "docs/manual/README.md:3: cites an ADR"

d="$(manual links-record $'# Manual\n\nSee [why](../adr/0001-decision.md).')"
expect fail "a manual that links into the decision record" "$d" "docs/manual/README.md:3: cites an ADR"

d="$(manual reference-link $'# Manual\n\nSee [why].\n\n[why]: ../adr/0001-decision.md')"
expect fail "a reference-style link into the decision record" "$d" "docs/manual/README.md:5: cites an ADR"

d="$(manual angle-link $'# Manual\n\nSee <../adr/0001-decision.md>.')"
expect fail "an angle-bracket link into the decision record" "$d" "docs/manual/README.md:3: cites an ADR"

d="$(manual names-path $'# Manual\n\n```\nless docs/adr/README.md\n```')"
expect fail "a citation in a code fence counts" "$d" "docs/manual/README.md:4: cites an ADR"

summary "Manual check self-test"
