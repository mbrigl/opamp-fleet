#!/usr/bin/env bash
#
# Sensor: timed waits added to the Rust tests without a reason (NUC-checks-are-bash §4, the sensors
# the template ships; docs/CONVENTIONS.md, "A test waits for its event, never for a time").
#
# Lists, between a base and a head, each added line of a Rust test that waits for a time rather
# than for its event: a 'thread::sleep(', 'time::sleep(' or 'sleep_until(', and a Duration literal
# ('Duration::from_secs(5)', 'Duration::new(1, 0)') in a statement with no 'timeout(', no
# assertion, which compares a time and waits for none, and no call of the waiter, 'wait_until(' or
# 'wait_until_within(' of crates/fleet-agent/tests/common/mod.rs. It reports and never fails. The
# section stood in the template's scripts/sensor-tests-kept.sh until the design revision of
# 2026-11-19 moved it here, so that the template's file is the template's again.
#
# A test is a Rust file under 'crates/*/tests/', a file under 'crates/*/src/' whose path below
# 'src/' is a test path as the tests-kept sensor reads one, a segment or the basename that
# contains 'test' or 'spec' ('test_support.rs'), and the '#[cfg(test)]' items of any other file
# under 'crates/*/src/'. A line that carries its reason in a comment, '// timed: <why>', on it or
# on a comment line right above it, is left out: the reason is read where the wait is, not in the
# pull request. It is a heuristic: a sleep reached through a bare 'sleep(' after a 'use', a
# Duration held in a constant, and a value that is no wait, such as a lease's term, are reported
# or missed by their text alone; a wait within an assertion,
# 'assert!(eventually(Duration::from_secs(5), ..))', is left out, and so is a literal of a later
# field or argument of a statement whose earlier line, ending in ',' or '(', called 'timeout(': a
# statement ends only at a line that ends in ';', '{' or '}', or at a blank line.
#
# Pure bash + git + awk. Usage:
#     scripts/sensor-timed-waits.sh [--root <dir>] [base [head]]
# It reads the repository it lives in, as the checks do; --root names another, as its self-test
# does with each fixture.
# Without a base, the merge base of head with origin/main (else main) is used; CI passes the
# pull request's base. A base of zeros, which a push event reports for a new branch, counts as
# none. With nothing to compare — on main without a remote, or base and head the same — it says
# so. Exit code 0 always, except 2 on a base or head that does not resolve, an argument it does
# not read, or a root that is no directory.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/sensor-timed-waits.sh [--root <dir>] [base [head]]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
set -- "${ARGS[@]}"
rest_args "${BASH_SOURCE[0]}" "$usage" 2 "$@"

resolve_change "Timed waits sensor" "${1:-}" "${2:-HEAD}" || exit 0
base="$CHANGE_BASE" head="$CHANGE_HEAD"
if [[ "$(git rev-parse "$base^{commit}")" == "$(git rev-parse "$head^{commit}")" ]]; then
  echo "Timed waits sensor: nothing to compare — base and head are the same commit."
  exit 0
fi

# is_test_path <path> — a segment or the basename contains 'test' or 'spec', in any case, as
# scripts/sensor-tests-kept.sh reads a test path.
is_test_path() { [[ "${1,,}" =~ (^|/)[^/]*(test|spec)[^/]*(/|$) ]]; }

# timed_candidates prints, for a file as head
# has it, each line of its test code that would be listed were it added, as 'line<TAB>text'. The
# test code is the whole file, or with PART=cfg its '#[cfg(test)]' items: from the attribute to
# where the item's braces close, or for an item without braces, to its ';' (a 'mod tests;'), or
# its ',' outside parentheses (a field). Braces and parentheses in a comment, a one-line string or
# a character literal are not counted. The exemptions read the statement the line is part of,
# from the line after the last one that ended in ';', '{' or '}': rustfmt breaks an assertion or
# a call over lines, and its 'assert_eq!(' or 'timeout(' stands on the first; a comment line
# between them does not end it, a blank line does. A reason is read on the line or on a comment
# line right above it. Whether an item without braces ends at a ',' is read on its first line:
# a field ('hook: fn(u8),') does, an item that a keyword opens ('fn', 'impl', 'where' later) waits
# for its braces or its ';'.
timed_candidates() {
  git show "$head:$1" 2>/dev/null | PART="$2" awk '
    {
      code = $0
      gsub(/"([^"\\]|\\.)*"/, "\"\"", code)
      gsub(/'\''([^'\''\\]|\\.)'\''/, "0", code)
      sub(/\/\/.*$/, "", code)
      reason = ($0 ~ /\/\/.*timed:[ \t]*[^ \t]/) || (above ~ /^[ \t]*\/\/.*timed:[ \t]*[^ \t]/)
      above = $0
      statement = statement " " code
    }
    ENVIRON["PART"] == "cfg" { inside = cfg_test(code) }
    ENVIRON["PART"] != "cfg" { inside = 1 }
    inside && !reason {
      sleep = $0 ~ /(thread::sleep|time::sleep|sleep_until)\(/
      literal = code ~ /Duration::(from_[a-z0-9_]+|new)\([ \t]*[0-9]/ \
                && statement !~ /timeout\(|wait_until(_within)?\(|assert[a-z_]*!/
      if (sleep || literal) printf "%d\t%s\n", NR, $0
    }
    code ~ /[;{}][ \t]*$/ || $0 ~ /^[ \t]*$/ { statement = "" }

    # cfg_test <code> — whether the line is part of a #[cfg(test)] item, the state carried over
    # lines: 0 outside, 1 after the attribute and before the braces, 2 within them.
    function cfg_test(code,   opens, closes, lead) {
      if (state == 0) {
        if (code !~ /#\[cfg\(([^]]*[^a-z_])?test([^a-z_][^]]*)?\)\]/ || code ~ /not\(test\)/)
          return 0
        state = 1; parens = 0; first = 1
        sub(/^.*#\[cfg\([^]]*\)\]/, "", code)
      }
      if (state == 1) {
        sub(/^[ \t]*(#\[[^]]*\][ \t]*)*/, "", code)
        if (code ~ /^[ \t]*$/) return 1
        if (first) {
          lead = code
          sub(/^[ \t]*(pub(\([^)]*\))?[ \t]+)?((async|const|unsafe|default|extern( "")?)[ \t]+)*/, \
              "", lead)
          sub(/[^A-Za-z0-9_!].*$/, "", lead)
          braceless_comma = lead != "macro_rules!" \
            && lead !~ /^(fn|impl|mod|struct|enum|trait|union|type|use|static|const)$/
          first = 0
        }
        opens = gsub(/\{/, "{", code); closes = gsub(/\}/, "}", code)
        if (opens > 0) {
          depth = opens - closes
          state = (depth <= 0) ? 0 : 2
          return 1
        }
        parens += gsub(/\(/, "(", code) - gsub(/\)/, ")", code)
        if (parens <= 0 && (code ~ /;[ \t]*$/ || (braceless_comma && code ~ /,[ \t]*$/))) state = 0
        return 1
      }
      depth += gsub(/\{/, "{", code) - gsub(/\}/, "}", code)
      if (depth <= 0) state = 0
      return 1
    }
  '
}

# The candidates of each Rust file under crates/ the change adds to or modifies, as
# 'path<TAB>line<TAB>text': a file under 'crates/*/tests/' or a test path below 'src/' whole, any
# other only in its #[cfg(test)] items.
candidates=""
while IFS= read -r path; do
  part=cfg
  if [[ "$path" == crates/*/tests/* ]] \
     || { [[ "$path" == crates/*/src/* ]] && is_test_path "${path#crates/*/src/}"; }; then
    part=whole
  fi
  while IFS=$'\t' read -r line text; do
    candidates+="$path"$'\t'"$line"$'\t'"$text"$'\n'
  done < <(timed_candidates "$path" "$part")
done < <(git -c core.quotePath=false diff --name-only -M --diff-filter=AMR "$base" "$head" \
           -- 'crates/*.rs')

# The candidates on an added line, read from one diff that pairs a renamed file with its old
# path, so that the lines a move carries over are not read as added.
# The list reaches awk through a file: an environment variable holds at most 128 KiB.
timed=()
if [[ -n "$candidates" ]]; then
  listed="$(mktemp)"
  trap 'rm -f "$listed"' EXIT
  printf '%s' "$candidates" > "$listed"
  while IFS= read -r hit; do
    timed+=("$hit")
  done < <(git -c core.quotePath=false diff -M -U0 --no-color "$base" "$head" -- 'crates/*.rs' \
           | awk '
               FNR == NR {
                 split($0, cols, "\t")
                 text[cols[1] SUBSEP cols[2]] = substr($0, length(cols[1] cols[2]) + 3)
                 next
               }
               /^\+\+\+ / { file = substr($0, 7); sub(/\t$/, "", file); next }
               /^@@/      {
                 match($0, /\+[0-9]+/); line = substr($0, RSTART + 1, RLENGTH - 1) + 0; next
               }
               /^\+/      {
                 key = file SUBSEP line
                 if (key in text) printf "%s:%d: %s\n", file, line, text[key]
                 line++
               }
             ' "$listed" -)
fi

short_base="$(git rev-parse --short "$base")"
short_head="$(git rev-parse --short "$head")"
if ((${#timed[@]} == 0)); then
  echo "Timed waits sensor ($short_base..$short_head): nothing to report."
  exit 0
fi
echo "Timed waits sensor ($short_base..$short_head): ${#timed[@]} timed wait(s) added to the tests — each waits for its event instead, or carries its reason on the line as '// timed: <why>' (docs/CONVENTIONS.md, \"A test waits for its event, never for a time\"):"
printf '  - timed wait added: %s\n' "${timed[@]}"
exit 0
