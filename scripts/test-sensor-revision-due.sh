#!/usr/bin/env bash
#
# Self-test of scripts/sensor-revision-due.sh. Verifies: ADR-0006
#
# Each case builds a throwaway git repository under a temporary directory with a
# docs/ARCHITECTURE.md carrying the 'Last design revision' line and commits on chosen dates,
# runs the sensor against it, and asserts the exit code and the line a reader would meet. The
# cases cover the count from the first commit and from the commit that set the date, what is not
# counted (a docs-only commit, a merge, a change of the number), the due message with its
# directories and their changes, and the three ways the line fails to read.
#
# Verifies: ADR-0002
# Inputs: scripts/sensor-revision-due.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-revision-due.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-revision-due.sh"

# commit <dir> <date> <message> — stage everything and commit on the given date.
commit() {
  fixture "$1"
  git -C "${1:?}" add -A
  GIT_AUTHOR_DATE="$2T12:00:00" GIT_COMMITTER_DATE="$2T12:00:00" \
    git -C "${1:?}" -c user.name=fixture -c user.email=fixture@example.invalid commit -q --allow-empty -m "$3"
}

# commit_at <dir> <date and time> <message> — stage everything and commit at the given moment.
commit_at() {
  fixture "$1"
  git -C "${1:?}" add -A
  GIT_AUTHOR_DATE="$2" GIT_COMMITTER_DATE="$2" \
    git -C "${1:?}" -c user.name=fixture -c user.email=fixture@example.invalid commit -q --allow-empty -m "$3"
}

# overview <dir> <line> — write docs/ARCHITECTURE.md with the given revision line.
overview() {
  fixture "$1"
  mkdir -p "$1/docs"
  printf '# Architecture\n\n> %s\n' "$2" > "$1/docs/ARCHITECTURE.md"
}

# code <dir> <path> <date> — add one line to a file outside docs/ and commit it on the date.
code() {
  fixture "$1"
  mkdir -p "$(dirname "$1/$2")"
  echo "change" >> "$1/$2"
  commit "$1" "$3" "change $2"
}

# repo <name> [line] — a repository with the overview committed, printed as its path; the line
# defaults to no revision yet, due after 20 changes.
repo() {
  local dir="$work/$1" line="${2:-**Last design revision:** none yet, due after 20 changes.}"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  overview "$dir" "$line"
  commit "$dir" "2026-01-05" "overview"
  echo "$dir"
}

# expect <exit code> <name> <dir> <needle> — run the sensor; its output must contain the needle.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4"
  local out rc
  fixture "$dir"
  out="$(bash "$SENSOR" "$dir" 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

d="$(repo none-yet)"
code "$d" src/a.py 2026-01-10
code "$d" src/b.py 2026-01-11
expect 0 "no revision yet counts from the first commit" "$d" "2 of 20 changes outside docs/ since the first commit — not yet due"

d="$(repo since-date)"
code "$d" src/a.py 2026-01-10
code "$d" src/b.py 2026-01-11
overview "$d" "**Last design revision:** 2026-02-01, due after 20 changes."
commit "$d" "2026-02-01" "revision"
code "$d" src/c.py 2026-03-01
expect 0 "only the changes after the date count" "$d" "1 of 20 changes outside docs/ since 2026-02-01"

d="$(repo docs-only)"
mkdir -p "$d/docs"; echo "x" > "$d/docs/GLOSSARY.md"
commit "$d" "2026-01-10" "a docs change"
expect 0 "a commit touching only docs/ is not a change" "$d" "0 of 20 changes"

d="$(repo merge-excluded)"
code "$d" src/a.py 2026-01-10
git -C "${d:?}" switch -q -c topic
code "$d" src/b.py 2026-01-11
git -C "${d:?}" switch -q main
GIT_AUTHOR_DATE="2026-01-12T12:00:00" GIT_COMMITTER_DATE="2026-01-12T12:00:00" \
  git -C "${d:?}" -c user.name=fixture -c user.email=fixture@example.invalid merge -q --no-ff -m "merge topic" topic
expect 0 "a merge commit is not a change" "$d" "2 of 20 changes"

d="$(repo due)"
code "$d" src/a.py 2026-01-10
code "$d" src/b.py 2026-01-11
code "$d" scripts/c.sh 2026-01-12
overview "$d" "**Last design revision:** none yet, due after 3 changes."
commit "$d" "2026-01-13" "lower the number"
expect 0 "the number reached marks the revision due" "$d" "3 changes outside docs/ since the first commit, due after 3 — DUE"
expect 0 "the due message lists the directories touched" "$d" "  - src/ (2 changes)"

# The count starts at the commit that set the date, not at a moment the clock derives from it:
# a change committed just after the revision, on the same day, counts whenever the sensor runs.
d="$(repo same-day)"
code "$d" src/a.py 2026-01-10
overview "$d" "**Last design revision:** 2026-02-01, due after 20 changes."
commit_at "$d" "2026-02-01T00:00:10" "revision"
mkdir -p "$d/src"; echo "x" >> "$d/src/b.py"
commit_at "$d" "2026-02-01T00:00:20" "change on the day of the revision"
expect 0 "a change on the day of the revision counts" "$d" "1 of 20 changes outside docs/ since 2026-02-01"

d="$(repo number-only)"
overview "$d" "**Last design revision:** 2026-02-01, due after 20 changes."
commit "$d" "2026-02-01" "revision"
code "$d" src/a.py 2026-02-02
overview "$d" "**Last design revision:** 2026-02-01, due after 30 changes."
commit "$d" "2026-02-03" "raise the number"
code "$d" src/b.py 2026-02-04
expect 0 "a change of the number alone does not reset the count" "$d" "2 of 30 changes"

d="$(repo uncommitted)"
code "$d" src/a.py 2026-01-10
overview "$d" "**Last design revision:** 2099-01-01, due after 20 changes."
expect 0 "a date not yet committed counts nothing" "$d" "0 of 20 changes outside docs/ since 2099-01-01"

d="$(repo per-change)"
mkdir -p "$d/src"; echo x > "$d/src/a.py"; echo x > "$d/src/b.py"
commit "$d" "2026-01-10" "two files, one change"
mkdir -p "$d/old"; echo x > "$d/old/c.py"
commit "$d" "2026-01-11" "a directory"
git -C "${d:?}" rm -rq old
commit "$d" "2026-01-12" "the directory goes"
overview "$d" "**Last design revision:** none yet, due after 2 changes."
commit "$d" "2026-01-13" "lower the number"
expect 0 "a directory counts changes, not files" "$d" "  - src/ (1 change)"
out="$(bash "$SENSOR" "$d" 2>&1)"
if [[ "$out" != *"old/"* ]]; then passed=$((passed + 1)); else
  failed=$((failed + 1)); echo "FAIL: a path that no longer exists is listed" >&2; fi

d="$(repo line-missing)"
overview "$d" "Nothing about revisions here."
commit "$d" "2026-01-06" "drop the line"
expect 1 "no revision line" "$d" "has no '**Last design revision:**' line"

d="$(repo line-unreadable)"
overview "$d" "**Last design revision:** soon, due after some changes."
commit "$d" "2026-01-06" "garble the line"
expect 1 "a line that names no date and no number" "$d" "is unreadable"

d="$(repo number-zero)"
overview "$d" "**Last design revision:** none yet, due after 0 changes."
commit "$d" "2026-01-06" "zero"
expect 1 "a number of zero is unreadable" "$d" "is unreadable"

d="$(repo overview-missing)"
git -C "${d:?}" rm -q docs/ARCHITECTURE.md
commit "$d" "2026-01-06" "drop the overview"
expect 1 "no overview at all" "$d" "ARCHITECTURE.md not found"

# --- summary ---------------------------------------------------------------------------------

summary "Design revision sensor self-test"
