# shellcheck shell=bash
# The rules a change adds to, read one way by scripts/sensor-plain-rules.sh: AGENTS.md, and the
# sections ADR_RULE_SECTIONS of each ADR (AGENTS.md section 4). Sourced after scripts/lib/adr.sh
# and scripts/lib/git.sh, run inside the repository of the change.

# rule_stream <base> [head] — every line of the rules in the files the change touches, as the
# change has them: without a head, the working tree with its untracked files. For each file
# 'F<TAB><path>'; for each line of its rules 'L<TAB><number><TAB><1 if the change adds it, else
# 0><TAB><text>', the text without code spans, link targets, and words in italics, '*word*' or
# '_word_', which a rule uses to name a word, each italic a space; 'P' where a paragraph, a list
# item, a heading, a block quote, a table, or fenced code ends a run of prose. A code span or an
# italic may wrap within its run of prose; one that does not close in it is text. Fenced code and
# the sections of an ADR end where adr_section reads them to end (ADR_FENCE_AWK).
# 'E<TAB><path>' where a reader of that file fails, and 'E<TAB>' with no path where git cannot list
# the files; the status is then 1, since a rule that cannot be read is no rule that passes. A
# renamed file adds the lines its rename changes, as git's rename detection pairs it.
rule_stream() {
  local base="$1" head="${2:-}" p from lines files failed=0
  local -a refs=("$base")
  [[ -n "$head" ]] && refs+=("$head")
  files="$(
    set -o pipefail
    { git -c core.quotePath=false diff --name-status -M "${refs[@]}" \
        | awk -F '\t' '{ print $NF "\t" ($1 ~ /^R/ ? $2 : "") }' \
        && if [[ -z "$head" ]]; then git -c core.quotePath=false ls-files --others --exclude-standard | awk '{ print $0 "\t" }'; fi
    } | LC_ALL=C sort -u)" || { printf 'E\t\n'; return 1; }
  while IFS=$'\t' read -r p from; do
    [[ -n "$p" ]] || continue
    [[ "$p" == AGENTS.md ]] || { [[ "$p" == docs/adr/* && "$p" != docs/adr/*/* ]] && adr_is_file "$p"; } || continue
    if [[ -z "$head" ]]; then [[ -f "$p" ]] || continue; else git cat-file -e "$head:$p" 2>/dev/null || continue; fi
    if [[ -z "$head" ]] && ! git ls-files --error-unmatch -- "$p" >/dev/null 2>&1; then
      lines="$(awk '{ printf ",%d", NR }' "$p")" || { printf 'E\t%s\n' "$p"; failed=1; continue; }
    else
      lines="$(set -o pipefail; git diff -U0 -M "${refs[@]}" -- ${from:+"$from"} "$p" \
        | awk '/^@@ / { split($3, a, ","); s = substr(a[1], 2); n = (2 in a) ? a[2] : 1; for (i = 0; i < n; i++) printf ",%d", s + i }')" \
        || { printf 'E\t%s\n' "$p"; failed=1; continue; }
    fi
    [[ -n "$lines" ]] || continue
    printf 'F\t%s\n' "$p"
    ( set -o pipefail; change_file "$head" "$p" | ADDED="$lines," SECTIONS="^## ($ADR_RULE_SECTIONS)\$" awk \
      -v adr="$([[ "$p" == AGENTS.md ]] || echo 1)" "$ADR_FENCE_AWK"'
      BEGIN { added = ENVIRON["ADDED"]; sections = ENVIRON["SECTIONS"] }
      # A run of prose is held until it ends, since a code span or an italic may close on a later
      # line of it, and one that never closes is plain text, as Markdown reads it.
      function para() { flush(); if (!gap) print "P"; gap = 1 }
      function flush(  i, parts) {
        if (!held) return
        split(plain(buf), parts, "\n")
        for (i = 1; i <= held; i++) print "L\t" num[i] "\t" (index("," added, "," num[i] ",") ? 1 : 0) "\t" parts[i]
        held = 0; buf = ""; gap = 0
      }
      # breaks <text> — the line breaks of the text, all that is kept of a code span or an italic.
      function breaks(t) { gsub(/[^\n]/, "", t); return t }
      function run(s, i, c,  n) { n = 0; while (substr(s, i + n, 1) == c) n++; return n }
      # spans <text> — the text without code spans: a run of backticks opens one and the next run
      # of the same length closes it; a run that no other closes is text.
      function spans(s,  out, i, j, n, m) {
        out = ""; i = 1
        while (i <= length(s)) {
          if (substr(s, i, 1) != "`") { out = out substr(s, i, 1); i++; continue }
          n = run(s, i, "`")
          for (j = i + n; j <= length(s); j += m) {
            m = (substr(s, j, 1) == "`") ? run(s, j, "`") : 1
            if (m == n && substr(s, j, 1) == "`") break
          }
          if (j <= length(s)) { out = out breaks(substr(s, i, j + n - i)); i = j + n }
          else { out = out substr(s, i, n); i += n }
        }
        return out
      }
      # closer <text> <i> <char> — where the italic the char at i opens closes: a char after a
      # non-space, an underscore not before a letter or digit; 0 where none does.
      function closer(s, i, c,  j) {
        for (j = i + 2; j <= length(s); j++)
          if (substr(s, j, 1) == c && substr(s, j - 1, 1) ~ /[^[:space:]]/ && (c == "*" || substr(s, j + 1, 1) !~ /[[:alnum:]]/)) return j
        return 0
      }
      # plain <text> — the text without code spans, link targets, and italics, each italic a space
      # at either end. A star or an underscore opens an italic before a non-space when a closer
      # follows; an underscore inside a word, as in snake_case, does not. Bold is no italic.
      function plain(s,  out, i, j, c, prev) {
        s = spans(s)
        gsub(/\]\([^)\n]*\)/, "]", s)
        gsub(/\*\*/, "\001", s); gsub(/__/, "\002", s); out = ""; prev = " "
        for (i = 1; i <= length(s); i++) {
          c = substr(s, i, 1)
          if ((c == "*" || (c == "_" && prev !~ /[[:alnum:]]/)) && substr(s, i + 1, 1) ~ /[^[:space:]]/ && (j = closer(s, i, c))) {
            out = out " " breaks(substr(s, i, j - i + 1)) " "; i = j
          } else out = out c
          prev = c
        }
        gsub(/\001/, "**", out); gsub(/\002/, "__", out)
        return out
      }
      { f = fenced($0) }
      f == 2 { para(); next }
      f { next }
      /^## / { para(); if (adr) inside = ($0 ~ sections); next }
      adr && !inside { next }
      /^#/ || /^[[:space:]]*[>|]/ || /^[[:space:]]*$/ { para(); next }
      /^[[:space:]]*([-*+]|[0-9]+\.)[[:space:]]/ { para() }
      { num[++held] = NR; buf = (held > 1 ? buf "\n" : "") $0 }
      END { flush() }' ) || { printf 'E\t%s\n' "$p"; failed=1; }
  done <<< "$files"
  return "$failed"
}

# avoid_terms <glossary> — the items an entry of the glossary lists under _Avoid:_, its lines
# joined, split at commas and semicolons, without a leading 'and', a trailing period, italics, code
# span backticks, or a placeholder in angle brackets. Each item it reads is 'A<TAB>word<TAB>term<TAB>
# qualifier': one to three words in lower case, letters beyond ASCII among them, then 'alone' where
# the item ends so, else nothing; 'in prose' at its end reads as nothing, since only prose is read.
# Each item it cannot read is 'U<TAB>line<TAB>item<TAB>term', by the line it starts on, each term
# 'T<TAB>term' in lower case. An entry is its '- **Term**' line and the indented lines after it;
# prose outside one lists nothing (NUC-documents-are-fixed §5); an indented list item ends a list.
avoid_terms() {
  [[ -f "$1" ]] || return 0
  awk '
    # take <list> <line> — the items of a list from that line, "\003" where a line of it breaks.
    function take(s, line,  n, i, k, w, q, at, parts, parts2) {
      n = split(s, parts, /[,;]/); for (i = 1; i <= n; i++) {
        w = " " parts[i] " "; q = w; sub(/[^[:space:]\003].*/, "", q); at = line + gsub(/\003/, "", q); line += gsub(/\003/, "", w); gsub(/[*`]/, "", w)
        while (match(w, /[^[:alnum:]]_|_[^[:alnum:]]/)) w = substr(w, 1, RSTART - 1 + (substr(w, RSTART, 1) != "_")) substr(w, RSTART + 1 + (substr(w, RSTART, 1) != "_"))
        w = tolower(w); gsub(/[[:space:]]+/, " ", w); gsub(/^ (and )?| ?[.]? ?$/, "", w)
        if (w == "" || w ~ /[<>]/) continue
        q = ""; if (w ~ / alone$/) { q = "alone"; sub(/ alone$/, "", w) } else sub(/ in prose$/, "", w)
        k = split(w, parts2, " ")
        if (k > 3 || w ~ /^[-.\047]/ || w ~ /[\[\]!"#$%&()*+,\/:;<=>?@\\^_`{|}~]/) printf "U\t%d\t%s\t%s\n", at, w (q ? " " q : ""), term
        else printf "A\t%s\t%s\t%s\n", w, term, q
      }
    }
    function done() { if (open) take(list, from); open = 0 }
    /^- \*\*[^*]+\*\*/ { done(); term = $0; sub(/^- \*\*/, "", term); sub(/\*\*.*/, "", term); entry = 1; printf "T\t%s\n", tolower(term) }
    !/^- \*\*/ && !/^[[:space:]]+[^[:space:]]/ { done(); entry = 0; term = "" }
    /^[[:space:]]+([-*+]|[0-9]+\.)[[:space:]]/ { done() }
    entry && open && !index($0, "_Avoid:_") && /^[[:space:]]+[^[:space:]-]/ { list = list "\003" $0; next }
    entry && term != "" && index($0, "_Avoid:_") { done(); open = 1; from = NR; list = substr($0, index($0, "_Avoid:_") + 8) }
    END { done() }
  ' "$1"
}
