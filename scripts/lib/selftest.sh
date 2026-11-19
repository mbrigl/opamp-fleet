# shellcheck shell=bash
# Shared by the self-tests under scripts/ (scripts/test-*.sh), sourced as their first act: the
# repository root, a temporary directory for the fixtures, the counters, the guard on a fixture's
# path, the reader of the fixture tables, one counted assertion, and the summary.
#
# The checks a self-test exercises build fixture repositories under $work. Git hands a hook, a
# linked worktree, and 'git -c' variables that bind every git command to one repository;
# inherited, a fixture's commands would act on this one and could push from it. They are unset
# here, so a self-test run from anywhere is as safe as one run by hand (see .githooks/pre-push).
#
# Usage, from scripts/test-<name>.sh:
#     # shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
#     source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
#     ...
#     summary "<Name> self-test"

# shellcheck source=scripts/lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/bash.sh"

# shellcheck disable=SC2034 # ROOT is the sourcing self-test's
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# shellcheck disable=SC2046 # one variable name per word
unset $(git rev-parse --local-env-vars)

# A fixture repository has no .gitattributes of its own. Git for Windows sets core.autocrlf=true,
# which checks a fixture's files out with CRLF and prints a warning naming each file; a check
# then reads a status word with a carriage return, and a needle meets a file name in a warning.
# The fixtures are written with LF and read as they are written. MSYS copies on 'ln -s' unless
# told to make a native link, and a fixture's pointer under .claude/skills/ must be a symlink.
# The count starts at 0: the unset above drops a GIT_CONFIG_COUNT the environment set, with the
# settings it counts, since they would reach every fixture as 'git -c' does.
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.autocrlf GIT_CONFIG_VALUE_0=false
[[ " ${MSYS-} " == *" winsymlinks:nativestrict "* ]] || export MSYS="${MSYS:+$MSYS }winsymlinks:nativestrict"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

failed=0
passed=0

# fixture <dir> — refuse a path that is no fixture repository under $work: 'git -C ""' acts on
# this repository, and a path outside $work may be this repository or another one.
fixture() {
  [[ -n "${1:-}" && "$1" == "$work"/* && -d "$1/.git" ]] \
    || { echo "fixture: '${1:-}' is no fixture repository under $work" >&2; exit 1; }
}

# table_hyphens <text> — the notation of the fixture tables under scripts/fixtures/ read: a colon
# after two to five capital letters, not preceded by a letter, a digit, or '_', and before a letter
# or a digit is a hyphen.
table_hyphens() { sed -E 's/(^|[^A-Za-z0-9_])([A-Z]{2,5}):([A-Za-z0-9])/\1\2-\3/g' <<< "$1"; }

# table_holds <output> <field> — whether the output meets the expected field of a case of a table:
#
#   -                         anything
#   <key>[ @ <subject>]...    a line that ends in '[<key>]' and holds each subject,
#                             in that order, with no ASCII letter, digit, hyphen, or
#                             underscore beside it
#   !<key>[ @ <subject>]...   no such line
#
# A key is a rule identifier '<area>/<name>', a colon, and a reason word. A subject is a datum of
# the case's tree: a file, a file and line, an identifier, or a term in double quotes. The wording
# around them is free. Anything else in a field exits 2, which stops a wording outside quotes.
# The subjects are looked for in the part of the line before its key, so that no subject holds by
# the key alone, as 'docs' would in '[docs/links:broken]'. A field '!<key> @ <a> @ <b>' holds where
# no line that ends in the key holds <a> and then <b>: a line with the subjects in the other order
# does not break it.
table_holds() {
  local field="${2#!}" line rest subject found=1 re='^[a-z]+/[a-z-]+:[a-z]+( @ ([^ "]+|"[^"]+"))*$'
  local -a parts
  [[ "$2" != - ]] || return 0
  [[ "$field" =~ $re ]] || { echo "fixture table: '$2' is no key and subjects" >&2; exit 2; }
  mapfile -t parts <<< "${field// @ /$'\n'}"
  while IFS= read -r line; do
    [[ "$line" == *"[${parts[0]}]" ]] || continue
    rest="${line%"[${parts[0]}]"}"
    for subject in "${parts[@]:1}"; do table_datum "$rest" "$subject" || continue 2; rest="$TABLE_REST"; done
    found=0
  done <<< "$1"
  [[ "$2" == '!'* ]] && return $((1 - found))
  return "$found"
}

# table_datum <text> <subject> — whether the text holds the subject as a datum of its own, neither
# ASCII letter, digit, hyphen, nor underscore on either side, so that '2' is not found in '12',
# nor 'a-rule' in 'a-rule-two'. A dot or a slash is no such character: 'x.md' is found in
# 'docs/x.md'. TABLE_REST is the text after the first such place, where the next
# subject of a field is looked for: the subjects stand in the order the field names them.
table_datum() {
  local seen="" text="$1" before
  while [[ "$text" == *"$2"* ]]; do
    before="$seen${text%%"$2"*}" TABLE_REST="${text#*"$2"}"
    [[ "$before" =~ [A-Za-z0-9_-]$ || "$TABLE_REST" =~ ^[A-Za-z0-9_-] ]] || return 0
    seen="$before${2:0:1}" text="${2:1}$TABLE_REST"
  done
  return 1
}

# output_holds <output> <needle> — a needle of a self-test's own case: a text the output contains,
# which may be a '[<key>]', or '[<key>] @ <subject>...', read as a field of a table.
output_holds() {
  local field="${2#[}"
  [[ "$2" == '['*'] @ '* ]] || { [[ "$1" == *"$2"* ]]; return; }
  table_holds "$1" "${field/] @ / @ }"
}

# table_case <name> <pass or fail> <field> <exit code> <output> — count one case of a table: exit
# code 0 for pass and 1 for fail, and the field holds. A failing case has to name a key.
table_case() {
  local want=0
  [[ "$2" == pass ]] || want=1
  [[ "$2" == pass || "$3" != - ]] || { echo "fixture table: the failing case '$1' names no key" >&2; exit 2; }
  if [[ "$4" == "$want" ]] && table_holds "$5" "$3"; then passed=$((passed + 1)); return; fi
  failed=$((failed + 1))
  echo "FAIL: $1 (expected $2 with '$3', exit $4)" >&2
  while IFS= read -r line; do printf '    %s\n' "$line"; done <<< "$5" >&2
}

# check <name> <condition...> — count one assertion.
check() {
  local name="$1"; shift
  if "$@"; then passed=$((passed + 1)); else failed=$((failed + 1)); echo "FAIL: $name" >&2; fi
}

# summary <name> — the result line, and the exit code: 0 when every case passed, 1 otherwise.
summary() {
  if ((failed)); then
    echo "$1 FAILED ($failed of $((passed + failed)) cases)." >&2
    exit 1
  fi
  echo "$1 passed ($passed cases)."
  exit 0
}
