#!/usr/bin/env bash
#
# Self-test of scripts/number-adrs.sh. Verifies: ADR-0002
#
# Each case builds a throwaway repository under a temporary directory: a main with one numbered
# ADR, a branch that adds drafts and cites them from the record, a script, and a document, and a
# main that gains a number of its own meanwhile. The script runs on the branch, and the case
# asserts the numbers it gave, the references it moved, the paths each of its two commits
# touched, the map in the second commit's body, and that the review binding sensor lists neither
# commit as one that changes ADR text together with other paths. The refusals have a case each.
#
# Draft identifiers and ADR numbers inside fixtures are assembled, never written literally, so
# that scripts/check-docs.sh does not read this script's fixtures as references into this
# repository.
#
# Inputs: scripts/number-adrs.sh scripts/lib/ scripts/sensor-review-binding.sh .gitattributes .githooks/merge-adr-index
#
# Usage:
#     scripts/test-number-adrs.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SCRIPT="$ROOT/scripts/number-adrs.sh"
SENSOR="$ROOT/scripts/sensor-review-binding.sh"

D="draft"
# id <number> — the identifier ADR-NNNN; draft <slug> — the draft identifier of a slug.
id() { printf 'ADR-%04d' "$1"; }
draft() { printf 'ADR-%s-%s' "$D" "$1"; }

# g <dir> <git args...> — git in a fixture, whose config carries the identity the script commits
# with too.
g() {
  local dir="$1"; shift
  fixture "$dir"
  git -C "$dir" "$@"
}

# adr <file> <identifier> [header line] — write one ADR.
adr() {
  { echo "# $2: decision"; echo; echo "- **Status:** 🟡 proposed"; [[ -n "${3:-}" ]] && echo "$3"
    echo; echo "## Decision"; echo; echo "We will."; } > "$1"
}

# repo <name> — the fixture: main with ADR 1, the branch 'work' with two drafts whose slugs start
# alike, each added in a commit of its own, the second first, and main gaining ADR 2 meanwhile.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr" "$dir/scripts"
  git init -q -b main "$dir"
  adr "$dir/docs/adr/0001-first.md" "$(id 1)"
  printf '| ADR |\n|-----|\n| [0001](0001-first.md) |\n' > "$dir/docs/adr/README.md"
  cp "$ROOT/.gitattributes" "$dir/.gitattributes"
  git -C "$dir" config user.name t
  git -C "$dir" config user.email t@t
  git -C "$dir" config commit.gpgsign false
  git -C "$dir" config merge.adr-index.driver "$ROOT/.githooks/merge-adr-index %O %A %B %S %X %Y"
  g "$dir" add -A && g "$dir" commit -q -m main

  g "$dir" switch -q -c work
  adr "$dir/docs/adr/$D-alpha-two.md" "$(draft alpha-two)"
  echo "| [$D-alpha-two]($D-alpha-two.md) |" >> "$dir/docs/adr/README.md"
  g "$dir" add -A && g "$dir" commit -q -m "first draft"
  adr "$dir/docs/adr/$D-alpha.md" "$(draft alpha)" "- **Supersedes:** [$(id 1)](0001-first.md)"
  echo "| [$D-alpha]($D-alpha.md) |" >> "$dir/docs/adr/README.md"
  sed "s/🟡 proposed/⚪ superseded by [$(draft alpha)]($D-alpha.md)/" "$dir/docs/adr/0001-first.md" \
    > "$dir/x" && mv "$dir/x" "$dir/docs/adr/0001-first.md"
  printf '#!/usr/bin/env bash\n# Verifies: %s %s\n' "$(draft alpha)" "$(draft alpha-two)" \
    > "$dir/scripts/test-x.sh"
  chmod +x "$dir/scripts/test-x.sh"
  printf 'See [%s](adr/%s-alpha.md) and %s.\n' "$(draft alpha)" "$D" "$(draft alpha-two)" \
    > "$dir/docs/note.md"
  g "$dir" add -A && g "$dir" commit -q -m "second draft and its citations"

  g "$dir" switch -q main
  adr "$dir/docs/adr/0002-second.md" "$(id 2)"
  echo "| [0002](0002-second.md) |" >> "$dir/docs/adr/README.md"
  g "$dir" add -A && g "$dir" commit -q -m "another branch landed"
  g "$dir" switch -q work
  echo "$dir"
}

# --- cases: numbering --------------------------------------------------------------------

# take_up <dir> — merge main into the branch. Both sides append rows at the same end of the
# index, and the repository's .gitattributes, which the fixture carries, merges them with the
# repository's driver, which keeps the rows of both sides.
take_up() {
  g "$1" merge -q --no-edit main > /dev/null
}

d="$(repo numbered)"
take_up "$d"
start="$(g "$d" rev-parse HEAD)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "the script succeeds" test "$rc" -eq 0
check "the draft added first takes the next free number" \
  grep -q "^# $(id 3): " "$d/docs/adr/0003-alpha-two.md"
check "the draft added second takes the one after" \
  grep -q "^# $(id 4): " "$d/docs/adr/0004-alpha.md"
check "no draft file is left" \
  test -z "$(find "$d/docs/adr" -name "$D-*")"
check "the index rows carry the numbers" \
  grep -q "^| \[0004\](0004-alpha.md) |$" "$d/docs/adr/README.md"
rows="$(grep '^| \[' "$d/docs/adr/README.md")"
check "the index rows run in the order of their numbers, main's row taken up included" \
  test "$rows" = "$(printf '| [%s](%s) |\n' 0001 0001-first.md 0002 0002-second.md \
    0003 0003-alpha-two.md 0004 0004-alpha.md)"
check "the superseded ADR names its successor by number" \
  grep -q "superseded by \[$(id 4)\](0004-alpha.md)" "$d/docs/adr/0001-first.md"
check "the markers move, and a slug that starts another's stays apart" \
  grep -q "^# Verifies: $(id 4) $(id 3)$" "$d/scripts/test-x.sh"
check "the document's link and reference move" \
  grep -qF "See [$(id 4)](adr/0004-alpha.md) and $(id 3)." "$d/docs/note.md"
check "a rewritten script keeps its mode" test -x "$d/scripts/test-x.sh"
check "nothing in the tree cites a draft" \
  test -z "$(grep -rl "ADR-$D-\|$D-alpha" "$d/docs" "$d/scripts")"
check "two commits" test "$(g "$d" rev-list --count "$start..HEAD")" -eq 2
first="$(g "$d" diff-tree -r --no-commit-id --name-only HEAD~1)"
second="$(g "$d" diff-tree -r --no-commit-id --name-only HEAD)"
check "the first commit changes only docs/adr/" \
  test -z "$(grep -v '^docs/adr/' <<< "$first")"
check "the second commit changes nothing under docs/adr/" \
  test -z "$(grep '^docs/adr/' <<< "$second")"
check "the second commit's body carries the map" \
  grep -qF "$(draft alpha) → $(id 4)" <<< "$(g "$d" log -1 --format=%b)"
sensor="$(bash "$SENSOR" --root "$d" "$start" HEAD 2>&1)"
check "the review binding sensor lists neither commit as mixed" \
  test -z "$(grep 'changes the text of' <<< "$sensor")"
# The control: the same sensor lists a commit that does mix them, on this fixture.
numbered="$(g "$d" rev-parse HEAD)"
echo "More." >> "$d/docs/adr/0004-alpha.md"
echo "# more" >> "$d/scripts/test-x.sh"
g "$d" commit -q -am mixed
sensor="$(bash "$SENSOR" --root "$d" "$numbered" HEAD 2>&1)"
check "the sensor lists a commit that changes ADR text and a script together" \
  grep -q 'changes the text of' <<< "$sensor"

# The draft added first is renamed to a slug its name sorts after: it keeps its place.
d="$(repo renamed)"
g "$d" mv "docs/adr/$D-alpha-two.md" "docs/adr/$D-zeta.md"
for f in docs/adr/$D-zeta.md docs/adr/README.md scripts/test-x.sh docs/note.md; do
  sed "s/$D-alpha-two/$D-zeta/g" "$d/$f" > "$d/x" && cat "$d/x" > "$d/$f"
done
rm "$d/x"
g "$d" commit -q -am "rename the first draft"
take_up "$d"
bash "$SCRIPT" --root "$d" > /dev/null 2>&1; rc=$?
check "a renamed draft is numbered" test "$rc" -eq 0
check "a draft renamed on the branch keeps the place it was added in" \
  grep -q "^# $(id 3): " "$d/docs/adr/0003-zeta.md"

# --- cases: refusals and nothing to do ----------------------------------------------------

d="$(repo on-main)"
g "$d" switch -q main
adr "$d/docs/adr/$D-gamma.md" "$(draft gamma)"
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "numbering on main is refused" test "$rc" -eq 1
check "and says why" grep -q "the checkout is on main" <<< "$out"

d="$(repo behind-main)"
bash "$SCRIPT" --root "$d" > /dev/null 2>&1; rc=$?
check "a branch that has not taken up main is refused" test "$rc" -eq 1
check "and nothing was committed" test -n "$(find "$d/docs/adr" -name "$D-alpha.md")"

d="$(repo dirty)"
take_up "$d"
echo "change" >> "$d/docs/note.md"
bash "$SCRIPT" --root "$d" > /dev/null 2>&1; rc=$?
check "a working tree with changes is refused" test "$rc" -eq 1

d="$(repo none)"
g "$d" switch -q main
out="$(bash "$SCRIPT" --root "$d" 2>&1)"; rc=$?
check "a tree without drafts has nothing to number" test "$rc" -eq 0
check "and says so" grep -q "nothing to number" <<< "$out"

summary "Numbering script self-test"
