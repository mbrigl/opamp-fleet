#!/usr/bin/env bash
#
# Self-test of the map script of the normalize skill, .agents/skills/normalize/remap.sh.
# Verifies: ADR-0001
#
# Each case builds a throwaway git repository under a temporary directory, writes a map, runs the
# script in it, and asserts the exit code and the files afterwards. The cases cover a swap of two
# ADRs and of two criteria, a merge of two ADRs into one, the record left untouched, a criterion
# outside Markdown and outside a marker line, a reference to an ADR with no successor, an ignored
# file, an invalid map that changes nothing, and a cycle of every ADR and criterion in a copy of
# this repository after which the other self-tests still pass.
#
# The identifiers in the fixtures are assembled at run time: a literal one would be a reference
# that check-docs.sh and check-traceability.sh hold against this repository.
#
# Inputs: .agents/skills/normalize/
# The cycle of the real tree runs every other self-test again, the costliest case of the local
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

A="ADR"
V="Verifies"
# id <n> — the identifier ADR-000n.
id() { printf '%s-%04d' "$A" "$1"; }

# repo <name> — a repository with an ADR record of three files, printed as its path.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr" "$dir/src"
  git -C "${dir:?}" init -q -b main
  printf '# %s: a\n\nSee [%s](0002-beta.md).\n' "$(id 1)" "$(id 2)" > "$dir/docs/adr/0001-alpha.md"
  printf '# %s: b\n\nRests on G-1.\n' "$(id 2)" > "$dir/docs/adr/0002-beta.md"
  printf '# %s: c\n' "$(id 3)" > "$dir/docs/adr/0003-gamma.md"
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

# A swap of two ADRs: every identifier and file name moves once, so the two come out swapped.
d="$(repo swap)"
printf 'Read [%s](docs/adr/0001-alpha.md), then %s.\n' "$(id 1)" "$(id 2)" > "$d/README.md"
printf '| [0001](0001-alpha.md) | [0002](docs/adr/0002-beta.md) |\n' > "$d/index.md"
printf '# %s: %s %s\n' "$V" "$(id 1)" "$(id 2)" > "$d/src/test.sh"
run "$d" "0001-alpha.md 0002-beta.md" "0002-beta.md 0001-alpha.md"; rc=$?
check "a swap exits 0" "[[ $rc -eq 0 ]]"
check "a swap moves an identifier and its link together" \
  "grep -qF 'Read [$(id 2)](docs/adr/0002-beta.md), then $(id 1).' '$d/README.md'"
check "a swap moves a marker's identifiers" "grep -qF '$V: $(id 2) $(id 1)' '$d/src/test.sh'"
check "a swap moves the number in the text of a link to an ADR file" \
  "grep -qxF '| [0002](0002-beta.md) | [0001](docs/adr/0001-alpha.md) |' '$d/index.md'"
check "a swap prints the files it changed" "grep -qx 'README.md' '$work/out' && grep -qx 'src/test.sh' '$work/out'"

# The record is written new by the normalization: no ADR identifier moves under docs/adr/.
check "the record under docs/adr/ is left as it was" \
  "grep -qF '[$(id 2)](0002-beta.md)' '$d/docs/adr/0001-alpha.md'"

# A merge of two ADRs into one: both old identifiers name the new one.
d="$(repo merge)"
printf '%s and %s, see 0003-gamma.md.\n' "$(id 2)" "$(id 3)" > "$d/README.md"
run "$d" "0001-alpha.md 0001-alpha.md" "0002-beta.md 0002-beta-gamma.md" "0003-gamma.md 0002-beta-gamma.md"; rc=$?
check "a merge exits 0" "[[ $rc -eq 0 ]]"
check "a merge maps both old ADRs to the new one" \
  "grep -qF '$(id 2) and $(id 2), see 0002-beta-gamma.md.' '$d/README.md'"

# A swap of two criteria: in Markdown, on a marker line, and in an ADR; never in the specification
# and never on another line of a file that is not Markdown.
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
check "a criterion cited in an ADR moves" "grep -qF 'Rests on G-2.' '$d/docs/adr/0002-beta.md'"
check "the specification is left as it was" "grep -qF -- '- **G-1** — one' '$d/docs/SPECIFICATION.md'"

# An ADR with no successor: its references stay and are listed, and the rest still moves.
d="$(repo noheir)"
printf '%s, %s.\n' "$(id 1)" "$(id 3)" > "$d/README.md"
printf 'See [0003](0003-gamma.md).\n' > "$d/index.md"
run "$d" "0001-alpha.md 0002-alpha.md" "0003-gamma.md -"; rc=$?
check "a reference to an ADR with no successor exits 2" "[[ $rc -eq 2 ]]"
check "a reference to an ADR with no successor is listed" \
  "grep -qF 'README.md:1: $(id 3) has no successor' '$work/out'"
check "the other references still move" "grep -qF '$(id 2), $(id 3).' '$d/README.md'"
check "a link to an ADR with no successor is listed by its file" \
  "grep -qF 'index.md:1: 0003-gamma.md has no successor' '$work/out'"

# A file git ignores is left alone.
d="$(repo ignored)"
printf 'notes.md\n' > "$d/.gitignore"
printf '%s\n' "$(id 1)" > "$d/notes.md"
run "$d" "0001-alpha.md 0002-alpha.md"; rc=$?
check "an ignored file is left alone" "[[ $rc -eq 0 ]] && grep -qxF '$(id 1)' '$d/notes.md'"

# An invalid map changes nothing.
d="$(repo invalid)"
printf '%s\n' "$(id 1)" > "$d/README.md"
run "$d" "0001-alpha.md 0002-alpha.md" "G-1 Q-1"; rc=$?
check "a criterion mapped across kinds exits 1" "[[ $rc -eq 1 ]]"
check "an invalid map changes nothing" "grep -qxF '$(id 1)' '$d/README.md'"
run "$d" "0001-alpha.md 0002-alpha.md" "0001-other.md 0003-other.md"; rc=$?
check "an ADR mapped twice exits 1" "[[ $rc -eq 1 ]] && grep -qF 'mapped twice' '$work/out'"
run "$d" "0001-alpha.md"; rc=$?
check "a line without NEW exits 1" "[[ $rc -eq 1 ]]"

# Outside a repository the script refuses.
mkdir -p "$work/bare"
printf '0001-alpha.md 0002-alpha.md\n' > "$work/map-bare"
(cd "$work/bare" && bash "$REMAP" "$work/map-bare") > /dev/null 2>&1; rc=$?
check "outside a repository the script exits 1" "[[ $rc -eq 1 ]]"

# The real tree: every ADR and every criterion moved one place on, the last to the first, leaves
# every other self-test passing. So no fixture carries a literal identifier whose move by a
# normalization would break it. The template setup self-test reads the record itself, which the
# map script leaves to the normalization, so it is not run; its references to the record are
# links and file names, which the map script moves.
copy="$work/tree"
mkdir -p "$copy"
(cd "$ROOT" && git ls-files --cached --others --exclude-standard | while IFS= read -r f; do
  [[ -e "$f" || -L "$f" ]] && printf '%s\0' "$f"
done | tar cf - --null -T -) | tar xf - -C "$copy"
git -C "$copy" init -q && git -C "$copy" add -A
# cycle <ids...> — one map line per identifier, each to the next and the last to the first.
cycle() {
  local i
  for ((i = 0; i < $#; i++)); do
    printf '%s %s\n' "${@:i+1:1}" "${@:(i + 1) % $# + 1:1}"
  done
}
mapfile -t adrs < <(cd "$copy/docs/adr" && printf '%s\n' [0-9][0-9][0-9][0-9]-*.md)
mapfile -t goals < <(grep -oE '^[-0-9. ]*\*\*G-[0-9]+\*\*' "$copy/docs/SPECIFICATION.md" | grep -oE 'G-[0-9]+')
mapfile -t quals < <(grep -oE '^[-0-9. ]*\*\*Q-[0-9]+\*\*' "$copy/docs/SPECIFICATION.md" | grep -oE 'Q-[0-9]+')
{ cycle "${adrs[@]}"; cycle "${goals[@]}"; cycle "${quals[@]}"; } > "$work/map-tree"
(cd "$copy" && bash .agents/skills/normalize/remap.sh "$work/map-tree") > /dev/null 2>&1; rc=$?
check "a cycle of the real tree's ADRs and criteria exits 0" "[[ $rc -eq 0 ]]"
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
