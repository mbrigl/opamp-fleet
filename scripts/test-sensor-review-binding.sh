#!/usr/bin/env bash
#
# Self-test of scripts/sensor-review-binding.sh.
#
# Each case builds a throwaway git repository under a temporary directory with a main tagged
# 'base' and a feat/ branch at it, merges one or more branches into that with --no-ff and a chosen
# message, runs the sensor against the base, and asserts the exit code and the line a reader would
# meet. The cases cover a review at the tip, commits after the review, the three ways the line
# binds nothing, the feat/ branch taking up main, the defaults, a rework and the hand-off of the
# whole branch that covers it, commits outside any merge, alone and among merges, listed unless
# the hand-off covers them, and a base that does not resolve. The cases after them commit ADRs
# and code together and apart: the text of an ADR with other paths is listed, its Status, Date,
# and Deciders lines and the index are not, inside a merged branch too.
#
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

# expect <exit code> <name> <dir> <needle> [base] — run the sensor in the repository against the
# given base (default 'base'); the output must contain the needle.
expect() {
  local want="$1" name="$2" dir="$3" needle="$4" base="${5-base}"
  local out rc
  fixture "$dir"
  out="$(cd "$dir" && bash "$SENSOR" "$base" 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# reject <name> <dir> <needle> [base] — run the sensor against the given base (default 'base');
# the output must not contain the needle.
reject() {
  local name="$1" dir="$2" needle="$3" base="${4-base}" out
  fixture "$dir"
  out="$(cd "$dir" && bash "$SENSOR" "$base" 2>&1)"
  if [[ "$out" != *"$needle"* ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (\"$needle\" reported)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

d="$(repo at-tip)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
tip="$(g "$d" rev-parse topic)"
merge "$d" topic "$(printf 'Merge topic\n\n## Review\n\n- Reviewed at: %s\n' "$tip")"
expect 0 "a review at the tip is counted" "$d" "1 merge since"
expect 0 "a review at the tip is reported as such" "$d" "1 reviewed at the tip of its branch"
reject "a review at the tip lists nothing" "$d" "  - "

d="$(repo short-sha)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
tip="$(g "$d" rev-parse --short topic)"
# shellcheck disable=SC2016  # the backticks are literal Markdown, not a command substitution
merge "$d" topic "$(printf 'Merge topic\n\nReviewed at: `%s`\n' "$tip")"
expect 0 "a short commit in backticks binds the review" "$d" "1 reviewed at the tip"

d="$(repo after-review)"
g "$d" switch -q -c topic
change "$d" src/a.py "reviewed work"
reviewed="$(g "$d" rev-parse topic)"
change "$d" src/b.py "fix the findings"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$reviewed")"
expect 0 "commits after the review are counted" "$d" "1 commit after the review at ${reviewed:0:7}, unreviewed:"
expect 0 "commits after the review are listed" "$d" "fix the findings"
expect 0 "the unreviewed diff is summed up" "$d" "src/b.py"
reject "the reviewed commit is not listed" "$d" "reviewed work"
reject "the reviewed diff is not summed up" "$d" "src/a.py"
expect 0 "a merge with commits after the review is not at the tip" "$d" "0 reviewed at the tip"

d="$(repo branch-name)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: topic\n')"
expect 0 "a name that moves with the branch binds nothing" "$d" "reviewed at 'topic', which is no commit of the merged branch"

d="$(repo hex-ref)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
g "$d" tag cafe123
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: cafe123\n')"
expect 0 "a ref whose name looks like a hash binds nothing" "$d" "reviewed at 'cafe123', which is no commit of the merged branch"

d="$(repo two-lines)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at:\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
expect 0 "an empty placeholder before the filled line is passed over" "$d" "1 reviewed at the tip"

d="$(repo no-line)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "Merge topic"
expect 0 "a merge message without the line" "$d" "no 'Reviewed at:' commit in its message"

d="$(repo empty-line)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at:\n')"
expect 0 "the template's line left empty" "$d" "no 'Reviewed at:' commit in its message"

d="$(repo unknown-sha)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: deadbeef\n')"
expect 0 "a commit that does not resolve" "$d" "reviewed at 'deadbeef', which is no commit of the merged branch"

d="$(repo main-sha)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse base)")"
expect 0 "a commit the feat/ branch already had before the merged one" "$d" "which is no commit of the merged branch"

d="$(repo other-branch)"
g "$d" switch -q -c other
change "$d" src/o.py "elsewhere"
other="$(g "$d" rev-parse other)"
g "$d" switch -q feat/x
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$other")"
expect 0 "a commit of another branch" "$d" "which is no commit of the merged branch"

d="$(repo two-merges)"
g "$d" switch -q -c one
change "$d" src/a.py "one"
merge "$d" one "$(printf 'Merge one\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse one)")"
g "$d" switch -q -c two
change "$d" src/b.py "two"
merge "$d" two "Merge two"
expect 0 "every merge since the base is read" "$d" "2 merges since"
expect 0 "the merge without the line is the one listed" "$d" "Merge two: no 'Reviewed at:'"
reject "the merge reviewed at its tip is not listed" "$d" "Merge one:"

d="$(repo branch-merges)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
g "$d" switch -q -c inner
change "$d" src/b.py "inner work"
g "$d" switch -q topic
g "$d" merge -q --no-ff -m "Merge inner" inner
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
expect 0 "a merge inside the branch is not a hand-off" "$d" "1 merge since"

# Without a head or base, the branch checked out is read from its merge base with main.
d="$(repo defaults)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
out="$(cd "$d" && bash "$SENSOR" 2>&1)"
if [[ "$out" == *"1 merge since $(g "$d" rev-parse --short base)"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: without arguments, the branch checked out is read from its merge base with main" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
fi

# The feat/ branch takes up what main gained: that merge lands no branch and is passed over.
d="$(repo take-up)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
g "$d" switch -q main
change "$d" src/m.py "on main"
g "$d" switch -q feat/x
g "$d" merge -q --no-ff -m "Merge branch 'main' into feat/x" main
expect 0 "taking up main is passed over" "$d" "1 merge since" "main"
reject "taking up main is no hand-off" "$d" "Merge branch 'main'" "main"

# CI checks out a synthetic merge of the pull request into its base and passes the head.
d="$(repo pull-request)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
pr_head="$(g "$d" rev-parse feat/x)"
g "$d" switch -q --detach base
g "$d" merge -q --no-ff -m "Merge $pr_head into base" "$pr_head"
out="$(cd "$d" && bash "$SENSOR" base "$pr_head" 2>&1)"
if [[ "$out" == *"1 reviewed at the tip"* && "$out" != *"Merge $pr_head"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: the pull request's head is read, not the synthetic merge" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
fi

# hand_off <exit code> <name> <dir> <needle> <absent> <text> — run the sensor against 'base' with
# the text as the hand-off; the output must contain the needle and, unless empty, not the absent.
hand_off() {
  local want="$1" name="$2" dir="$3" needle="$4" absent="$5" out rc
  fixture "$dir"
  printf '%s\n' "$6" > "$work/hand-off.md"
  out="$(cd "$dir" && bash "$SENSOR" --hand-off "$work/hand-off.md" base 2>&1)"; rc=$?
  if [[ $rc -eq "$want" && "$out" == *"$needle"* && ( -z "$absent" || "$out" != *"$absent"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (expected exit $want with \"$needle\", got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# A rework: main gains a commit and the feat/ branch is rebased onto it, merges kept. The line
# in the merge names a commit the rebase replaced.
d="$(repo rework)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
g "$d" switch -q main
change "$d" src/m.py "on main"
g "$d" tag base -f >/dev/null
g "$d" switch -q feat/x
g "$d" rebase -q --rebase-merges main >/dev/null 2>&1
expect 0 "a rebase leaves the merge bound to a commit it replaced" "$d" "which is no commit of the merged branch"
hand_off 0 "a hand-off reviewed at the head covers the merges" "$d" \
  "the hand-off was reviewed at the head" "Merge topic" "$(printf '## Review\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse HEAD)")"

d="$(repo hand-off-after)"
g "$d" switch -q -c topic
change "$d" src/a.py "work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
reviewed="$(g "$d" rev-parse HEAD)"
change "$d" src/b.py "fix the findings"
hand_off 0 "commits after the hand-off's review are listed" "$d" \
  "the hand-off: 1 commit after the review at ${reviewed:0:7}, unreviewed:" "" "- Reviewed at: $reviewed"
hand_off 0 "a hand-off not reviewed at the head leaves the merges read" "$d" \
  "1 merge since" "" "- Reviewed at: $reviewed"

d="$(repo hand-off-empty)"
change "$d" src/a.py "work"
hand_off 0 "a hand-off without the line" "$d" "the hand-off: no 'Reviewed at:' commit" "nothing to read" \
  "$(printf '## Review\n\n- Reviewed at:\n')"
hand_off 0 "a hand-off naming a commit before the branch" "$d" \
  "the hand-off: reviewed at '$(g "$d" rev-parse base)', which is no commit of the branch since the base" "" \
  "- Reviewed at: $(g "$d" rev-parse base)"

hand_off 0 "a hand-off reviewed at the head of a branch without merges claims to cover none" "$d" \
  "the hand-off was reviewed at the head" "covers" "- Reviewed at: $(g "$d" rev-parse HEAD)"

out="$(cd "$d" && bash "$SENSOR" --hand-off "$work/no-such-file" base 2>&1)"; rc=$?
if [[ $rc -eq 2 && "$out" == *"is no file"* ]]; then
  passed=$((passed + 1))
else
  failed=$((failed + 1))
  echo "FAIL: a hand-off that is no file exits 2 (got $rc)" >&2
fi

d="$(repo no-merge)"
change "$d" src/a.py "work"
expect 0 "a history without merges lists its commit" "$d" "1 commit landed outside a merge, named by no review:"
reject "a history without merges and without ADRs mixes nothing" "$d" "mixing ADR text"

d="$(repo linear)"
change "$d" src/a.py "first linear"
change "$d" src/b.py "second linear"
expect 0 "linear commits are counted" "$d" "2 commits landed outside a merge, named by no review:"
expect 0 "linear commits are listed" "$d" "second linear"
reject "linear commits leave nothing that reads as clean" "$d" "nothing to read"
hand_off 0 "a hand-off reviewed at the head covers the linear commits" "$d" \
  "the hand-off was reviewed at the head" "outside a merge" "- Reviewed at: $(g "$d" rev-parse HEAD)"
hand_off 0 "a hand-off reviewed before the head lists the linear commits after the review" "$d" \
  "the hand-off: 1 commit after the review at $(g "$d" rev-parse --short HEAD~1), unreviewed:"$'\n'"    $(g "$d" rev-parse --short HEAD) second linear" \
  "outside a merge" "- Reviewed at: $(g "$d" rev-parse HEAD~1)"
hand_off 0 "a hand-off bound to nothing leaves the linear commits listed" "$d" \
  "2 commits landed outside a merge" "" "- Reviewed at:"

d="$(repo mixed-history)"
g "$d" switch -q -c topic
change "$d" src/a.py "merged work"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
change "$d" src/b.py "straight onto the branch"
expect 0 "merges and linear commits: the merge is read" "$d" "1 merge since"
expect 0 "merges and linear commits: the linear one is listed" "$d" \
  "1 commit landed outside a merge, named by no review:"$'\n'"    $(g "$d" rev-parse --short HEAD) straight onto the branch"
reject "merges and linear commits: the merged branch's commit is not listed as outside" "$d" \
  "merged work"

# --- ADRs apart from their implementation ----------------------------------------------------

# adr_repo <name> — a repository whose base carries an accepted ADR and the ADR index, with the
# integration branch feat/x checked out, printed as its path.
adr_repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr"
  git -C "${dir:?}" init -q -b main
  printf '# ADR-0001: One\n\n- **Status:** 🟡 proposed\n- **Date:** 2026-01-01\n- **Deciders:** maintainer\n- **Applies to:** everything\n\n## Decision\n\nOne.\n' \
    > "$dir/docs/adr/0001-one.md"
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
echo "More." >> "$d/docs/adr/0001-one.md"
echo "code" > "$d/a.py"
commit_all "$d" "decide and implement"
expect 0 "a commit changing ADR text and code is listed" "$d" "1 commit mixing ADR text with other paths"
expect 0 "the mixed commit names its ADR and its other path" "$d" \
  "decide and implement: changes the text of docs/adr/0001-one.md together with a.py"

d="$(adr_repo adr-new)"
printf '# ADR-0002: Two\n' > "$d/docs/adr/0002-two.md"
echo "code" > "$d/a.py"
commit_all "$d" "propose and implement"
expect 0 "a new ADR in a commit with code is listed" "$d" "changes the text of docs/adr/0002-two.md"

d="$(adr_repo adr-apart)"
echo "More." >> "$d/docs/adr/0001-one.md"
echo "| 0001 | proposed, revised |" > "$d/docs/adr/README.md"
commit_all "$d" "revise the ADR"
echo "code" > "$d/a.py"
commit_all "$d" "implement"
reject "ADRs and their implementation in commits apart are not listed" "$d" "mixing ADR text"

d="$(adr_repo adr-flip)"
swap "$d/docs/adr/0001-one.md" 's/🟡 proposed/🟢 accepted/; s/2026-01-01/2026-02-01/'
echo "| 0001 | accepted |" > "$d/docs/adr/README.md"
echo "code" > "$d/a.py"
commit_all "$d" "implement and accept"
reject "the flip to accepted, its date, and the index row landing with the implementation are not listed" \
  "$d" "mixing ADR text"

d="$(adr_repo adr-applies)"
swap "$d/docs/adr/0001-one.md" 's/everything/src only/'
echo "code" > "$d/a.py"
commit_all "$d" "narrow and implement"
expect 0 "another header line changed with code is text" "$d" "narrow and implement: changes the text"

d="$(adr_repo adr-decider)"
swap "$d/docs/adr/0001-one.md" 's/maintainer/maintainer, project/'
echo "code" > "$d/a.py"
commit_all "$d" "accept as our own"
reject "a decider added with other paths is not listed" "$d" "mixing ADR text"

d="$(adr_repo adr-many)"
echo "More." >> "$d/docs/adr/0001-one.md"
for f in a b c d e; do echo "code" > "$d/$f.py"; done
commit_all "$d" "a wide change"
expect 0 "a mixed commit names three other paths and counts the rest" "$d" "c.py and 2 more"

d="$(adr_repo adr-merged)"
g "$d" switch -q -c topic
echo "More." >> "$d/docs/adr/0001-one.md"
echo "code" > "$d/a.py"
commit_all "$d" "mixed on the topic"
merge "$d" topic "$(printf 'Merge topic\n\n- Reviewed at: %s\n' "$(g "$d" rev-parse topic)")"
expect 0 "a mixed commit of a merged branch is listed" "$d" "mixed on the topic: changes the text"
reject "the merge itself is not listed as mixed" "$d" "Merge topic: changes"
hand_off 0 "a hand-off reviewed at the head still lists the mixed commit" "$d" \
  "mixed on the topic: changes the text" "" "- Reviewed at: $(g "$d" rev-parse HEAD)"

d="$(repo bad-base)"
expect 2 "a base that does not resolve" "$d" "does not resolve to a commit" "no-such-ref"

# --- summary ---------------------------------------------------------------------------------

summary "Review binding sensor self-test"
