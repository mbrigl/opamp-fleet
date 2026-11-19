#!/usr/bin/env bash
#
# Self-test of scripts/sensor-review-binding.sh.
#
# Each case builds a throwaway git repository under a temporary directory with a main tagged
# 'base' and a feat/ branch at it, commits onto that branch linearly, runs the sensor against the
# base, with or without a hand-off, and asserts the exit code and the line a reader would meet.
# The cases cover a branch without commits, commits bound only by a hand-off when none is given,
# a hand-off reviewed at the head, commits after its review, the ways its line binds nothing,
# the defaults, the synthetic merge of CI, a rework, a merge on the branch and main taken up by
# one, and a base that does not resolve. The cases after them commit ADRs and code together and
# apart: the text of an ADR with other paths is listed, its Status, Date, and Deciders lines and
# the index are not, inside a merged branch too.
#
# Verifies: NUC-checks-are-bash
# Inputs: scripts/sensor-review-binding.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-review-binding.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-review-binding.sh"

# g <dir> <git args...> — git in the fixture with a fixed identity.
g() {
  fixture "$1"
  local dir="$1"
  shift
  git -C "${dir:?}" -c user.name=fixture -c user.email=fixture@example.invalid "$@"
}

# change <dir> <path> <message> — add one line to a file and commit it.
change() {
  fixture "$1"
  mkdir -p "$(dirname "$1/$2")"
  echo "change" >> "$1/$2"
  g "$1" add -A
  g "$1" commit -q -m "$3"
}

# repo <name> — a repository with one commit on main, tagged 'base', and the integration branch
# feat/x at it checked out, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir"
  git -C "${dir:?}" init -q -b main
  change "$dir" README.md "initial"
  g "$dir" tag base
  g "$dir" switch -q -c feat/x
  echo "$dir"
}

# merge <dir> <branch> <message> — merge the branch into feat/x with --no-ff and the message.
merge() {
  g "$1" switch -q feat/x
  g "$1" merge -q --no-ff -m "$3" "$2"
}

# report <name> <dir> <exit code> <needle> <absent> [args...] — run the sensor in the repository
# with the arguments (default 'base'); it must exit with the code, and its output must contain
# the needle and, unless empty, not the absent.
report() {
  local name="$1" dir="$2" want="$3" needle="$4" absent="$5" out rc
  shift 5
  (($#)) || set -- base
  fixture "$dir"
  out="$(bash "$SENSOR" --root "$dir" "$@" 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* && ( -z "$absent" || "$out" != *"$absent"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# expect <exit code> <name> <dir> <needle> — the sensor against 'base' prints the needle.
expect() { report "$2" "$3" "$1" "$4" ""; }

# reject <name> <dir> <needle> — the sensor against 'base' does not print the needle.
reject() { report "$1" "$2" 0 "" "$3"; }

# hand_off <exit code> <name> <dir> <needle> <absent> <text> — run the sensor against 'base' with
# the text as the hand-off; the output must contain the needle and, unless empty, not the absent.
hand_off() {
  printf '%s\n' "$6" > "$work/hand-off.md"
  report "$2" "$3" "$1" "$4" "$5" --hand-off "$work/hand-off.md" base
}

# --- a branch without a hand-off -------------------------------------------------------------

d="$(repo empty)"
expect 0 "a branch without commits has nothing to read" "$d" \
  "no commit since $(g "$d" rev-parse --short base) — nothing to read."

d="$(repo linear)"
change "$d" src/a.py "first linear"
change "$d" src/b.py "second linear"
expect 0 "commits without a hand-off are bound only by one, and the option is named" "$d" \
  "2 commits since $(g "$d" rev-parse --short base), bound only by a hand-off: name the pull request's description with --hand-off <file>."
reject "commits without a hand-off are no finding" "$d" "  - "
reject "commits without a hand-off leave nothing that reads as clean" "$d" "nothing to read"

d="$(repo one)"
change "$d" src/a.py "work"
expect 0 "one commit is counted as one" "$d" "1 commit since"
reject "a history without ADRs mixes nothing" "$d" "mixing ADR text"

# --- the hand-off of the whole branch --------------------------------------------------------

d="$(repo at-head)"
change "$d" src/a.py "first linear"
change "$d" src/b.py "second linear"
hand_off 0 "a hand-off reviewed at the head covers the commits" "$d" \
  "the hand-off was reviewed at the head, $(g "$d" rev-parse --short HEAD), which covers the 2 commits since $(g "$d" rev-parse --short base)." \
  "  - " "$(printf '## Review\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse HEAD)")"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
hand_off 0 "a short commit in backticks binds the review" "$d" \
  "the hand-off was reviewed at the head" "" "$(printf 'Reviewed at: `%s`\n' "$(g "$d" rev-parse --short HEAD)")"
hand_off 0 "an empty placeholder before the filled line is passed over" "$d" \
  "the hand-off was reviewed at the head" "" \
  "$(printf -- '- Reviewed at:\n- Reviewed at: %s\n' "$(g "$d" rev-parse HEAD)")"
hand_off 0 "a hand-off reviewed before the head lists the commits after the review" "$d" \
  "the hand-off: 1 commit after the review at $(g "$d" rev-parse --short HEAD~1), unreviewed:"$'\n'"    $(g "$d" rev-parse --short HEAD) second linear" \
  "first linear" "- Reviewed at: $(g "$d" rev-parse HEAD~1)"
hand_off 0 "the unreviewed diff is summed up" "$d" "src/b.py" "src/a.py" \
  "- Reviewed at: $(g "$d" rev-parse HEAD~1)"
hand_off 0 "the headline of a hand-off before the head counts the finding below" "$d" \
  "2 commits since $(g "$d" rev-parse --short base), bound only by the hand-off; 1 finding below." "" \
  "- Reviewed at: $(g "$d" rev-parse HEAD~1)"
hand_off 0 "a comma after the hash leaves it readable" "$d" \
  "the hand-off: 1 commit after the review at $(g "$d" rev-parse --short HEAD~1)" "" \
  "- Reviewed at: $(g "$d" rev-parse --short HEAD~1), two rounds"

hand_off 0 "a hand-off without the line" "$d" "the hand-off: no 'Reviewed at:' commit" "nothing to read" \
  "$(printf '## Review\n\n- Reviewed at:\n')"
hand_off 0 "a hand-off bound to nothing names no option" "$d" "bound only by the hand-off; 1 finding below." \
  "--hand-off <file>" "- Reviewed at:"
hand_off 0 "a hand-off naming a commit before the branch" "$d" \
  "the hand-off: reviewed at '$(g "$d" rev-parse base)', which is no commit of the branch since the base" "" \
  "- Reviewed at: $(g "$d" rev-parse base)"
hand_off 0 "a hand-off naming a commit that does not resolve" "$d" \
  "reviewed at 'deadbeef', which is no commit of the branch since the base" "" "- Reviewed at: deadbeef"
hand_off 0 "a name that moves with the branch binds nothing" "$d" \
  "reviewed at 'feat/x', which is no commit of the branch since the base" "" "- Reviewed at: feat/x"
g "$d" tag cafe123
hand_off 0 "a ref whose name looks like a hash binds nothing" "$d" \
  "reviewed at 'cafe123', which is no commit of the branch since the base" "" "- Reviewed at: cafe123"
g "$d" switch -q -c other base
change "$d" src/o.py "elsewhere"
other="$(g "$d" rev-parse other)"
g "$d" switch -q feat/x
hand_off 0 "a hand-off naming a commit of another branch" "$d" \
  "which is no commit of the branch since the base" "" "- Reviewed at: $other"

out="$(bash "$SENSOR" --root "$d" --hand-off "$work/no-such-file" base 2>&1)"; rc=$?
if [[ $rc -eq 2 && "$out" == *"is no file"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: a hand-off that is no file exits 2 (got $rc)" >&2
fi

# Without a head or base, the branch checked out is read from its merge base with main.
d="$(repo defaults)"
change "$d" src/a.py "work"
out="$(bash "$SENSOR" --root "$d" 2>&1)"
if [[ "$out" == *"1 commit since $(g "$d" rev-parse --short base)"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: without arguments, the branch checked out is read from its merge base with main" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
fi

# CI checks out a synthetic merge of the pull request into its base and passes the head.
d="$(repo pull-request)"
change "$d" src/a.py "work"
pr_head="$(g "$d" rev-parse feat/x)"
g "$d" switch -q --detach base
g "$d" merge -q --no-ff -m "Merge $pr_head into base" "$pr_head"
printf -- '- Reviewed at: %s\n' "$pr_head" > "$work/hand-off.md"
report "the pull request's head is read, not the synthetic merge" "$d" 0 \
  "the hand-off was reviewed at the head" "Merge $pr_head" --hand-off "$work/hand-off.md" base "$pr_head"

# A rework: main gains a commit and the feat/ branch is rebased onto it. A hand-off reviewed at
# the head before the rework names a commit the rebase replaced.
d="$(repo rework)"
change "$d" src/a.py "work"
before="$(g "$d" rev-parse HEAD)"
g "$d" switch -q main
change "$d" src/m.py "on main"
g "$d" tag base -f >/dev/null
g "$d" switch -q feat/x
g "$d" rebase -q main >/dev/null 2>&1
hand_off 0 "a rebase leaves the hand-off bound to a commit it replaced" "$d" \
  "which is no commit of the branch since the base" "" "- Reviewed at: $before"
hand_off 0 "a hand-off reviewed at the head after the rework covers the branch" "$d" \
  "the hand-off was reviewed at the head" "" "- Reviewed at: $(g "$d" rev-parse HEAD)"

# --- merges on the branch --------------------------------------------------------------------

d="$(repo merged)"
g "$d" switch -q -c topic
change "$d" src/a.py "merged work"
merge "$d" topic "Merge topic"
change "$d" src/b.py "straight onto the branch"
expect 0 "a merge on the branch is a finding" "$d" \
  "1 merge between the base and the head; a feat/ branch carries none (NUC-main-only-via-pr §5), and no review is read from its message:"$'\n'"    $(g "$d" rev-parse --short HEAD~1) Merge topic"
expect 0 "the commits a merge brought count with the others" "$d" "2 commits since"
expect 0 "the headline counts the merge finding" "$d" "bound only by a hand-off: name the pull request's description with --hand-off <file>; 1 finding below."
hand_off 0 "a hand-off reviewed at the head leaves the merge listed" "$d" \
  "Merge topic" "" "- Reviewed at: $(g "$d" rev-parse HEAD)"
hand_off 0 "a hand-off reviewed at the head covers the merged commits too" "$d" \
  "which covers the 2 commits since $(g "$d" rev-parse --short base); 1 finding below." "" \
  "- Reviewed at: $(g "$d" rev-parse HEAD)"

# The feat/ branch takes up what main gained by a merge, instead of a rebase.
d="$(repo take-up)"
change "$d" src/a.py "work"
g "$d" switch -q main
change "$d" src/m.py "on main"
g "$d" switch -q feat/x
g "$d" merge -q --no-ff -m "Merge branch 'main' into feat/x" main
expect 0 "taking up main by a merge is listed" "$d" "Merge branch 'main' into feat/x"


# --- ADRs apart from their implementation ----------------------------------------------------

# adr_repo <name> — a repository whose base carries an accepted ADR and the ADR index, with the
# integration branch feat/x checked out, printed as its path.
adr_repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr"
  git -C "${dir:?}" init -q -b main
  printf '# FIX-rule-one: One\n\n- **Status:** 🟡 proposed\n- **Date:** 2026-01-01\n- **Deciders:** maintainer\n- **Applies to:** everything\n\n## Decision\n\nOne.\n' \
    > "$dir/docs/adr/FIX-rule-one.md"
  echo "| 0001 | proposed |" > "$dir/docs/adr/README.md"
  g "$dir" add -A
  g "$dir" commit -q -m "initial"
  g "$dir" tag base
  g "$dir" switch -q -c feat/x
  echo "$dir"
}

# swap <file> <sed script> — edit the file in place through a temporary file.
swap() {
  sed "$2" "$1" > "$1.tmp" && mv "$1.tmp" "$1"
}

# commit_all <dir> <message> — commit everything the working tree changed.
commit_all() {
  g "$1" add -A
  g "$1" commit -q -m "$2"
}

d="$(adr_repo adr-mixed)"
echo "More." >> "$d/docs/adr/FIX-rule-one.md"
echo "code" > "$d/a.py"
commit_all "$d" "decide and implement"
expect 0 "a commit changing ADR text and code is listed" "$d" "1 commit mixing ADR text with other paths"
expect 0 "the mixed commit names its ADR and its other path" "$d" \
  "decide and implement: changes the text of docs/adr/FIX-rule-one.md together with a.py"

d="$(adr_repo adr-new)"
printf '# FIX-rule-two: Two\n' > "$d/docs/adr/FIX-rule-two.md"
echo "code" > "$d/a.py"
commit_all "$d" "propose and implement"
expect 0 "a new ADR in a commit with code is listed" "$d" "changes the text of docs/adr/FIX-rule-two.md"

d="$(adr_repo adr-apart)"
echo "More." >> "$d/docs/adr/FIX-rule-one.md"
echo "| 0001 | proposed, revised |" > "$d/docs/adr/README.md"
commit_all "$d" "revise the ADR"
echo "code" > "$d/a.py"
commit_all "$d" "implement"
reject "ADRs and their implementation in commits apart are not listed" "$d" "mixing ADR text"

d="$(adr_repo adr-flip)"
swap "$d/docs/adr/FIX-rule-one.md" 's/🟡 proposed/🟢 accepted/; s/2026-01-01/2026-02-01/'
echo "| 0001 | accepted |" > "$d/docs/adr/README.md"
echo "code" > "$d/a.py"
commit_all "$d" "implement and accept"
reject "the flip to accepted, its date, and the index row landing with the implementation are not listed" \
  "$d" "mixing ADR text"

d="$(adr_repo adr-applies)"
swap "$d/docs/adr/FIX-rule-one.md" 's/everything/src only/'
echo "code" > "$d/a.py"
commit_all "$d" "narrow and implement"
expect 0 "another header line changed with code is text" "$d" "narrow and implement: changes the text"

d="$(adr_repo adr-decider)"
swap "$d/docs/adr/FIX-rule-one.md" 's/maintainer/maintainer, project/'
echo "code" > "$d/a.py"
commit_all "$d" "accept as our own"
reject "a decider added with other paths is not listed" "$d" "mixing ADR text"

d="$(adr_repo adr-many)"
echo "More." >> "$d/docs/adr/FIX-rule-one.md"
for f in a b c d e; do echo "code" > "$d/$f.py"; done
commit_all "$d" "a wide change"
expect 0 "a mixed commit names three other paths and counts the rest" "$d" "c.py and 2 more"

d="$(adr_repo adr-merged)"
g "$d" switch -q -c topic
echo "More." >> "$d/docs/adr/FIX-rule-one.md"
echo "code" > "$d/a.py"
commit_all "$d" "mixed on the topic"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
expect 0 "a mixed commit of a merged branch is listed" "$d" "mixed on the topic: changes the text"
reject "the merge itself is not listed as mixed" "$d" "Merge topic: changes"
hand_off 0 "a hand-off reviewed at the head still lists the mixed commit" "$d" \
  "mixed on the topic: changes the text" "" "- Reviewed at: $(g "$d" rev-parse HEAD)"
hand_off 0 "a hand-off reviewed at the head counts the mixed commits in its headline" "$d" \
  "which covers the 1 commit since $(g "$d" rev-parse --short base); 2 findings below." "" \
  "- Reviewed at: $(g "$d" rev-parse HEAD)"

d="$(repo bad-base)"
report "a base that does not resolve" "$d" 2 "does not resolve to a commit" "" no-such-ref

# --- summary ---------------------------------------------------------------------------------

summary "Review binding sensor self-test"
