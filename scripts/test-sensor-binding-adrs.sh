#!/usr/bin/env bash
#
# Self-test of scripts/sensor-binding-adrs.sh. Verifies: ADR-0001
#
# Each case builds a throwaway git repository with ADRs under docs/adr/, commits a change on
# top of a tagged base, runs the sensor against the base, and asserts the exit code and a line a
# reader would meet. The cases cover a directory path and a file path in an Applies to line, a
# path its prose only resembles, an ADR no longer in force, an ADR the change itself adds, a
# change no ADR names, nothing to compare, and a base that does not resolve; a Rust file binds the
# modules under the directory of its name, a Rust library root and a file of another kind do not.
# Paths named with --path stand in for a change, and --sections prints the header, Decision, and
# Enforcement of each ADR listed, and nothing else of it, under a banner that names the sections
# ADR_RULE_SECTIONS lists, and the Applies to of each other ADR in force whose Applies to is
# prose, also where the prose comes before a list longer than a pipe holds.
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

# expect_with <exit code> <name> <dir> <needle> <argument>... — run the sensor in the repository
# with the given arguments; the output must contain the needle.
expect_with() {
  local want="$1" name="$2" dir="$3" needle="$4" out rc
  shift 4
  fixture "$dir"
  out="$(bash "$SENSOR" --root "$dir" "$@" 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# lacks_with <name> <dir> <needle> <argument>... — with the given arguments, the sensor's output
# must not contain the needle.
lacks_with() {
  local name="$1" dir="$2" needle="$3" out
  shift 3
  fixture "$dir"
  out="$(bash "$SENSOR" --root "$dir" "$@" 2>&1)"
  if [[ "$out" != *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (\"$needle\" in the output)" >&2
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

d="$(repo rust-module)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
adr "$d" 0004 "🟢 accepted" 'the graph of `crates/engine/src/graph.rs`'
mkdir -p "$d/crates/engine/src/graph"
printf 'mod\n' > "$d/crates/engine/src/graph/hit.rs"
commit "$d" "a module of the graph"
expect 0 "a Rust file binds the modules under the directory of its name" "$d" \
  "- $(adr_id 4) (docs/adr/0004-fixture.md) — crates/engine/src/graph/hit.rs"

# base <dir> — fold what the fixture added into the base commit and tag it again.
base() {
  git -C "${1:?}" add -A
  git -C "${1:?}" -c user.name=fixture -c user.email=fixture@example.invalid commit -q --amend -m base
  git -C "${1:?}" tag -f base >/dev/null
}

d="$(repo rust-root)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
adr "$d" 0004 "🟢 accepted" 'the crate root `crates/engine/src/lib.rs` and `build.rs`'
mkdir -p "$d/crates/engine/src/lib" "$d/build"
printf 'x\n' > "$d/crates/engine/src/lib/part.rs"
printf 'x\n' > "$d/build/asset.txt"
base "$d"
printf 'more\n' >> "$d/crates/engine/src/lib/part.rs"
printf 'more\n' >> "$d/build/asset.txt"
commit "$d" "files under directories named like a library root and a build script"
expect 0 "a Rust library root or build script binds no directory of its name" "$d" \
  "no ADR in force names one of them"

d="$(repo other-kind)"
mkdir -p "$d/tools/app"
printf 'x\n' > "$d/tools/app/part.sh"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
adr "$d" 0004 "🟢 accepted" 'the script `tools/app.sh`'
base "$d"
printf 'more\n' >> "$d/tools/app/part.sh"
commit "$d" "a file under a directory named like a script"
expect 0 "a file of another kind binds no directory of its name" "$d" \
  "no ADR in force names one of them"

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

d="$(repo named)"
expect_with 0 "a named path binds before anything changed" "$d" \
  "1 path(s) named; 1 ADR(s) in force name one of them" --path src/app.sh
expect_with 0 "a named path lists the ADR that names it" "$d" \
  "- $(adr_id 3) (docs/adr/0003-fixture.md) — src/app.sh" --path ./src/app.sh
expect_with 0 "a named directory binds an ADR that names a path under it" "$d" \
  "- $(adr_id 1) (docs/adr/0001-fixture.md) — docs/" --path docs/
expect_with 2 "--path without a path fails" "$d" "--path needs a path" --path

d="$(repo sections)"
{
  printf '\n## Context\n\nWhy it came up.\n\n## Decision\n\nWe will plan in the tree.\n'
  printf '\n## Alternatives considered\n\n- None.\n\n## Enforcement\n\nA sensor lists it.\n'
} >> "$d/docs/adr/0001-fixture.md"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
{
  printf '# ADRs\n\n## Mechanics\n\nHow the record is kept.\n\n## Index\n\n### Binding\n\n'
  printf '| ADR | Title | Applies to | Status |\n|-----|-------|-----|-----|\n'
  printf '| [0001](0001-fixture.md) | Fixture | the plan under `docs/plan/` and `README.md` | 🟢 |\n'
  printf '| [0003](0003-fixture.md) | Code | the code under `src/`, and the `pull request` template | 🟡 |\n'
  printf '| [0005](0005-fixture.md) | Paths | `tools/`, `lib/x.sh`, and `Makefile` | 🟢 |\n'
  printf '\n### Superseded and rejected\n\n| [0002](0002-fixture.md) | Old | prose too | ⚪ |\n'
} > "$d/docs/adr/README.md"
base "$d"
expect_with 0 "--sections prints the Applies to of an ADR in force with prose" "$d" \
  "- $(adr_id 3): the code under" --sections --path README.md
lacks_with "--sections leaves out an ADR it lists" "$d" "- $(adr_id 1):" --sections --path README.md
lacks_with "--sections leaves out an ADR whose Applies to is paths alone" "$d" "- $(adr_id 5):" \
  --sections --path README.md
expect_with 0 "--sections prints the header" "$d" "- **Applies to:** the plan" --sections --path README.md
expect_with 0 "--sections prints the Decision" "$d" "We will plan in the tree." --sections --path README.md
expect_with 0 "--sections prints the Enforcement" "$d" "A sensor lists it." --sections --path README.md
lacks_with "--sections leaves out the Context" "$d" "Why it came up." --sections --path README.md
lacks_with "--sections leaves out the Alternatives" "$d" "- None." --sections --path README.md
lacks_with "--sections leaves out the mechanics of the index" "$d" "How the record is kept." \
  --sections --path README.md
lacks_with "--sections leaves out the rows of dead ADRs" "$d" "prose too" --sections --path README.md
expect_with 0 "--sections points at the ADRs it prints, not at the index" "$d" \
  "Read these, and the ADRs whose Applies to is prose, below:" --sections --path README.md
lacks_with "--sections sends no session to the index" "$d" "Read the index" --sections --path NOTES.txt
expect_with 0 "without --sections the index is named" "$d" "Read the index" --path NOTES.txt
lacks_with "without --sections nothing of an ADR is printed" "$d" "We will plan" --path README.md
mv "$d/docs/adr/README.md" "$d/README.adr"
expect_with 0 "--sections says so when it finds no Binding table" "$d" "No row found under '### Binding'" \
  --sections --path README.md
mv "$d/README.adr" "$d/docs/adr/README.md"
expect_with 0 "--sections prints the ADRs with prose when nothing changed" "$d" \
  "- $(adr_id 1): the plan under" --sections base

# An Applies to whose prose comes before a list longer than a pipe holds: a grep that exits at
# the prose before the list is written cuts the writer off, and the ADR goes unlisted.
d="$(repo sections-long)"
pad="$(printf '%*s' 200000 '' | tr ' ' z)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
{
  printf '# ADRs\n\n## Index\n\n### Binding\n\n'
  printf '| ADR | Title | Applies to | Status |\n|-----|-------|-----|-----|\n'
  printf '| [0007](0007-fixture.md) | Long | every document, `%s`, and `%s` | 🟢 |\n' "$pad" "$pad"
} > "$d/docs/adr/README.md"
base "$d"
expect_with 0 "--sections prints prose before a list longer than a pipe holds" "$d" \
  "- $(adr_id 7): every document" --sections --path README.md

# The banner of an ADR's sections names the sections ADR_RULE_SECTIONS lists, as a copy of the
# sensor whose library lists three of them prints.
d="$(repo sections-named)"
printf '\n## Decision\n\nWe will plan.\n\n## Consequences\n\nIt costs.\n\n## Enforcement\n\nA sensor.\n' \
  >> "$d/docs/adr/0001-fixture.md"
base "$d"
mkdir -p "$d/scripts" && cp "$SENSOR" "$d/scripts/" && cp -R "$ROOT/scripts/lib" "$d/scripts/"
lib="$d/scripts/lib/adr.sh"
sed "s/^ADR_RULE_SECTIONS=.*/ADR_RULE_SECTIONS='Decision|Consequences|Enforcement'/" "$lib" > "$lib.tmp" \
  && cat "$lib.tmp" > "$lib" && rm "$lib.tmp"
SENSOR="$d/scripts/sensor-binding-adrs.sh" expect_with 0 "--sections names the sections it prints" "$d" \
  "): the header, the Decision, the Consequences, and the Enforcement ===" --sections --path README.md
SENSOR="$d/scripts/sensor-binding-adrs.sh" expect_with 0 "--sections prints each section it names" "$d" \
  "It costs." --sections --path README.md
expect_with 0 "--sections names the two sections of the template" "$d" \
  "): the header, the Decision, and the Enforcement ===" --sections --path README.md

# --- summary ---------------------------------------------------------------------------------

summary "Binding ADRs sensor self-test"
