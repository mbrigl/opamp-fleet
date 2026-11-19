#!/usr/bin/env bash
#
# Sensor: rules written plainly (AGENTS.md section 4, NUC-checks-are-bash §4), and with '--require'
# the check of them (NUC-documents-are-fixed §5).
#
# Reads the rules a change touches as scripts/lib/rules.sh streams them: AGENTS.md, and the
# Decision and Enforcement of an ADR, without code spans, italics, link targets, block quotes,
# tables, fenced code, and headings; a renamed file by the lines its rename changes. It lists each
# sentence over 25 words that holds an added line, by the line it starts on, each soft modal verb
# on an added line: should, could, might, ideally, and each word on an added line that an entry of
# docs/GLOSSARY.md lists under _Avoid:_, with the term to write instead. A verb or a word counts
# whole, case insensitive, and not inside an identifier, where a letter, a digit, or an underscore
# adjoins it. A word does not count inside a longer term of the glossary that holds it, such as
# 'node shape' for 'shape', whether the item says 'alone' or another entry lists the word bare.
# It lists each item of an _Avoid:_ list that scripts/lib/rules.sh cannot read. A listed word
# that the line break of a paragraph splits counts when either line is added, on the line it
# starts on if that line is added, else on the next; the gap a code span or an italic leaves at
# the end of a line stays. A paragraph or a list item ends a sentence; so do '.', '!', and '?'
# before a space or the end, also after what a link whose text is a code span leaves.
#
# Without '--require' it reports for the reviewer and never fails on what it counts. With
# '--require' it fails on a soft modal verb or an avoided word in a project ADR; in a 'NUC-' ADR
# and in AGENTS.md, which a take-up replaces as the release has them, it reports them only.
#
# Pure bash + git + awk. Usage:
#     scripts/sensor-plain-rules.sh [--root <dir>] [--require] [base [head]]
# Without a base, the merge base of head with origin/main (else main) is used; CI passes the pull
# request's base, and a base of zeros counts as none. Without a head, the change is the working
# tree, untracked files included. Exit code 0, 1 with '--require' on what it fails on, and 2 on a
# base or head that does not resolve and on a reader of the rules that fails.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"
# shellcheck source-path=SCRIPTDIR source=lib/git.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/git.sh"
# shellcheck source-path=SCRIPTDIR source=lib/rules.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/rules.sh"

enter_root "Plain rules sensor" "${BASH_SOURCE[0]}" "$@"
shift "$ROOT_TAKEN"
require=0
[[ "${1:-}" == "--require" ]] && { require=1; shift; }

MAX_WORDS=25
resolve_change "Plain rules sensor" "${1:-}" "${2:-}" || exit 0
base="$CHANGE_BASE" head="$CHANGE_HEAD"
# at — the head a file is read at, empty for the working tree, as rule_stream and change_file take it.
at="$( ((CHANGE_WORKTREE)) || echo "$head")"
range="$(git rev-parse --short "$base")..$(git rev-parse --short "$head")"
[[ -n "$at" ]] || range="${range%..*}..working tree"

if ! stream="$(rule_stream "$base" "$at")"; then
  awk -F '\t' -v range="$range" '
    $1 == "E" && $2 == "" { print "Plain rules sensor (" range "): could not list the rules the change touches." }
    $1 == "E" && $2 != "" { print "Plain rules sensor (" range "): could not read the rules of " $2 "." }' <<< "$stream" >&2
  exit 2
fi
# The glossary as the change has it: the working tree's, or the head's.
glossary="$(mktemp)"
change_file "$at" docs/GLOSSARY.md > "$glossary" 2>/dev/null
avoid="$(avoid_terms "$glossary")"; rc=$?
rm -f "$glossary"
((rc == 0)) || { echo "Plain rules sensor ($range): could not read docs/GLOSSARY.md." >&2; exit 2; }
# The rows of the glossary and the rules reach awk through the environment and stdin: an awk of
# the BWK line refuses a newline in a '-v' value.
out="$(AVOID="$avoid" awk -F '\t' -v max="$MAX_WORDS" '
    # A row of avoid_terms: an item it reads (A), a term (T), or an item it cannot read (U).
    BEGIN {
      n = split(ENVIRON["AVOID"], rows, "\n"); na = 0; nt = 0
      for (i = 1; i <= n; i++) {
        split(rows[i], kv, "\t")
        if (kv[1] == "A") { word[++na] = kv[2]; term[na] = kv[3] }
        else if (kv[1] == "T") terms[++nt] = kv[2]
        else if (kv[1] == "U") print "docs/GLOSSARY.md:" kv[2] ": cannot read \"" kv[3] "\" under _Avoid:_ of \"" kv[4] "\" [rules/avoid-list:unread]"
      }
    }
    # bare <text> <i> — the text with each longer term of the glossary that holds the word of
    # item i blanked: a word inside a term of the glossary is no synonym to avoid.
    function bare(s, i,  k, t, b) {
      for (k = 1; k <= nt; k++) {
        t = terms[k]
        if (t == word[i] || !found(" " t " ", word[i], 0)) continue
        b = t; gsub(/./, " ", b)
        while (found(s, t, 0)) s = substr(s, 1, FOUND - 1) b substr(s, FOUND + length(t))
      }
      return s
    }
    function flush(  i, any) {
      if (count > max) {
        any = 0
        for (i in seen) if (seen[i] == 1) any = 1
        if (any) print file ":" start ": a sentence of " count " words [rules/long-sentence:words]"
      }
      count = 0; start = 0; split("", seen)
    }
    # squash <text> — the text in lower case without the white space at its ends, one space at either end.
    function squash(t) { t = tolower(t); sub(/^[[:space:]]+/, "", t); sub(/[[:space:]]+$/, "", t); return " " t " " }
    # found <text> <word> <break> — whether the word stands whole in the text; with a break, only
    # where it spans that position. FOUND is where it stands.
    function found(s, w, b,  rest, off, at, before, after) {
      rest = s; off = 0
      while ((at = index(rest, w)) > 0) {
        at += off
        before = substr(s, at - 1, 1); after = substr(s, at + length(w), 1)
        if (before !~ /[a-z0-9_]/ && after !~ /[a-z0-9_]/ && (!b || (at < b && at + length(w) > b))) { FOUND = at; return 1 }
        off = at; rest = substr(s, off + 1)
      }
      return 0
    }
    function avoided(nr, i) {
      if ((nr, i) in told) return
      told[nr, i] = 1
      print fails file ":" nr ": \"" word[i] "\" is listed under _Avoid:_ in docs/GLOSSARY.md — write \"" term[i] "\" [rules/avoided-term:listed]"
    }
    # A finding that fails with --require is marked "!": in a project ADR, not in a NUC- ADR or AGENTS.md.
    $1 == "F" { flush(); file = $2; prev = ""; split("", told); fails = (file ~ /^docs\/adr\/[^\/]+$/ && file !~ /^docs\/adr\/NUC-/) ? "!" : ""; next }
    $1 == "P" { flush(); prev = ""; next }
    {
      nr = $2; text = $4; low = squash(text)
      if (prev != "" && ($3 == 1 || prevadded)) {
        joined = prev " " substr(low, 2)
        for (i = 1; i <= na; i++) if (found(bare(joined, i), word[i], length(prev) + 1)) avoided(prevadded ? prevnr : nr, i)
      }
      prev = tolower(text); sub(/^[[:space:]]+/, "", prev); prev = " " prev; prevnr = nr; prevadded = ($3 == 1)
      if ($3 == 1) {
        n = split("should could might ideally", soft, " ")
        for (i = 1; i <= n; i++)
          if (low ~ ("[^a-z0-9_]" soft[i] "[^a-z0-9_]")) print fails file ":" nr ": the soft modal verb \"" soft[i] "\" [rules/soft-modal:verb]"
        for (i = 1; i <= na; i++) if (found(bare(low, i), word[i], 0)) avoided(nr, i)
      }
      w = split(text, words, /[[:space:]]+/)
      for (i = 1; i <= w; i++) {
        # A token without a letter or a digit, such as what a link leaves, ends a sentence by a stop.
        if (words[i] !~ /[A-Za-z0-9]/) { if (count && words[i] ~ /[.!?]/) flush(); continue }
        if (!count) start = nr
        count++; if (!(nr in seen) || $3 == 1) seen[nr] = $3
        if (words[i] ~ /[.!?][)*_"]*$/) flush()
      }
    }
    END { flush() }' <<< "$stream")" \
  || { echo "Plain rules sensor ($range): awk failed on the rules." >&2; exit 2; }
findings=()
while IFS= read -r finding; do [[ -z "$finding" ]] || findings+=("$finding"); done <<< "$out"

failing=0
for i in "${!findings[@]}"; do
  [[ "${findings[i]}" == '!'* ]] || continue
  findings[i]="${findings[i]#!}"
  ((require)) && { failing=$((failing + 1)); findings[i]="${findings[i]% \[*} (fails) [${findings[i]##* \[}"; }
done
if ((${#findings[@]} == 0)); then
  echo "Plain rules sensor ($range): no long sentence, no soft modal verb, and no avoided term on the lines the change adds to the rules."
  exit 0
fi
echo "Plain rules sensor ($range): ${#findings[@]} to read on the lines the change adds to the rules (AGENTS.md section 4):"
printf '  - %s\n' "${findings[@]}"
((failing == 0)) && exit 0
echo "Plain rules sensor: $failing of them fail: a soft modal verb or an avoided word in a project ADR (NUC-documents-are-fixed §5)."
exit 1
