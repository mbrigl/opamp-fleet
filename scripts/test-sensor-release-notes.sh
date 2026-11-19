#!/usr/bin/env bash
#
# Self-test of scripts/sensor-release-notes.sh.
#
# Each case builds a fixture repository with a release tag and merges of pull requests on main
# after it. The sensor reads the descriptions of the pull requests through gh; here a stand-in
# named by GH answers instead, with the description kept in a fixture file per pull request. The
# cases assert the field 'A derived project acts on' of each hand-off since the tag, a field that
# says none, an empty field, a description with CRLF line ends, the pull request of a feat/ branch
# whose inner merge lies off the first-parent line and is not read, a merge on main that is no
# pull request, the tag of the highest version as the default, the option --since, and the exit
# codes.
#
# Verifies: NUC-releases-are-semver-tags NUC-checks-are-bash
# Inputs: scripts/sensor-release-notes.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-release-notes.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-release-notes.sh"
USAGE="scripts/sensor-release-notes.sh [--root <dir>] [--since <tag>]"

# The stand-in for gh: 'pr view <n> …' prints the fixture file of that pull request, and fails
# where there is none, as gh does on an error.
fake="$work/gh"
cat > "$fake" <<'EOF'
#!/usr/bin/env bash
[[ "$1 $2" == "pr view" && -f "$FIX/pr-$3" ]] || exit 1
cat "$FIX/pr-$3"
EOF
chmod +x "$fake"

# g <dir> <git args...> — git in the fixture with an identity.
g() {
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=t -c user.email=t@example.invalid "$@"
}

# repo <name> — a fixture with one commit on main, printed as its path; its gh answers under fix/.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/fix"
  git -C "$dir" init -q -b main
  echo fix/ > "$dir/.gitignore"
  g "$dir" add -A
  g "$dir" commit -q -m base
  echo "$dir"
}

# tag <dir> <tag> — an annotated release tag on the tip of main.
tag() { g "$1" tag -a -m "release $2" "$2" main; }

# branch <dir> <branch> <from> — a branch with one commit, cut from <from>.
branch() {
  g "$1" switch -q -c "$2" "$3"
  echo "$2" > "$1/${2//\//-}.txt"
  g "$1" add -A
  g "$1" commit -q -m "work on $2"
}

# pr <dir> <n> <branch> [body] — the branch merged into main as GitHub merges pull request <n>;
# the body, if any, is the description gh answers with.
pr() {
  local dir="$1" n="$2" b="$3"
  git -C "$dir" rev-parse -q --verify "refs/heads/$b" > /dev/null || branch "$dir" "$b" main
  g "$dir" switch -q main
  g "$dir" merge -q --no-ff -m "Merge pull request #$n from owner/$b" "$b"
  (($# > 3)) && printf '%s\n' "$4" > "$dir/fix/pr-$n"
  return 0
}

# handoff <field> — a hand-off whose Doubts end in the field 'A derived project acts on'.
handoff() {
  printf '## Summary\n\nA change.\n\n## Doubts\n\n- Disagreed: none.\n- Unsure: none.\n'
  printf -- '- A derived project acts on:%s\n\n## Documents and decisions\n\n- ADR: none.\n' "$1"
}

# expect <name> <dir> <rc> <needle> [args...] — run the sensor; it must exit <rc> and its output
# hold the needle, which may span lines.
expect() {
  local name="$1" dir="$2" want="$3" needle="$4" out rc
  shift 4
  out="$(GH="${GH_CMD:-$fake}" FIX="$dir/fix" bash "$SENSOR" --root "$dir" "$@" 2>&1)"; rc=$?
  if [[ "$rc" == "$want" && "$out" == *"$needle"* ]]; then
    passed=$((passed + 1))
  else
    failed=$((failed + 1))
    echo "FAIL: $name (expected exit $want and '$needle', got exit $rc)" >&2
    while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
  fi
}

# absent <name> <dir> <needle> [args...] — run the sensor; its output must not hold the needle.
absent() {
  local name="$1" dir="$2" needle="$3" out
  shift 3
  out="$(GH="$fake" FIX="$dir/fix" bash "$SENSOR" --root "$dir" "$@" 2>&1)"
  check "$name" test "${out/"$needle"/}" = "$out"
}

# A release, then a pull request before the next: a field over two lines, a field with bullets
# under it, a field that says none, and a description without the field.
d="$(repo main)"
pr "$d" 1 fix/old "$(handoff ' rename the old key.')"
tag "$d" v0.1.0
pr "$d" 2 fix/two "$(handoff ' a call of scripts/x.sh --old exits 2;
  call scripts/x.sh --new instead.')"
pr "$d" 3 fix/three "$(handoff '
  - move the key a to b;
  - delete docs/old.md.')"
pr "$d" 4 docs/four "$(handoff ' none.')"
pr "$d" 5 docs/five '## Summary

A change from before the field.'
expect "the headline names the tag and the merges read" "$d" 0 \
  "Release notes sensor: what a derived project acts on, from the 4 merges listed since v0.1.0:"
expect "a field over two lines, from the pull request" "$d" 0 \
  "  - #2 fix/two, from its pull request description:
      a call of scripts/x.sh --old exits 2;
      call scripts/x.sh --new instead."
expect "a field with bullets under it" "$d" 0 \
  "  - #3 fix/three, from its pull request description:
      - move the key a to b;
      - delete docs/old.md."
expect "a field that says none" "$d" 0 \
  "  - #4 docs/four, from its pull request description: none."
expect "a description without the field says so" "$d" 0 \
  "  - #5 docs/five: its pull request description has no field 'A derived project acts on'."
absent "a field after the hand-off's Doubts is not read on" "$d" "ADR: none."
absent "a merge before the tag is not read" "$d" "#1 "

# --since an older tag reads from there; a tag that does not exist exits 2.
tag "$d" v0.2.0
pr "$d" 6 fix/six "$(handoff ' -')"
expect "the latest tag is the default" "$d" 0 \
  "Release notes sensor: what a derived project acts on, from the 1 merge listed since v0.2.0:"
expect "a field that says -" "$d" 0 "  - #6 fix/six, from its pull request description: -"
expect "--since reads from the tag it names" "$d" 0 \
  "  - #2 fix/two, from its pull request description:" --since v0.1.0
expect "--since a tag that does not exist exits 2" "$d" 2 \
  "Release notes sensor: 'v9.9.9' is no tag of $d." --since v9.9.9
expect "--since without a tag exits 2" "$d" 2 \
  "sensor-release-notes.sh: '--since' needs a tag — usage: $USAGE" --since

# A pull request gh cannot read: its merge message stands in where it carries the field.
d="$(repo unread)"
tag "$d" v1.0.0
pr "$d" 7 fix/seven
pr "$d" 8 fix/eight
g "$d" commit -q --amend -m "Merge pull request #8 from owner/fix/eight

$(handoff ' drop the file x.')"
expect "a description gh cannot read, and no field in the merge message" "$d" 0 \
  "  - #7 fix/seven: its pull request description was not read, and its merge message has no field 'A derived project acts on'."
expect "a description gh cannot read, the field in the merge message" "$d" 0 \
  "  - #8 fix/eight, from its merge message, since its pull request description was not read:
      drop the file x."

# A feat/ branch integrates linearly; its pull request into main carries the field of the whole
# branch. A merge into the feat/ branch, off the first-parent line of main, is not read, even
# one whose message carries the field; nor is a merge on main that is no pull request.
d="$(repo feat)"
tag "$d" v2.0.0
branch "$d" feat/s main
branch "$d" ticket/s/01-one feat/s
g "$d" switch -q feat/s
g "$d" merge -q --no-ff -m "Merge ticket/s/01-one

$(handoff ' the sensor y needs gh.')" ticket/s/01-one
pr "$d" 9 feat/s "$(handoff ' rename z.')"
branch "$d" other main
g "$d" switch -q main
g "$d" merge -q --no-ff -m "Merge branch 'other'

$(handoff ' drop w.')" other
expect "the pull request of the feat/ branch" "$d" 0 \
  "Release notes sensor: what a derived project acts on, from the 1 merge listed since v2.0.0:
  - #9 feat/s, from its pull request description:
      rename z."
absent "a merge into the feat/ branch is not read" "$d" "the sensor y needs gh."
absent "a merge on main that is no pull request is not read" "$d" "drop w."

# The highest version reachable from HEAD is the default, not the nearest tag, nor the
# highest by the letters: v0.10.0 before v0.9.0, which a later commit carries.
d="$(repo versions)"
tag "$d" v0.10.0
pr "$d" 1 fix/one "$(handoff ' x.')"
tag "$d" v0.9.0
pr "$d" 2 fix/two "$(handoff ' y.')"
expect "the highest version is the default tag" "$d" 0 \
  "Release notes sensor: what a derived project acts on, from the 2 merges listed since v0.10.0:"

# A description with CRLF line ends, as GitHub may keep them, reads as one with LF.
d="$(repo crlf)"
tag "$d" v0.1.0
pr "$d" 1 fix/one "$(handoff ' rename a;
  delete b.' | sed 's/$/\r/')"
expect "a description with CRLF line ends" "$d" 0 \
  "  - #1 fix/one, from its pull request description:
      rename a;
      delete b."
absent "a description with CRLF line ends leaves no carriage return" "$d" $'\r'

# An empty field on a pull request prints as empty.
d="$(repo blank)"
tag "$d" v0.1.0
pr "$d" 1 feat/s "$(handoff '')"
expect "an empty field on a pull request" "$d" 0 "  - #1 feat/s, from its pull request description: empty"
absent "an empty field raises no error" "$d" "bad array subscript"

# Nothing since the tag, no tag, no repository, no gh, a missing root, and an unknown argument.
d="$(repo empty)"
tag "$d" v0.1.0
expect "no merge since the tag says so" "$d" 0 \
  "Release notes sensor: no merge since v0.1.0 — nothing to report."
d="$(repo untagged)"
pr "$d" 1 fix/one "$(handoff ' x.')"
expect "no tag exits 2" "$d" 2 \
  "Release notes sensor: no tag v* is reachable from HEAD in $d — name one with --since <tag>."
mkdir -p "$work/plain"
expect "a root that is no git repository exits 2" "$work/plain" 2 \
  "Release notes sensor: '$work/plain' is no git repository."
GH_CMD="$work/no-such-gh" expect "no gh exits 2" "$work/main" 2 \
  "Release notes sensor: the GitHub CLI ($work/no-such-gh) is not installed."
expect "a root that does not exist exits 2" "$work/none" 2 \
  "sensor-release-notes.sh: '$work/none' is not a directory — usage: $USAGE"
expect "an unknown argument exits 2" "$work/main" 2 \
  "sensor-release-notes.sh: unknown argument '--verbose' — usage: $USAGE" --verbose

summary "Release notes sensor self-test"
