#!/usr/bin/env bash
#
# Self-test of scripts/sensor-binding-adrs.sh. Verifies: ADR-0001
#
# Each case builds a throwaway git repository with ADRs under docs/adr/, commits a change on
# top of a tagged base, runs the sensor against the base, and asserts the exit code and a line a
# reader would meet. The cases cover a directory path and a file path in an Applies to line, a
# path its prose only resembles, an ADR no longer in force, an ADR the change itself adds, a
# change no ADR names, nothing to compare, and a base that does not resolve.
#
# Verifies: ADR-0002
# Inputs: scripts/sensor-binding-adrs.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-binding-adrs.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-binding-adrs.sh"

# commit <dir> <message> — commit everything in the fixture.
commit() {
  fixture "$1"
  git -C "${1:?}" add -A
  git -C "${1:?}" -c user.name=fixture -c user.email=fixture@example.invalid commit -q -m "$2"
}

# adr_id <n> — the identifier ADR-000n, assembled so that this file carries no literal one: a
# normalization rewrites every literal one in the tree.
adr_id() { printf "ADR-%04d" "$1"; }

# adr <dir> <number> <status emoji and word> <applies to> — write an ADR header.
adr() {
  mkdir -p "$1/docs/adr"
  printf '# ADR-%s: Fixture\n\n- **Status:** %s\n- **Applies to:** %s\n' "$2" "$3" "$4" \
    > "$1/docs/adr/$2-fixture.md"
}

# repo <name> — a repository with three ADRs and some files, its first commit tagged 'base'.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/plan" "$dir/src"
  git -C "${dir:?}" init -q -b main
  # shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
  adr "$dir" 0001 "🟢 accepted" 'the plan under `docs/plan/` and `README.md`'
  # shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
  adr "$dir" 0002 "⚪ superseded by $(adr_id 3)" 'everything under `src/`'
  # shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
  adr "$dir" 0003 "🟡 proposed" 'the code under `src/`, and the `pull request` template'
  printf 'readme\n' > "$dir/README.md"
  printf 'readme of the plan\n' > "$dir/docs/plan/README.md"
  printf 'code\n' > "$dir/src/app.sh"
  commit "$dir" "base"
  git -C "${dir:?}" tag base
  echo "$dir"
}

# expect <exit code> <name> <dir> <needle> [base] — run the sensor in the repository against the
# given base (default 'base'); the output must contain the needle.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4" base="${5-base}"
  local out rc
  fixture "$dir"
  out="$(bash "$SENSOR" --root "$dir" "$base" ${HEADREF:+"$HEADREF"} 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# reject <name> <dir> <needle> — the sensor's output must not contain the needle.
reject() {
  local name="$1" dir="$2" needle="$3" out
  fixture "$dir"
  out="$(bash "$SENSOR" --root "$dir" base 2>&1)"
  if [[ "$out" != *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (\"$needle\" in the output)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

d="$(repo same)"
expect 0 "base and head the same is nothing to compare" "$d" "nothing to compare"
expect 2 "a base that does not resolve fails" "$d" "does not resolve" "no-such-ref"

d="$(repo dir)"
printf 'more\n' >> "$d/docs/plan/README.md"
commit "$d" "touch the plan"
expect 0 "a path under a directory of Applies to binds" "$d" "- $(adr_id 1) (docs/adr/0001-fixture.md) — docs/plan/README.md"
expect 0 "one ADR in force names the change" "$d" "1 ADR(s) in force name one of them"

d="$(repo file)"
printf 'more\n' >> "$d/README.md"
commit "$d" "touch the readme"
expect 0 "a file named in Applies to binds" "$d" "- $(adr_id 1) (docs/adr/0001-fixture.md) — README.md"

d="$(repo prefix)"
printf 'x\n' > "$d/README.md.bak"
commit "$d" "a file that only begins like one"
expect 0 "a file named in Applies to binds only itself" "$d" "no ADR in force names one of them"

d="$(repo code)"
printf 'more\n' >> "$d/src/app.sh"
commit "$d" "touch the code"
expect 0 "a proposed ADR binds" "$d" "- $(adr_id 3) (docs/adr/0003-fixture.md) — src/app.sh"
reject "a superseded ADR does not bind" "$d" "$(adr_id 2)"
reject "a backticked phrase with a space is no path" "$d" "pull request"

d="$(repo added)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
adr "$d" 0004 "🟡 proposed" 'the tools under `tools/`'
mkdir -p "$d/tools" && printf 'tool\n' > "$d/tools/t.sh"
commit "$d" "a new ADR with its first tool"
expect 0 "an ADR the change adds is read as the head has it" "$d" "- $(adr_id 4) (docs/adr/0004-fixture.md) — tools/t.sh"

d="$(repo none)"
printf 'notes\n' > "$d/NOTES.txt"
commit "$d" "notes"
expect 0 "a change no ADR names says so" "$d" "1 path(s) changed; no ADR in force names one of them"

d="$(repo many)"
for n in 1 2 3 4 5; do printf '%s\n' "$n" > "$d/src/f$n.sh"; done
commit "$d" "five files"
expect 0 "more than three hits are counted, not listed" "$d" "src/f1.sh src/f2.sh src/f3.sh, and 2 more"

d="$(repo worktree)"
printf 'more\n' >> "$d/src/app.sh"
expect 0 "an uncommitted change binds before the first commit" "$d" "- $(adr_id 3) (docs/adr/0003-fixture.md) — src/app.sh"
printf 'new\n' > "$d/src/new.sh"
expect 0 "an untracked file is part of the change" "$d" "src/app.sh src/new.sh"
HEADREF=HEAD expect 0 "with a head, the working tree is not read" "$d" "nothing to compare"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
adr "$d" 0001 "🔴 rejected" 'the plan under `docs/plan/` and `README.md`'
printf 'more\n' >> "$d/README.md"
reject "the working tree's own ADRs are read without a head" "$d" "$(adr_id 1)"

d="$(repo names)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
adr "$d" 0004 "🟢 accepted" 'the `Makefile`, the hooks under `.githooks`, `docs/über.md`, and the `plan-feature` skill'
mkdir -p "$d/.githooks" && printf 'all:\n' > "$d/Makefile" && printf 'x\n' > "$d/.githooks/pre-push"
printf 'ü\n' > "$d/docs/über.md"
commit "$d" "names"
git -C "${d:?}" tag -f base >/dev/null
printf 'more\n' >> "$d/Makefile"
expect 0 "a file without an extension binds" "$d" "$(adr_id 4) (docs/adr/0004-fixture.md) — Makefile"
git -C "${d:?}" checkout -q -- Makefile
printf 'more\n' >> "$d/.githooks/pre-push"
expect 0 "a directory written without its slash binds what is under it" "$d" "— .githooks/pre-push"
git -C "${d:?}" checkout -q -- .githooks
printf 'more\n' >> "$d/docs/über.md"
expect 0 "a path outside ASCII binds" "$d" "— docs/über.md"
git -C "${d:?}" checkout -q -- docs
rm "$d/Makefile"
expect 0 "a deleted file binds" "$d" "$(adr_id 4) (docs/adr/0004-fixture.md) — Makefile"

# --- summary ---------------------------------------------------------------------------------

summary "Binding ADRs sensor self-test"
