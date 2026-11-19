#!/usr/bin/env bash
#
# Self-test of the map script of the normalize skill, .agents/skills/normalize/remap.sh.
# Verifies: NUC-decisions-are-proposed-then-accepted
#
# Each case builds a throwaway git repository under a temporary directory, writes a map, runs the
# script in it, and asserts the exit code and the files afterwards. The cases cover an ADR moved to
# a new identifier, a clause moved to another ADR, the one migration of a numbered ADR, a split left
# for the hand, a marker that already names the successor, a refused chain, the --check mode, a
# second run that changes nothing, a swap of two criteria, the record left untouched, a criterion
# outside Markdown and outside a marker line, a reference to an ADR with no successor, an ignored
# file, files without a final newline and a binary one, the release's map applied from the tree, the
# record moved with --record, each invalid map, which changes nothing, and every ADR of a copy of
# this repository moved to a new identifier, after which the other self-tests still pass.
#
# The identifiers in the fixtures are assembled at run time: a literal one would be a reference
# that check-docs.sh and check-traceability.sh hold against this repository.
#
# Inputs: .agents/skills/normalize/
# The move of the real tree runs every other self-test again, the costliest case of the local
# gate. Before a push it runs only when the map script changes; a self-test that took up a
# literal identifier fails it in CI, which runs it on every pull request.
#
# Usage:
#     scripts/test-normalize-remap.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
REMAP="$ROOT/.agents/skills/normalize/remap.sh"

C="SEC"
V="Verifies"
S="§"
# id <word> — the identifier SEC-rule-<word>.
id() { printf '%s-rule-%s' "$C" "$1"; }
# old <n> — the identifier of a numbered ADR, ADR-000n.
old() { printf '%s-%04d' ADR "$1"; }

# repo <name> — a repository with an ADR record of three files, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr" "$dir/src"
  git -C "${dir:?}" init -q -b main
  printf '# %s: a\n\nSee [%s](%s.md).\n' "$(id one)" "$(id two)" "$(id two)" > "$dir/docs/adr/$(id one).md"
  printf '# %s: b\n\nRests on G-1.\n' "$(id two)" > "$dir/docs/adr/$(id two).md"
  printf '# %s: c\n' "$(id three)" > "$dir/docs/adr/$(id three).md"
  printf '# Spec\n\n- **G-1** — one\n- **G-2** — two\n' > "$dir/docs/SPECIFICATION.md"
  echo "$dir"
}

# check <name> <condition> — record one assertion.
check() {
  if eval "$2"; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: $1" >&2
  fi
}

# run <dir> <map lines...> — write the map outside the repository and run the script in it.
run() {
  local dir="$1"
  shift
  printf '%s\n' "$@" > "$work/map-$(basename "$dir")"
  (cd "$dir" && bash "$REMAP" "$work/map-$(basename "$dir")") > "$work/out" 2>&1
}

# rerun <dir> [--check] — run the script again with the map the last run wrote, SIGPIPE ignored.
rerun() {
  (trap '' PIPE; cd "$1" && bash "$REMAP" ${2:+"$2"} "$work/map-$(basename "$1")") > "$work/out" 2>&1
}

# An ADR moved to a new identifier: a citation, a clause, a link and its file, and a marker.
d="$(repo move)"
printf 'Read [%s](docs/adr/%s.md), then %s %s3.\n' "$(id one)" "$(id one)" "$(id one)" "$S" > "$d/README.md"
printf '# %s: %s %s\n' "$V" "$(id one)" "$(id two)" > "$d/src/test.sh"
printf 'Not %s-x, x%s, nor _%s.\n' "$(id one)" "$(id one)" "$(id one)" > "$d/notes.md"
run "$d" "$(id one) $(id four)"; rc=$?
check "a move exits 0" "[[ $rc -eq 0 ]]"
check "a move rewrites the identifier, its link, and a clause" \
  "grep -qF 'Read [$(id four)](docs/adr/$(id four).md), then $(id four) ${S}3.' '$d/README.md'"
check "a move rewrites a marker" "grep -qF '$V: $(id four) $(id two)' '$d/src/test.sh'"
check "a longer identifier or word is left alone, on either side" \
  "grep -qF 'Not $(id one)-x, x$(id one), nor _$(id one).' '$d/notes.md'"
check "a move prints the files it changed" "grep -qx 'README.md' '$work/out' && grep -qx 'src/test.sh' '$work/out'"
check "the record under docs/adr/ is left as it was" \
  "grep -qxF '# $(id one): a' '$d/docs/adr/$(id one).md' && grep -qF '[$(id two)]($(id two).md)' '$d/docs/adr/$(id one).md'"

# A second run with the same map changes nothing, and --check then finds nothing to move.
cp "$d/README.md" "$work/readme.before"
rerun "$d"; rc=$?
check "a second run exits 0" "[[ $rc -eq 0 ]]"
check "a second run changes nothing" "cmp -s '$d/README.md' '$work/readme.before' && [[ ! -s '$work/out' ]]"
rerun "$d" --check; rc=$?
check "--check after the map exits 0" "[[ $rc -eq 0 ]]"

# --check lists what the map would move, changes nothing, and exits 1.
d="$(repo check)"
printf 'See %s.\n' "$(id one)" > "$d/README.md"
printf '%s %s\n' "$(id one)" "$(id four)" > "$work/map-check"
(cd "$d" && bash "$REMAP" --check "$work/map-check") > "$work/out" 2>&1; rc=$?
check "--check exits 1 when something would move" "[[ $rc -eq 1 ]]"
check "--check names the reference" "grep -qF 'README.md:1: $(id one) -> $(id four)' '$work/out'"
check "--check changes nothing" "grep -qxF 'See $(id one).' '$d/README.md'"
printf 'See %s and %s.\n' "$(old 1)" "$(old 3)" > "$d/README.md"
printf '0001-alpha.md %s %s\n0003-gamma.md %s\n' "$(id one)" "$(id two)" "$(id four)" > "$work/map-check"
(cd "$d" && bash "$REMAP" --check "$work/map-check") > "$work/out" 2>&1; rc=$?
check "--check lists what it would leave beside what it would move" \
  "[[ $rc -eq 1 ]] && grep -qF 'README.md:1: $(old 3) -> $(id four)' '$work/out' && grep -qF 'README.md:1: $(old 1) is split into' '$work/out'"

# A clause moved to another ADR: the citation and the file of its link move together, and the
# other clauses of the old ADR stay.
d="$(repo clause)"
printf '[%s %s3](docs/adr/%s.md) and %s %s4.\n' "$(id one)" "$S" "$(id one)" "$(id one)" "$S" > "$d/README.md"
run "$d" "$(id one)${S}3 $(id five)${S}1"; rc=$?
check "a clause map exits 0" "[[ $rc -eq 0 ]]"
check "a clause moves with the file of its link, and another clause stays" \
  "grep -qF '[$(id five) ${S}1](docs/adr/$(id five).md) and $(id one) ${S}4.' '$d/README.md'"

# The one migration of a numbered ADR to an identifier: its identifier, its file name, and an index
# row move together.
d="$(repo numbered)"
printf 'See [%s](docs/adr/0002-beta.md) and %s.\n| [0002](0002-beta.md) |\n' "$(old 2)" "$(old 2)" > "$d/README.md"
run "$d" "0002-beta.md $(id six)"; rc=$?
check "a numbered ADR mapped to an identifier exits 0" "[[ $rc -eq 0 ]]"
check "a numbered ADR moves with its file and its index row" \
  "grep -qF 'See [$(id six)](docs/adr/$(id six).md) and $(id six).' '$d/README.md' && grep -qxF '| [$(id six)]($(id six).md) |' '$d/README.md'"

# With --record, the one migration moves the record too: a numbered ADR's file, heading, status,
# links, and index row, and a split it cites is listed. A NUC- file stays as it was.
d="$(repo record)"
base="$(printf '%s-base-rule' NUC)"
printf '# %s: a\n\nSee [%s](0008-beta.md) and %s.\n' "$(old 7)" "$(old 8)" "$(old 1)" > "$d/docs/adr/0007-alpha.md"
printf '# %s: b\n\n- **Status:** superseded by %s\n' "$(old 8)" "$(old 7)" > "$d/docs/adr/0008-beta.md"
printf '| [0007](0007-alpha.md) |\n' > "$d/docs/adr/README.md"; echo x > "$d/docs/adr/0006-gamma.md"
printf '# %s: c\n\nNot %s.\n' "$base" "$(old 7)" > "$d/docs/adr/$base.md"; cp "$d/docs/adr/$base.md" "$d/AGENTS.md"
git -C "$d" add -A
printf '0001-x.md %s %s\n0008-beta.md %s\n0007-alpha.md %s\n0006-gamma.md %s\n' "$(id one)" "$(id two)" "$(id eight)" "$(id seven)" "$(id six)" > "$work/map-record"
(cd "$d" && bash "$REMAP" --record --check "$work/map-record") > "$work/out" 2>&1; rc=$?
check "--record --check exits 1, names the renames sorted by the old file name, and changes nothing" \
  "[[ $rc -eq 1 && \"\$(grep -o '^docs/adr/000[678][^ ]* -> docs/adr/[^.]*' '$work/out' | tr '\n' ' ')\" == 'docs/adr/0006-gamma.md -> docs/adr/$(id six) docs/adr/0007-alpha.md -> docs/adr/$(id seven) docs/adr/0008-beta.md -> docs/adr/$(id eight) ' && -f '$d/docs/adr/0007-alpha.md' ]]"
(cd "$d" && bash "$REMAP" --record "$work/map-record") > "$work/out" 2>&1; rc=$?
check "--record exits 2 on a split the record cites, and lists it" \
  "[[ $rc -eq 2 ]] && grep -qF '0007-alpha.md:3: $(old 1) is split into' '$work/out'"
check "--record renames a numbered ADR and moves its heading and links" \
  "[[ ! -e '$d/docs/adr/0007-alpha.md' ]] && grep -qxF '# $(id seven): a' '$d/docs/adr/$(id seven).md' && grep -qF 'See [$(id eight)]($(id eight).md) and $(old 1).' '$d/docs/adr/$(id seven).md'"
check "--record moves a status line and an index row" \
  "grep -qF 'superseded by $(id seven)' '$d/docs/adr/$(id eight).md' && grep -qxF '| [$(id seven)]($(id seven).md) |' '$d/docs/adr/README.md'"
check "--record leaves a NUC- file and AGENTS.md as they were" "grep -qxF 'Not $(old 7).' '$d/docs/adr/$base.md' && grep -qxF 'Not $(old 7).' '$d/AGENTS.md'"
cp "$d/docs/adr/$(id seven).md" "$work/seven.before"
(cd "$d" && bash "$REMAP" --record "$work/map-record") > "$work/out" 2>&1
check "a second run with --record changes nothing" "cmp -s '$d/docs/adr/$(id seven).md' '$work/seven.before'"
# A rename onto an existing file, a NUC- one among them, or two renames onto one file are refused.
printf 'c\n' > "$d/docs/adr/0009-c.md"; printf 'd\n' > "$d/docs/adr/0010-d.md"
# Filler longer than a pipe holds keeps the map's reader writing when the refusal exits.
pad="$(printf '%9000s' '' | tr ' ' a)"
for m in "0009-c.md $base" "0009-c.md $(id nine)|0010-d.md $(id nine)"; do
  { tr '|' '\n' <<< "$m"; for i in 1 2 3 4 5 6 7 8; do echo "$(id f)$pad$i $(id g)$pad$i"; done; } > "$work/map-record"
  (cd "$d" && bash "$REMAP" --record --check "$work/map-record") > "$work/out" 2>&1; rc=$?
  (trap '' PIPE; cd "$d" && bash "$REMAP" --record "$work/map-record") >> "$work/out" 2>&1; rc=$rc$?
  check "--record refuses a rename onto a taken name: $m" "[[ $rc == 11 ]] && ! grep -q 'Broken pipe' '$work/out' && grep -qxF c '$d/docs/adr/0009-c.md' && grep -qxF d '$d/docs/adr/0010-d.md' && grep -qF 'Not $(old 7).' '$d/docs/adr/$base.md'"
done

# A split: references are left for the hand and listed, but a marker that already names one of
# the successors drops the old identifier.
d="$(repo split)"
printf 'See %s.\n' "$(old 1)" > "$d/README.md"
printf '# %s: %s %s\n# %s: %s, %s\n' "$V" "$(old 1)" "$(id one)" "$V" "$(id two)" "$(old 1)" > "$d/src/test.sh"
run "$d" "0001-alpha.md $(id one) $(id two)"; rc=$?
check "a split exits 2" "[[ $rc -eq 2 ]]"
check "a split lists the reference with its successors" \
  "grep -qF 'README.md:1: $(old 1) is split into $(id one) $(id two)' '$work/out'"
check "a split leaves the reference" "grep -qxF 'See $(old 1).' '$d/README.md'"
check "a marker that names a successor drops the old identifier" \
  "grep -qxF '# $V: $(id one)' '$d/src/test.sh' && grep -qxF '# $V: $(id two)' '$d/src/test.sh'"

# A marker that already names the successor of a move drops the old identifier too.
d="$(repo marker)"
printf '# %s: %s %s\n' "$V" "$(id four)" "$(old 3)" > "$d/src/test.sh"
run "$d" "0003-gamma.md $(id four)"; rc=$?
check "a marker that already names the successor keeps it once" \
  "[[ $rc -eq 0 ]] && grep -qxF '# $V: $(id four)' '$d/src/test.sh'"
printf '# %s: %s, %s\n' "$V" "$(old 3)" "$(id four)" > "$d/src/test.sh"
run "$d" "0003-gamma.md $(id four)"; rc=$?
check "an old identifier dropped first in a marker takes the separator after it" \
  "[[ $rc -eq 0 ]] && grep -qxF '# $V: $(id four)' '$d/src/test.sh'"

# A chain or a cycle among ADR identifiers is refused: an identifier is never used twice, and a
# second run of such a map would move again.
d="$(repo chain)"
printf '%s\n' "$(id one)" > "$d/README.md"
run "$d" "$(id one) $(id two)" "$(id two) $(id one)"; rc=$?
check "a cycle of ADR identifiers exits 1" "[[ $rc -eq 1 ]] && grep -qF 'never used twice' '$work/out'"
run "$d" "$(id one) $(id two)" "$(id two) $(id three)"; rc=$?
check "a chain of ADR identifiers exits 1" "[[ $rc -eq 1 ]]"
run "$d" "$(id one) $(id two)" "$(id two)${S}2 $(id three)${S}1"; rc=$?
check "a successor whose clause is mapped exits 1" "[[ $rc -eq 1 ]] && grep -qF 'never used twice' '$work/out'"
run "$d" "$(id one)${S}3 $(id two)${S}1" "$(id two) $(id three)"; rc=$?
check "a clause whose successor's identifier is mapped exits 1" "[[ $rc -eq 1 ]] && grep -qF 'never used twice' '$work/out'"
check "a refused map changes nothing" "grep -qxF '$(id one)' '$d/README.md'"

# A swap of two criteria: in Markdown, on a marker line, and in an ADR; never in the specification
# and never on another line of a file that is not Markdown. A criteria swap is one pass.
d="$(repo criteria)"
printf 'G-1 before G-2, not XG-1.\n' > "$d/README.md"
printf '# %s: G-1 G-2\nlabel = "G-1"\n' "$V" > "$d/src/test.sh"
run "$d" "G-1 G-2" "G-2 G-1"; rc=$?
check "a criteria swap exits 0" "[[ $rc -eq 0 ]]"
check "a criteria swap moves them in Markdown, and leaves a longer word alone" \
  "grep -qF 'G-2 before G-1, not XG-1.' '$d/README.md'"
check "a criteria swap moves a marker's identifiers" "grep -qF '$V: G-2 G-1' '$d/src/test.sh'"
check "a criterion on a line without a marker stays outside Markdown" \
  "grep -qF 'label = \"G-1\"' '$d/src/test.sh'"
check "a criterion cited in an ADR moves" "grep -qF 'Rests on G-2.' '$d/docs/adr/$(id two).md'"
check "the specification is left as it was" "grep -qF -- '- **G-1** — one' '$d/docs/SPECIFICATION.md'"

# An ADR with no successor: its references stay and are listed, and the rest still moves.
d="$(repo noheir)"
printf '%s, %s.\n' "$(id one)" "$(id three)" > "$d/README.md"
printf 'See [%s](%s.md).\n' "$(id three)" "$(id three)" > "$d/index.md"
run "$d" "$(id one) $(id four)" "$(id three) -"; rc=$?
check "a reference to an ADR with no successor exits 2" "[[ $rc -eq 2 ]]"
check "a reference to an ADR with no successor is listed" \
  "grep -qF 'README.md:1: $(id three) has no successor' '$work/out'"
check "the other references still move" "grep -qF '$(id four), $(id three).' '$d/README.md'"
check "a link to an ADR with no successor is listed" \
  "grep -qF 'index.md:1: $(id three) has no successor' '$work/out'"

# A file git ignores is left alone.
d="$(repo ignored)"
printf 'notes.md\n' > "$d/.gitignore"
printf '%s\n' "$(id one)" > "$d/notes.md"
run "$d" "$(id one) $(id four)"; rc=$?
check "an ignored file is left alone" "[[ $rc -eq 0 ]] && grep -qxF '$(id one)' '$d/notes.md'"

# The release's map, applied from where the tree ships it: it is valid, a whole ADR moves, a split
# is left for the hand, and the map file itself stays as it was.
d="$(repo shipped)"
shipped=".agents/skills/normalize/map-v0.9.0.txt"
mkdir -p "$d/${shipped%/*}"
cp "$ROOT/$shipped" "$d/$shipped"
to="$(awk '$1 ~ /^0003-/ { print $2 }' "$d/$shipped")"
printf 'See %s and %s.\n' "$(old 3)" "$(old 1)" > "$d/README.md"
(cd "$d" && bash "$REMAP" "$shipped") > "$work/out" 2>&1; rc=$?
check "the release's map exits 2 on a split" "[[ $rc -eq 2 ]]"
check "the release's map moves a whole ADR and leaves a split" \
  "[[ -n '$to' ]] && grep -qxF 'See $to and $(old 1).' '$d/README.md'"
check "the release's map leaves its own file as it was" "cmp -s '$d/$shipped' '$ROOT/$shipped'"

# A file the map does not change keeps its bytes, a binary one and one without a final newline
# among them, and a file it changes keeps a missing final newline.
d="$(repo bytes)"
printf '\001\002\377' > "$d/seed.bin"
printf 'no final newline' > "$d/drawing.txt"
printf 'see %s' "$(id one)" > "$d/open.md"
cp "$d/seed.bin" "$work/seed.bin"
cp "$d/drawing.txt" "$work/drawing.txt"
run "$d" "$(id one) $(id four)"; rc=$?
check "a map over files without a final newline exits 0" "[[ $rc -eq 0 ]]"
check "a binary file the map does not change keeps its bytes" "cmp -s '$d/seed.bin' '$work/seed.bin'"
check "a text file the map does not change keeps its bytes" \
  "cmp -s '$d/drawing.txt' '$work/drawing.txt'"
check "only the file the map changes is printed" "[[ \"\$(cat '$work/out')\" == open.md ]]"
check "a changed file keeps a missing final newline" \
  "[[ \"\$(od -An -c '$d/open.md' | tr -d ' \n')\" == \"see\$(id four)\" ]]"

# An invalid map changes nothing.
d="$(repo invalid)"
printf '%s\n' "$(id one)" > "$d/README.md"
run "$d" "$(id one) $(id four)" "G-1 Q-1"; rc=$?
check "a criterion mapped across kinds exits 1" "[[ $rc -eq 1 ]]"
check "an invalid map changes nothing" "grep -qxF '$(id one)' '$d/README.md'"
run "$d" "$(id one) $(id four)" "$(id one) $(id five)"; rc=$?
check "an ADR mapped twice exits 1" "[[ $rc -eq 1 ]] && grep -qF 'mapped twice' '$work/out'"
run "$d" "$(id one)"; rc=$?
check "a line without NEW exits 1" "[[ $rc -eq 1 ]]"
run "$d" "# only a comment"; rc=$?
check "an empty map exits 1" "[[ $rc -eq 1 ]] && grep -qF 'the map is empty' '$work/out'"
run "$d" "$(id one)${S}3 $(id two)${S}1 $(id three)${S}1"; rc=$?
check "a clause mapped to two exits 1" "[[ $rc -eq 1 ]] && grep -qF 'only a clause maps to, and alone' '$work/out'"
run "$d" "$(id one)${S}3 $(id two) $(id three)"; rc=$?
check "a clause mapped to two identifiers exits 1" "[[ $rc -eq 1 ]] && grep -qF 'a clause maps to one clause' '$work/out'"
run "$d" "$(id one) $(id two)${S}1"; rc=$?
check "an identifier mapped to a clause exits 1" "[[ $rc -eq 1 ]] && grep -qF 'only a clause maps to' '$work/out'"
run "$d" "0001-alpha.md $(id four)" "0001-other.md $(id five)"; rc=$?
check "a number mapped twice exits 1" "[[ $rc -eq 1 ]] && grep -qF 'its number is mapped twice' '$work/out'"

# Outside a repository the script refuses.
mkdir -p "$work/bare"
printf '%s %s\n' "$(id one)" "$(id four)" > "$work/map-bare"
(cd "$work/bare" && bash "$REMAP" "$work/map-bare") > /dev/null 2>&1; rc=$?
check "outside a repository the script exits 1" "[[ $rc -eq 1 ]]"

# The real tree: every ADR moved to a new identifier, its file renamed in the copy, leaves every
# other self-test passing. So no fixture carries a literal identifier whose move by a re-cut would
# break it. The template setup self-test reads the record itself, which the map script leaves
# alone, so it is not run.
copy="$work/tree"
mkdir -p "$copy"
(cd "$ROOT" && git ls-files --cached --others --exclude-standard | while IFS= read -r f; do
  [[ -e "$f" || -L "$f" ]] && printf '%s\0' "$f"
done | tar cf - --null -T -) | tar xf - -C "$copy"
git -C "$copy" init -q && git -C "$copy" add -A
# moved <identifier> — a new identifier for it: its slug with 'moved' as a further word, or in
# place of its last word where it already has five.
moved() {
  local slug="${1#*-}"
  [[ "$slug" =~ ^([^-]+-){4}[^-]+$ ]] && slug="${slug%-*}"
  printf '%s-%s-moved' "${1%%-*}" "$slug"
}
: > "$work/map-tree"
for f in "$copy"/docs/adr/[A-Z]*-*.md; do
  n="$(basename "$f" .md)"
  printf '%s %s\n' "$n" "$(moved "$n")" >> "$work/map-tree"
  mv "$f" "$copy/docs/adr/$(moved "$n").md"
done
mapfile -t goals < <(grep -oE '^[-0-9. ]*\*\*G-[0-9]+\*\*' "$copy/docs/SPECIFICATION.md" | grep -oE 'G-[0-9]+')
mapfile -t quals < <(grep -oE '^[-0-9. ]*\*\*Q-[0-9]+\*\*' "$copy/docs/SPECIFICATION.md" | grep -oE 'Q-[0-9]+')
# cycle <ids...> — one map line per criterion, each to the next and the last to the first.
cycle() {
  local i
  for ((i = 0; i < $#; i++)); do
    printf '%s %s\n' "${@:i+1:1}" "${@:(i + 1) % $# + 1:1}"
  done
}
{ cycle "${goals[@]}"; cycle "${quals[@]}"; } >> "$work/map-tree"
(cd "$copy" && bash .agents/skills/normalize/remap.sh "$work/map-tree") > "$work/out" 2>&1; rc=$?
check "a move of the real tree's ADRs and a cycle of its criteria exit 0" "[[ $rc -eq 0 ]]"
((rc == 0)) || sed 's/^/    /' "$work/out" >&2
pids=()
names=()
for t in "$copy"/scripts/test-*.sh; do
  case "$t" in */test-template-setup.sh | */test-normalize-remap.sh) continue ;; esac
  name="$(basename "$t")"
  names+=("$name")
  bash "$t" > "$work/$name.log" 2>&1 &
  pids+=("$!")
done
for i in "${!pids[@]}"; do
  wait "${pids[$i]}"; rc=$?
  check "${names[$i]} passes after a cycle of the real tree" "[[ $rc -eq 0 ]]"
  ((rc == 0)) || sed 's/^/    /' "$work/${names[$i]}.log" >&2
done

summary "Normalize map self-test"
