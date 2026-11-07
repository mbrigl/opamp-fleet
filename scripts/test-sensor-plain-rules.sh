#!/usr/bin/env bash
#
# Self-test of scripts/sensor-plain-rules.sh. Verifies: ADR-0002
#
# Verifies: ADR-0001
#
# Each case builds a throwaway git repository under a temporary directory: main holds an
# AGENTS.md and one ADR, a branch changes them, and the sensor runs against main. The cases are a
# table of what the change adds and what the sensor must and must not print: a long sentence in
# the Decision and in the Enforcement, one in the Context, one held in a code span, a soft modal
# verb in AGENTS.md, named in italics, after an italic that wraps, in a code span that wraps, in
# an identifier, in a block quote, in fenced code, after a fence's marker line with an info string
# or a '## ' line inside it, and on a line the change did not add, a sentence that ends early, an
# avoided word after a bare star or an unclosed underscore, an avoided term of two words wrapped
# over two lines, around a code span, with only its second line added, and split by a paragraph
# break, a sub-list that ends an _Avoid:_ list, a change outside the rules, the working tree with
# an untracked ADR, and a base that does not resolve.
#
# Inputs: scripts/sensor-plain-rules.sh scripts/lib/
#
# Usage:
#     scripts/test-sensor-plain-rules.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"
SENSOR="$ROOT/scripts/sensor-plain-rules.sh"

LONG="The session that reads this rule before it starts any work must find every single word of it in the glossary and must also know who acts here."
SHORT="The human decides."

# git_in <dir> <args...> — git in the fixture, with an author.
git_in() {
  fixture "$1"
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=fixture -c user.email=fixture@example.invalid "$@"
}

# repo <name> — main with AGENTS.md and docs/adr/0001-x.md, then the branch 'work' checked out.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr"
  git -C "${dir:?}" init -q -b main
  printf '# Agent Guide\n\n## 1. Rules\n\n- %s\n- Old line that says should.\n' "$SHORT" > "$dir/AGENTS.md"
  printf '# ADR-0001: X\n\n- **Status:** 🟢 accepted\n\n## Context\n\n%s\n\n## Decision\n\n%s\n\n## Enforcement\n\n%s\n' \
    "$SHORT" "$SHORT" "$SHORT" > "$dir/docs/adr/0001-x.md"
  git_in "$dir" add -A
  git_in "$dir" commit -q -m start
  git_in "$dir" switch -q -c work
  echo "$dir"
}

# after <dir> <file> <heading> <text> — insert a paragraph after the heading line, and commit.
after() {
  fixture "$1"
  awk -v h="$3" -v t="$4" '{ print } $0 == h { print ""; print t }' "$1/$2" > "$1/$2.new"
  mv "$1/$2.new" "$1/$2"
  git_in "$1" commit -q -am "change $2"
}

# expect <name> <dir> <needle> <absent> [args...] — run the sensor; exit 0, output with the needle
# and without the absent text (either may be empty).
expect() {
  local name="$1" dir="$2" needle="$3" absent="$4"; shift 4
  local out rc
  out="$(bash "$SENSOR" --root "$dir" "$@" 2>&1)"; rc=$?
  if [[ $rc -eq 0 && "$out" == *"$needle"* && ( -z "$absent" || "$out" != *"$absent"* ) ]]; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

r="$(repo decision)"
after "$r" docs/adr/0001-x.md "## Decision" "$LONG"
expect "a long sentence in the Decision: listed with file, line, and count" "$r" \
  "docs/adr/0001-x.md:11: a sentence of 28 words" "" main

r="$(repo enforcement)"
after "$r" docs/adr/0001-x.md "## Enforcement" "$LONG"
expect "a long sentence in the Enforcement: listed" "$r" "a sentence of 28 words" "" main

r="$(repo context)"
after "$r" docs/adr/0001-x.md "## Context" "$LONG"
expect "a long sentence in the Context: not listed" "$r" "no long sentence" "" main

r="$(repo code-span)"
after "$r" docs/adr/0001-x.md "## Decision" "Run \`$LONG\` first."
expect "a long sentence in a code span: not listed" "$r" "no long sentence" "" main

r="$(repo two-lines)"
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '%s\n%s' "${LONG%% before it*}" "before it${LONG#* before it}")"
expect "a long sentence wrapped over two lines: counted as one" "$r" "a sentence of 28 words" "" main

r="$(repo early-end)"
after "$r" docs/adr/0001-x.md "## Decision" "${LONG/ any work / any work. Then it }"
expect "a sentence that ends early: two short ones" "$r" "no long sentence" "" main

r="$(repo modal)"
after "$r" AGENTS.md "## 1. Rules" "- The session should read the glossary."
expect "a soft modal verb in AGENTS.md: listed with line and word" "$r" \
  'AGENTS.md:5: the soft modal verb "should"' "" main
expect "a soft modal verb on a line not added: not listed" "$r" "" "AGENTS.md:8" main

r="$(repo italics)"
after "$r" AGENTS.md "## 1. Rules" "- **Plain.** Say *must*; *should* leaves the reader guessing."
expect "a soft modal verb named in italics: not listed" "$r" "no long sentence" "" main

r="$(repo wrapped-italics)"
after "$r" AGENTS.md "## 1. Rules" "$(printf -- '- **Cache.** A session writes the cache with *write\n  mode*, and it should flush before the *next* run.')"
expect "an italic wrapped over two lines: a real verb after it is listed" "$r" \
  'AGENTS.md:6: the soft modal verb "should"' "" main

r="$(repo underscore)"
after "$r" AGENTS.md "## 1. Rules" "- **Plain.** Say _must_; _should_ leaves the reader guessing."
expect "a soft modal verb named in underscore italics: not listed" "$r" "no long sentence" "" main

r="$(repo snake)"
after "$r" AGENTS.md "## 1. Rules" "- The run_should_stop flag should be set."
expect "an underscore inside a word opens no italic: the verb after it is listed" "$r" \
  'AGENTS.md:5: the soft modal verb "should"' "" main

r="$(repo wrapped-span)"
# shellcheck disable=SC2016 # the backticks are a Markdown code span
after "$r" AGENTS.md "## 1. Rules" "$(printf -- '- **Stop.** Run `make it so it\n  should stop` before the next step.')"
expect "a code span wrapped over two added lines: none of its text is read" "$r" "no long sentence" "" main

r="$(repo no-snake-verb)"
after "$r" AGENTS.md "## 1. Rules" "- The run_should_stop flag is set."
expect "a soft modal verb inside an identifier: not listed" "$r" "no long sentence" "" main

r="$(repo fence-info)"
# shellcheck disable=SC2016 # the backticks are a Markdown fence
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '````md\n```sh\nit should run\n````\n\nIt might run.')"
expect "a fence holds a line of its marker with an info string: the fence ends at its closing line only" \
  "$r" 'docs/adr/0001-x.md:16: the soft modal verb "might"' '"should"' main

r="$(repo fence-heading)"
# shellcheck disable=SC2016 # the backticks are a Markdown fence
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '```md\n## Notes\n```\n\nIt might run.')"
expect "a '## ' line inside a fence: the section continues" "$r" \
  'docs/adr/0001-x.md:15: the soft modal verb "might"' "" main
section="$(source "$ROOT/scripts/lib/adr.sh"; adr_section Decision < "$r/docs/adr/0001-x.md")"
check "a '## ' line inside a fence: adr_section continues the section as the reader does" \
  test "${section/It might run./}" != "$section"

r="$(repo quote)"
after "$r" AGENTS.md "## 1. Rules" "> A quote that should not count."
expect "a soft modal verb in a block quote: not listed" "$r" "no long sentence" "" main

r="$(repo fence)"
# shellcheck disable=SC2016 # the backticks are a Markdown fence
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '```sh\n# this could run\n```')"
expect "a soft modal verb in fenced code: not listed" "$r" "no long sentence" "" main

r="$(repo outside)"
printf '%s should\n' "$LONG" > "$r/README.md"
git_in "$r" add README.md
git_in "$r" commit -q -m readme
expect "a change outside the rules: nothing to report" "$r" "no long sentence" "" main

r="$(repo untracked)"
printf '# ADR-0002: Y\n\n## Decision\n\nWe might do it.\n' > "$r/docs/adr/0002-y.md"
expect "the working tree: an untracked ADR is read" "$r" \
  'docs/adr/0002-y.md:5: the soft modal verb "might"' ""

# glossary <dir> [entries] — a glossary whose entry "Git hook" lists "hook script" and "trigger"
# under _Avoid:_, or the entries given, committed on the branch.
glossary() {
  fixture "$1"
  printf '# Glossary\n\nOne term per concept; list under _Avoid:_ the synonyms it displaces.\n\n%s\n' \
    "${2:-$(printf -- '- **Git hook** — a script git runs.\n  _Avoid:_ hook script, trigger.')}" > "$1/docs/GLOSSARY.md"
  git_in "$1" add docs/GLOSSARY.md
  git_in "$1" commit -q -m glossary
}
AVOID_MSG='is listed under _Avoid:_ in docs/GLOSSARY.md — write "Git hook"'

r="$(repo avoid-decision)"
glossary "$r"
after "$r" docs/adr/0001-x.md "## Decision" "It runs a hook script."
expect "an avoided term added to a Decision: listed with the term" "$r" \
  "docs/adr/0001-x.md:11: \"hook script\" $AVOID_MSG" "" main

r="$(repo avoid-agents)"
glossary "$r"
after "$r" AGENTS.md "## 1. Rules" "- Each Trigger runs."
expect "an avoided term added to AGENTS.md, in another case: listed" "$r" "AGENTS.md:5: \"trigger\" $AVOID_MSG" "" main

r="$(repo avoid-context)"
glossary "$r"
after "$r" docs/adr/0001-x.md "## Context" "It runs a hook script."
expect "an avoided term in the Context: not listed" "$r" "" "$AVOID_MSG" main

r="$(repo avoid-old)"
glossary "$r"
after "$r" docs/adr/0001-x.md "## Decision" "It runs a hook script."
git_in "$r" branch -f main HEAD
after "$r" docs/adr/0001-x.md "## Enforcement" "Run it."
expect "an avoided term on a line not added, in a file the change touches: not listed" "$r" "" "$AVOID_MSG" main

r="$(repo avoid-head)"
glossary "$r"
after "$r" docs/adr/0001-x.md "## Decision" "It runs a hook script."
rm "$r/docs/GLOSSARY.md"
expect "with a head, the glossary is the head's, not the working tree's" "$r" \
  "docs/adr/0001-x.md:11: \"hook script\" $AVOID_MSG" "" main work

r="$(repo avoid-mentions)"
glossary "$r"
# shellcheck disable=SC2016 # the backticks are a Markdown code span
after "$r" docs/adr/0001-x.md "## Decision" 'Each `trigger`, *trigger*, and _trigger_ is named; snake_case_trigger and triggers run.'
expect "an avoided term in a code span, in italics, in an identifier, or in a longer word: not listed" \
  "$r" "" "$AVOID_MSG" main

r="$(repo avoid-after-star)"
glossary "$r"
# shellcheck disable=SC2016 # the dollar sign is the rule's text
after "$r" AGENTS.md "## 1. Rules" "$(printf -- '- Compute a * b from $_ENV, and then\n  run a hook script.')"
expect "a bare star and an unclosed underscore open no italic: an avoided word after them is listed" "$r" \
  "AGENTS.md:6: \"hook script\" $AVOID_MSG" "" main

r="$(repo avoid-none)"
glossary "$r" "- **Git hook** — a script git runs."
after "$r" docs/adr/0001-x.md "## Decision" "It runs a hook script."
expect "a glossary without _Avoid:_ entries: nothing listed" "$r" "no long sentence" "" main

r="$(repo avoid-prose)"
glossary "$r" "- **Git hook** — a script git runs."
after "$r" AGENTS.md "## 1. Rules" "- Each entry names the synonyms it displaces."
expect "the glossary's intro prose lists nothing" "$r" "no long sentence" "" main

r="$(repo avoid-parser)"
glossary "$r" "$(printf '%s\n' '- **Ticket** — one slice of work.' '  _Avoid:_ story; task,' '  and card.' '' \
  '- **Spec** — the destination of a feature.' '  It is disposable.' '  _Avoid:_ epic.')"
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '%s\n' 'A card is cut.' 'Then an epic closes.' 'It is disposable.')"
expect "the parser: a continuation line and a leading 'and' list a word" "$r" \
  '"card" is listed under _Avoid:_ in docs/GLOSSARY.md — write "Ticket"' "" main
expect "the parser: each entry names its own term, and a description lists nothing" "$r" \
  '"epic" is listed under _Avoid:_ in docs/GLOSSARY.md — write "Spec"' '"disposable" is listed' main

r="$(repo avoid-wrapped)"
glossary "$r"
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '%s\n' 'It runs a hook' '  script before each commit.')"
expect "an avoided term of two words wrapped over two added lines: listed by the line it starts on" "$r" \
  "docs/adr/0001-x.md:11: \"hook script\" $AVOID_MSG" "" main

r="$(repo avoid-wrapped-span)"
glossary "$r"
# shellcheck disable=SC2016 # the backticks are a Markdown code span
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '%s\n' 'It runs a hook `x`' '  script daily.')"
expect "a code span between the words of an avoided term across a line break: not listed" "$r" "" "$AVOID_MSG" main

r="$(repo avoid-wrapped-second)"
glossary "$r"
after "$r" docs/adr/0001-x.md "## Decision" "It runs a hook"
git_in "$r" branch -f main HEAD
awk '{ print } $0 == "It runs a hook" { print "  script daily." }' "$r/docs/adr/0001-x.md" > "$r/new" && mv "$r/new" "$r/docs/adr/0001-x.md"
git_in "$r" commit -q -am "wrap"
expect "an avoided term whose second line alone is added: listed by that line" "$r" \
  "docs/adr/0001-x.md:12: \"hook script\" $AVOID_MSG" "docs/adr/0001-x.md:11:" main

r="$(repo avoid-paragraph)"
glossary "$r"
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '%s\n' 'It runs on each hook' '' 'Script authors read it.')"
expect "the words of an avoided term across a paragraph break: not listed" "$r" "" "$AVOID_MSG" main

r="$(repo avoid-sublist)"
glossary "$r" "$(printf '%s\n' '- **Ticket** — one slice of work.' '  _Avoid:_ story,' '  - kept apart from boards,' '    lanes.')"
after "$r" docs/adr/0001-x.md "## Decision" "$(printf '%s\n' 'A story is cut.' 'Lanes are cut.')"
expect "the parser: an indented sub-list line ends the _Avoid:_ list" "$r" \
  '"story" is listed under _Avoid:_ in docs/GLOSSARY.md — write "Ticket"' '"lanes" is listed' main

r="$(repo bad-base)"
out="$(bash "$SENSOR" --root "$r" no-such-ref 2>&1)"; rc=$?
check "a base that does not resolve: exit 2" test "$rc" -eq 2 -a "${out/does not resolve/}" != "$out"

# A reader that breaks: an awk on PATH that fails on the program rule_stream runs per file, told
# by its 'added' variable, and hands every other program to the real awk.
broken="$work/broken-awk"
mkdir -p "$broken"
# shellcheck disable=SC2016 # the script is written as it runs, its variables unexpanded
printf '#!/usr/bin/env bash\nfor a; do [[ "$a" == added=* ]] && { echo "awk: syntax error" >&2; exit 2; }; done\nexec %q "$@"\n' \
  "$(command -v awk)" > "$broken/awk"
chmod +x "$broken/awk"

r="$(repo broken-reader)"
after "$r" AGENTS.md "## 1. Rules" "- $SHORT"
out="$(PATH="$broken:$PATH" bash "$SENSOR" --root "$r" main 2>&1)"; rc=$?
check "a reader that fails: exit 2, naming the file" test "$rc" -eq 2 -a "${out/could not read the rules of AGENTS.md/}" != "$out"

# A listing that breaks: a git on PATH that fails on '--name-only', which only rule_stream asks for
# when it lists the files a change touches, and hands every other command to the real git.
unlisted="$work/broken-git"
mkdir -p "$unlisted"
# shellcheck disable=SC2016 # the script is written as it runs, its variables unexpanded
printf '#!/usr/bin/env bash\nfor a; do [[ "$a" == --name-only ]] && { echo "fatal: broken" >&2; exit 128; }; done\nexec %q "$@"\n' \
  "$(command -v git)" > "$unlisted/git"
chmod +x "$unlisted/git"

r="$(repo broken-listing)"
after "$r" AGENTS.md "## 1. Rules" "- $SHORT"
out="$(PATH="$unlisted:$PATH" bash "$SENSOR" --root "$r" main 2>&1)"; rc=$?
check "a listing that fails: exit 2, saying it could not list the rules" \
  test "$rc" -eq 2 -a "${out/could not list the rules the change touches/}" != "$out"

summary "Plain rules sensor self-test"
