#!/usr/bin/env bash
#
# Self-test of scripts/sensor-plain-rules.sh. Verifies: NUC-checks-are-bash
#
# Verifies: NUC-documents-are-fixed
#
# Each case builds a throwaway git repository under a temporary directory: main holds an AGENTS.md
# and one ADR, a branch changes them, and the sensor runs against main. The cases are a table of
# what the change adds and what the sensor must and must not print: a long sentence in the Decision
# and in the Enforcement, one in the Context, one held in a code span, a soft modal verb in
# AGENTS.md, named in italics, after an italic that wraps, in a code span that wraps, in an
# identifier, in a block quote, in fenced code, after a fence's marker line with an info string or a
# '## ' line inside it, and on a line the change did not add, a sentence that ends early, one that
# ends after a link whose text is a code span, an avoided word after a bare star or an unclosed
# underscore, an avoided term of two words wrapped over two lines, around a code span, with only its
# second line added, and split by a paragraph break, a sub-list that ends an _Avoid:_ list, a change
# outside the rules, the working tree with an untracked ADR, and a base that does not resolve. The
# cases of '--require' stand in the fixture table scripts/fixtures/plain-rules.txt: what fails in a
# project ADR and is reported in a NUC- ADR and in AGENTS.md, the qualifiers of an _Avoid:_ item, an
# item a line break splits, a renamed ADR, and a take-up on comb's glossary. Two cases read the
# glossary reader's rows directly.
#
# Inputs: scripts/sensor-plain-rules.sh scripts/lib/ scripts/fixtures/plain-rules.txt
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
# The identifier of the fixture's NUC- ADR, assembled so that check-docs.sh reads no reference here.
NUC_Y="NUC""-rule-y"

# git_in <dir> <args...> — git in the fixture, with an author.
git_in() {
  fixture "$1"
  local dir="$1"; shift
  git -C "${dir:?}" -c user.name=fixture -c user.email=fixture@example.invalid "$@"
}

# repo <name> — main with AGENTS.md and docs/adr/FIX-rule-x.md, then the branch 'work' checked out.
repo() {
  local dir="$work/$1"
  mkdir -p "$dir/docs/adr"
  git -C "${dir:?}" init -q -b main
  printf '# Agent Guide\n\n## 1. Rules\n\n- %s\n- Old line that says should.\n' "$SHORT" > "$dir/AGENTS.md"
  printf '# FIX-rule-x: X\n\n- **Status:** 🟢 accepted\n\n## Context\n\n%s\n\n## Decision\n\n%s\n\n## Enforcement\n\n%s\n' \
    "$SHORT" "$SHORT" "$SHORT" > "$dir/docs/adr/FIX-rule-x.md"
  sed "s/^# FIX-rule-x: X/# $NUC_Y: Y/" "$dir/docs/adr/FIX-rule-x.md" > "$dir/docs/adr/$NUC_Y.md"
  git_in "$dir" add -A
  git_in "$dir" commit -q -m start
  git_in "$dir" switch -q -c work
  echo "$dir"
}

# after <dir> <file> <heading> <text> — insert a paragraph after the heading line, and commit.
after() {
  fixture "$1"
  T="$4" awk -v h="$3" '{ print } $0 == h { print ""; print ENVIRON["T"] }' "$1/$2" > "$1/$2.new"
  mv "$1/$2.new" "$1/$2"
  git_in "$1" commit -q -am "change $2"
}

# expect <name> <dir> <needle> <absent> [args...] — run the sensor; exit 0, output with the needle
# and without the absent text (either may be empty).
expect() {
  local name="$1" dir="$2" needle="$3" absent="$4"; shift 4
  local out rc
  out="$(bash "$SENSOR" --root "$dir" "$@" 2>&1)"; rc=$?
  if [[ $rc -eq 0 && ( -z "$absent" || "$out" != *"$absent"* ) ]] && output_holds "$out" "$needle"; then
    passed=$((passed + 1))
    return
  fi
  failed=$((failed + 1))
  echo "FAIL: $name (got exit $rc)" >&2
  while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

# --- cases -----------------------------------------------------------------------------------

r="$(repo decision)"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$LONG"
expect "a long sentence in the Decision: listed with file, line, and count" "$r" \
  "[rules/long-sentence:words] @ docs/adr/FIX-rule-x.md:11 @ 28" "" main

r="$(repo enforcement)"
after "$r" docs/adr/FIX-rule-x.md "## Enforcement" "$LONG"
expect "a long sentence in the Enforcement: listed" "$r" "[rules/long-sentence:words] @ 28" "" main

r="$(repo context)"
after "$r" docs/adr/FIX-rule-x.md "## Context" "$LONG"
expect "a long sentence in the Context: not listed" "$r" "no long sentence" "" main

r="$(repo code-span)"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "Run \`$LONG\` first."
expect "a long sentence in a code span: not listed" "$r" "no long sentence" "" main

r="$(repo two-lines)"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '%s\n%s' "${LONG%% before it*}" "before it${LONG#* before it}")"
expect "a long sentence wrapped over two lines: counted as one" "$r" "[rules/long-sentence:words] @ 28" "" main

r="$(repo early-end)"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "${LONG/ any work / any work. Then it }"
expect "a sentence that ends early: two short ones" "$r" "no long sentence" "" main

r="$(repo end-after-link)"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "${LONG/ any work / any [\`docs/GLOSSARY.md\`](docs/GLOSSARY.md). Then it }"
expect "a sentence that ends after a link whose text is a code span: two short ones" "$r" \
  "no long sentence" "" main

r="$(repo modal)"
after "$r" AGENTS.md "## 1. Rules" "- The session should read the glossary."
expect "a soft modal verb in AGENTS.md: listed with line and word" "$r" \
  '[rules/soft-modal:verb] @ AGENTS.md:5 @ "should"' "" main
expect "a soft modal verb on a line not added: not listed" "$r" "" "AGENTS.md:8" main

r="$(repo italics)"
after "$r" AGENTS.md "## 1. Rules" "- **Plain.** Say *must*; *should* leaves the reader guessing."
expect "a soft modal verb named in italics: not listed" "$r" "no long sentence" "" main

r="$(repo wrapped-italics)"
after "$r" AGENTS.md "## 1. Rules" "$(printf -- '- **Cache.** A session writes the cache with *write\n  mode*, and it should flush before the *next* run.')"
expect "an italic wrapped over two lines: a real verb after it is listed" "$r" \
  '[rules/soft-modal:verb] @ AGENTS.md:6 @ "should"' "" main

r="$(repo underscore)"
after "$r" AGENTS.md "## 1. Rules" "- **Plain.** Say _must_; _should_ leaves the reader guessing."
expect "a soft modal verb named in underscore italics: not listed" "$r" "no long sentence" "" main

r="$(repo snake)"
after "$r" AGENTS.md "## 1. Rules" "- The run_should_stop flag should be set."
expect "an underscore inside a word opens no italic: the verb after it is listed" "$r" \
  '[rules/soft-modal:verb] @ AGENTS.md:5 @ "should"' "" main

r="$(repo wrapped-span)"
# shellcheck disable=SC2016 # the backticks are a Markdown code span
after "$r" AGENTS.md "## 1. Rules" "$(printf -- '- **Stop.** Run `make it so it\n  should stop` before the next step.')"
expect "a code span wrapped over two added lines: none of its text is read" "$r" "no long sentence" "" main

r="$(repo no-snake-verb)"
after "$r" AGENTS.md "## 1. Rules" "- The run_should_stop flag is set."
expect "a soft modal verb inside an identifier: not listed" "$r" "no long sentence" "" main

r="$(repo fence-info)"
# shellcheck disable=SC2016 # the backticks are a Markdown fence
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '````md\n```sh\nit should run\n````\n\nIt might run.')"
expect "a fence holds a line of its marker with an info string: the fence ends at its closing line only" \
  "$r" '[rules/soft-modal:verb] @ docs/adr/FIX-rule-x.md:16 @ "might"' '"should"' main

r="$(repo fence-heading)"
# shellcheck disable=SC2016 # the backticks are a Markdown fence
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '```md\n## Notes\n```\n\nIt might run.')"
expect "a '## ' line inside a fence: the section continues" "$r" \
  '[rules/soft-modal:verb] @ docs/adr/FIX-rule-x.md:15 @ "might"' "" main
section="$(source "$ROOT/scripts/lib/adr.sh"; adr_section Decision < "$r/docs/adr/FIX-rule-x.md")"
check "a '## ' line inside a fence: adr_section continues the section as the reader does" \
  test "${section/It might run./}" != "$section"

r="$(repo quote)"
after "$r" AGENTS.md "## 1. Rules" "> A quote that should not count."
expect "a soft modal verb in a block quote: not listed" "$r" "no long sentence" "" main

r="$(repo fence)"
# shellcheck disable=SC2016 # the backticks are a Markdown fence
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '```sh\n# this could run\n```')"
expect "a soft modal verb in fenced code: not listed" "$r" "no long sentence" "" main

r="$(repo outside)"
printf '%s should\n' "$LONG" > "$r/README.md"
git_in "$r" add README.md
git_in "$r" commit -q -m readme
expect "a change outside the rules: nothing to report" "$r" "no long sentence" "" main

r="$(repo untracked)"
printf '# FIX-rule-y: Y\n\n## Decision\n\nWe might do it.\n' > "$r/docs/adr/FIX-rule-y.md"
expect "the working tree: an untracked ADR is read" "$r" \
  '[rules/soft-modal:verb] @ docs/adr/FIX-rule-y.md:5 @ "might"' ""

# glossary <dir> [entries] — a glossary whose entry "Git hook" lists "hook script" and "trigger"
# under _Avoid:_, or the entries given, committed on the branch.
glossary() {
  fixture "$1"
  printf '# Glossary\n\nOne term per concept; list under _Avoid:_ the synonyms it displaces.\n\n%s\n' \
    "${2:-$(printf -- '- **Git hook** — a script git runs.\n  _Avoid:_ hook script, trigger.')}" > "$1/docs/GLOSSARY.md"
  git_in "$1" add docs/GLOSSARY.md
  git_in "$1" commit -q -m glossary
}
AVOID_KEY='[rules/avoided-term:listed]'

r="$(repo avoid-decision)"
glossary "$r"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "It runs a hook script."
expect "an avoided term added to a Decision: listed with the term" "$r" \
  "$AVOID_KEY @ docs/adr/FIX-rule-x.md:11 @ \"hook script\" @ \"Git hook\"" "" main

r="$(repo avoid-agents)"
glossary "$r"
after "$r" AGENTS.md "## 1. Rules" "- Each Trigger runs."
expect "an avoided term added to AGENTS.md, in another case: listed" "$r" "$AVOID_KEY @ AGENTS.md:5 @ \"trigger\" @ \"Git hook\"" "" main

r="$(repo avoid-context)"
glossary "$r"
after "$r" docs/adr/FIX-rule-x.md "## Context" "It runs a hook script."
expect "an avoided term in the Context: not listed" "$r" "" "$AVOID_KEY" main

r="$(repo avoid-old)"
glossary "$r"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "It runs a hook script."
git_in "$r" branch -f main HEAD
after "$r" docs/adr/FIX-rule-x.md "## Enforcement" "Run it."
expect "an avoided term on a line not added, in a file the change touches: not listed" "$r" "" "$AVOID_KEY" main

r="$(repo avoid-head)"
glossary "$r"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "It runs a hook script."
rm "$r/docs/GLOSSARY.md"
expect "with a head, the glossary is the head's, not the working tree's" "$r" \
  "$AVOID_KEY @ docs/adr/FIX-rule-x.md:11 @ \"hook script\" @ \"Git hook\"" "" main work

r="$(repo avoid-mentions)"
glossary "$r"
# shellcheck disable=SC2016 # the backticks are a Markdown code span
after "$r" docs/adr/FIX-rule-x.md "## Decision" 'Each `trigger`, *trigger*, and _trigger_ is named; snake_case_trigger and triggers run.'
expect "an avoided term in a code span, in italics, in an identifier, or in a longer word: not listed" \
  "$r" "" "$AVOID_KEY" main

r="$(repo avoid-after-star)"
glossary "$r"
# shellcheck disable=SC2016 # the dollar sign is the rule's text
after "$r" AGENTS.md "## 1. Rules" "$(printf -- '- Compute a * b from $_ENV, and then\n  run a hook script.')"
expect "a bare star and an unclosed underscore open no italic: an avoided word after them is listed" "$r" \
  "$AVOID_KEY @ AGENTS.md:6 @ \"hook script\" @ \"Git hook\"" "" main

r="$(repo avoid-none)"
glossary "$r" "- **Git hook** — a script git runs."
after "$r" docs/adr/FIX-rule-x.md "## Decision" "It runs a hook script."
expect "a glossary without _Avoid:_ entries: nothing listed" "$r" "no long sentence" "" main

r="$(repo avoid-prose)"
glossary "$r" "- **Git hook** — a script git runs."
after "$r" AGENTS.md "## 1. Rules" "- Each entry names the synonyms it displaces."
expect "the glossary's intro prose lists nothing" "$r" "no long sentence" "" main

r="$(repo avoid-parser)"
glossary "$r" "$(printf '%s\n' '- **Ticket** — one slice of work.' '  _Avoid:_ story; task,' '  and card.' '' \
  '- **Spec** — the destination of a feature.' '  It is disposable.' '  _Avoid:_ epic.')"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '%s\n' 'A card is cut.' 'Then an epic closes.' 'It is disposable.')"
expect "the parser: a continuation line and a leading 'and' list a word" "$r" \
  '[rules/avoided-term:listed] @ "card" @ "Ticket"' "" main
expect "the parser: each entry names its own term, and a description lists nothing" "$r" \
  '[rules/avoided-term:listed] @ "epic" @ "Spec"' '"disposable"' main

r="$(repo avoid-wrapped)"
glossary "$r"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '%s\n' 'It runs a hook' '  script before each commit.')"
expect "an avoided term of two words wrapped over two added lines: listed by the line it starts on" "$r" \
  "$AVOID_KEY @ docs/adr/FIX-rule-x.md:11 @ \"hook script\" @ \"Git hook\"" "" main

r="$(repo avoid-wrapped-span)"
glossary "$r"
# shellcheck disable=SC2016 # the backticks are a Markdown code span
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '%s\n' 'It runs a hook `x`' '  script daily.')"
expect "a code span between the words of an avoided term across a line break: not listed" "$r" "" "$AVOID_KEY" main

r="$(repo avoid-wrapped-second)"
glossary "$r"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "It runs a hook"
git_in "$r" branch -f main HEAD
awk '{ print } $0 == "It runs a hook" { print "  script daily." }' "$r/docs/adr/FIX-rule-x.md" > "$r/new" && mv "$r/new" "$r/docs/adr/FIX-rule-x.md"
git_in "$r" commit -q -am "wrap"
expect "an avoided term whose second line alone is added: listed by that line" "$r" \
  "$AVOID_KEY @ docs/adr/FIX-rule-x.md:12 @ \"hook script\" @ \"Git hook\"" "docs/adr/FIX-rule-x.md:11:" main

r="$(repo avoid-paragraph)"
glossary "$r"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '%s\n' 'It runs on each hook' '' 'Script authors read it.')"
expect "the words of an avoided term across a paragraph break: not listed" "$r" "" "$AVOID_KEY" main

r="$(repo avoid-sublist)"
glossary "$r" "$(printf '%s\n' '- **Ticket** — one slice of work.' '  _Avoid:_ story,' '  - kept apart from boards,' '    lanes.')"
after "$r" docs/adr/FIX-rule-x.md "## Decision" "$(printf '%s\n' 'A story is cut.' 'Lanes are cut.')"
expect "the parser: an indented sub-list line ends the _Avoid:_ list" "$r" \
  '[rules/avoided-term:listed] @ "story" @ "Ticket"' '"lanes"' main

r="$(repo bad-base)"
out="$(bash "$SENSOR" --root "$r" no-such-ref 2>&1)"; rc=$?
check "a base that does not resolve: exit 2" test "$rc" -eq 2 -a "${out/does not resolve/}" != "$out"

# A reader that breaks: an awk on PATH that fails on the program rule_stream runs per file, told
# by its 'adr' variable, and hands every other program to the real awk.
broken="$work/broken-awk"
mkdir -p "$broken"
# shellcheck disable=SC2016 # the script is written as it runs, its variables unexpanded
printf '#!/usr/bin/env bash\nfor a; do [[ "$a" == adr=* ]] && { echo "awk: syntax error" >&2; exit 2; }; done\nexec %q "$@"\n' \
  "$(command -v awk)" > "$broken/awk"
chmod +x "$broken/awk"

r="$(repo broken-reader)"
after "$r" AGENTS.md "## 1. Rules" "- $SHORT"
out="$(PATH="$broken:$PATH" bash "$SENSOR" --root "$r" main 2>&1)"; rc=$?
check "a reader that fails: exit 2, naming the file" test "$rc" -eq 2 -a "${out/could not read the rules of AGENTS.md/}" != "$out"

# A sensor whose awk breaks, told by its 'max' variable: exit 2, never a silent pass.
r="$(repo broken-sensor)"
after "$r" AGENTS.md "## 1. Rules" "- $SHORT"
sed 's/adr=\*/max=*/' "$broken/awk" > "$work/awk" && mkdir -p "$work/max" && mv "$work/awk" "$work/max/awk" && chmod +x "$work/max/awk"
out="$(PATH="$work/max:$PATH" bash "$SENSOR" --root "$r" main 2>&1)"; rc=$?
check "a sensor whose awk fails: exit 2, saying so" test "$rc" -eq 2 -a "${out/awk failed on the rules/}" != "$out"

# A glossary reader that breaks, told by its function 'take': exit 2, never a silent pass.
r="$(repo broken-glossary)"
glossary "$r"
after "$r" AGENTS.md "## 1. Rules" "- $SHORT"
mkdir -p "$work/take"
# shellcheck disable=SC2016 # the script is written as it runs, its variables unexpanded
printf '#!/usr/bin/env bash\nfor a; do [[ "$a" == *"function take("* ]] && { echo "awk: syntax error" >&2; exit 2; }; done\nexec %q "$@"\n' \
  "$(command -v awk)" > "$work/take/awk"
chmod +x "$work/take/awk"
out="$(PATH="$work/take:$PATH" bash "$SENSOR" --root "$r" main 2>&1)"; rc=$?
check "a glossary reader that fails: exit 2, saying so" test "$rc" -eq 2 -a "${out/could not read docs\/GLOSSARY.md/}" != "$out"

# A listing that breaks: a git on PATH that fails on '--name-status', which only rule_stream asks for
# when it lists the files a change touches, and hands every other command to the real git.
unlisted="$work/broken-git"
mkdir -p "$unlisted"
# shellcheck disable=SC2016 # the script is written as it runs, its variables unexpanded
printf '#!/usr/bin/env bash\nfor a; do [[ "$a" == --name-status ]] && { echo "fatal: broken" >&2; exit 128; }; done\nexec %q "$@"\n' \
  "$(command -v git)" > "$unlisted/git"
chmod +x "$unlisted/git"

r="$(repo broken-listing)"
after "$r" AGENTS.md "## 1. Rules" "- $SHORT"
out="$(PATH="$unlisted:$PATH" bash "$SENSOR" --root "$r" main 2>&1)"; rc=$?
check "a listing that fails: exit 2, saying it could not list the rules" \
  test "$rc" -eq 2 -a "${out/could not list the rules the change touches/}" != "$out"

# --- the cases of --require, from the fixture table ------------------------------------------

TABLE="$ROOT/scripts/fixtures/plain-rules.txt"

# step <dir> <step> — apply one step of a tree of the table to the fixture.
step() {
  local dir="$1" verb="${2%% *}" rest="${2#* }" file section text from to
  case "$verb" in
    add)
      file="${rest%% *}"; rest="${rest#* }"; section="${rest%% *}"; text="${rest#* }"
      [[ "$section" == Rules ]] && section="1. Rules"
      after "$dir" "$file" "## $section" "${text//\\n/$'\n'}" ;;
    glossary)
      glossary "$dir" "$(awk -F ' [|] ' -v n="$rest" '$1 == "= " n { print $2 }' "$TABLE")" ;;
    base) git_in "$dir" branch -f main HEAD ;;
    rename)
      from="${rest%% *}"; to="${rest#* }"
      git_in "$dir" mv "docs/adr/$from.md" "docs/adr/$to.md"
      sed "1s/^# $from:/# $to:/" "$dir/docs/adr/$to.md" > "$dir/new" && mv "$dir/new" "$dir/docs/adr/$to.md"
      git_in "$dir" commit -q -am "rename $from" ;;
    takeup)
      rm -f "$dir"/docs/adr/NUC-*
      cp "$ROOT"/docs/adr/NUC-* "$ROOT/AGENTS.md" "$dir/docs/adr/" && mv "$dir/docs/adr/AGENTS.md" "$dir/AGENTS.md"
      git_in "$dir" add -A
      git_in "$dir" commit -q -m takeup ;;
    noflag) flag=() ;;
    *) echo "unknown step '$2'" >&2; return 1 ;;
  esac
}

cases=0
while IFS= read -r line; do
  [[ "$line" =~ ^(pass|fail)\ \|\  ]] || continue
  line="${line//NUC:/NUC-}"
  IFS='|' read -r want text name tree <<< "${line// | /|}"
  cases=$((cases + 1))
  r="$(repo "table-$cases")"
  flag=(--require)
  ok=1
  while IFS= read -r s; do [[ -z "$s" ]] || step "$r" "$s" >/dev/null || ok=0; done <<< "${tree//; /$'\n'}"
  out="$(bash "$SENSOR" --root "$r" "${flag[@]}" main 2>&1)"; rc=$?
  ((ok)) || rc="a step of the tree failed"
  table_case "$name" "$want" "$text" "$rc" "$out"
done < "$TABLE"
check "the fixture table holds cases" test "$cases" -gt 20

# The reader reads a qualifier with its word, and says what it cannot read.
reader="$work/reader-glossary.md"
awk -F ' [|] ' '$1 == "= shape" { print $2 }' "$TABLE" > "$reader"
terms="$(source "$ROOT/scripts/lib/rules.sh"; avoid_terms "$reader")"
check "the reader: '*shape* alone' is the word with its qualifier" \
  grep -qxF "$(printf 'A\tshape\tNode shape\talone')" <<< "$terms"
printf -- '- **Size** — how large a node is.\n  _Avoid:_ Größe.\n' > "$reader"
check "the reader: a word with letters beyond ASCII is read" \
  grep -qxF "$(printf 'A\tgröße\tSize\t')" <<< "$(source "$ROOT/scripts/lib/rules.sh"; avoid_terms "$reader")"
# shellcheck disable=SC2016 # the backticks are a Markdown code span
printf -- '- **Pipeline** — what runs a change.\n  _Avoid:_ `pin`, ci/cd, -flag, .dot, \x27quoted.\n' > "$reader"
items="$(source "$ROOT/scripts/lib/rules.sh"; avoid_terms "$reader")"
check "the reader: an item in backticks is read as its word" grep -qxF "$(printf 'A\tpin\tPipeline\t')" <<< "$items"
for item in ci/cd -flag .dot "'quoted"; do
  check "the reader: '$item' is an item it cannot read" grep -qxF "$(printf 'U\t2\t%s\tPipeline' "$item")" <<< "$items"
done
check "the reader: an item it cannot read is named with its line" \
  grep -qxF "$(printf 'U\t6\toutside the wording of the constraint that requires it\tCulling')" <<< "$terms"

summary "Plain rules sensor self-test"
