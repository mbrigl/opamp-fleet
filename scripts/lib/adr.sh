# shellcheck shell=bash disable=SC2034 # the variables are the sourcing script's
# The header of an ADR, read one way by every script that reads it: a header field is the list
# item '- **<Name>:** …' that docs/adr/template.md writes (NUC-adrs-are-named-by-category).
# check-docs.sh fails on a field written in any other form, so every other reader may take the
# list item for granted.
# The identifier of an ADR is read one way too: '<CAT>-<slug>' in the file '<CAT>-<slug>.md'
# (NUC-adrs-are-named-by-category §1).
# Sourced by the checks and sensors that read an ADR's file name, identifier, or header.

# An identifier: a category key of two to five capital letters, a hyphen, and a
# slug of two to five lower case words or numbers joined by hyphens.
ADR_SLUG_RE='[a-z0-9]+(-[a-z0-9]+){1,4}'
ADR_NEW_RE="[A-Z]{2,5}-$ADR_SLUG_RE"
# The fixed categories; a project defines its others in the category list of the ADR index.
ADR_FIXED_CATEGORIES=(NUC SEC OPS API)
# What a 'Verifies:' marker cites, read whole: capital letters, a hyphen, and the rest of the word.
# The reader holds each against the forms of an ADR's identifier, a goal, and a quality goal.
ADR_ID_RE='[A-Z]+-[A-Za-z0-9_-]+'
# The characters next to a reference: one of them on either side makes it part of a longer word.
ADR_WORD_CHARS='A-Za-z0-9_-'

# adr_categories <index> — sets ADR_CATEGORIES to the fixed categories and those the category list
# of the index defines, each a list item '- **<KEY>** …' below a heading '... Categories';
# ADR_REF_RE to an ADR as prose cites it, an identifier of a known category; and
# ADR_REF_WORD_RE to such a reference standing as a word of its own, the reference its group 2.
adr_categories() {
  local key
  ADR_CATEGORIES=("${ADR_FIXED_CATEGORIES[@]}")
  if [[ -f "$1" ]]; then
    while IFS= read -r key; do ADR_CATEGORIES+=("$key"); done < <(awk '
      /^#+ / { p = ($0 ~ /^#+ Categories[[:space:]]*$/); next }
      p && match($0, /^- [*][*][A-Z]+[*][*]/) { print substr($0, 5, RLENGTH - 6) }' "$1")
  fi
  local IFS='|'
  ADR_REF_RE="(${ADR_CATEGORIES[*]})-$ADR_SLUG_RE"
  ADR_REF_WORD_RE="(^|[^$ADR_WORD_CHARS])($ADR_REF_RE)([^$ADR_WORD_CHARS]|\$)"
}
adr_categories ""

# The roles a project fills with one accepted ADR each, named in its 'Role:' field
# (NUC-base-is-fixed §3).
ADR_ROLES=(harness toolchain)

# The fields of docs/adr/template.md, an item's start, a field's item, a field anywhere (awk-safe).
ADR_FIELDS='Status|Date|Deciders|Applies to|Supersedes|Extends|Narrows|Role'
ADR_ITEM_RE='^- [*][*]'
ADR_FIELD_ITEM_RE="${ADR_ITEM_RE}[A-Za-z ]+:[*][*]"
ADR_FIELD_MARK_RE="[*][*]($ADR_FIELDS):[*][*]"

# adr_field_line <file, or - for stdin> <name> — the first line of the header field, or nothing.
adr_field_line() { grep -m1 -E "$ADR_ITEM_RE$2:[*][*]" "$1"; }
# adr_field_value <line> <name> — sets ADR_VALUE to what follows the field's name on the line.
adr_field_value() { ADR_VALUE="${1##*\*\*"$2":\*\*}"; }

# The names of ADR files: any name of letters before its first hyphen, so that the rule
# docs/adr-identifiers of check-docs.sh sees a misnamed ADR, such as 'Sec-x.md'. ADR_FILE_RE
# matches the same names.
ADR_GLOB='[A-Za-z]*-*.md'
ADR_FILE_RE="[A-Za-z]+-[^/]*\\.md"

# adr_rule_paths <Applies to> — each path the line names in backticks with no space in it, one
# a line, '<path><TAB><keys>': the rule keys named before it, 'the rule `<key>` of `<path>`' or
# 'the rules `<a>`, `<b>`, and `<c>` of `<path>`', separated by spaces, or none.
adr_rule_paths() {
  local token p rules pre
  # shellcheck disable=SC2016  # the backticks are Markdown to match, not a command substitution
  while IFS= read -r token; do
    p="${token%\`}" && p="${p##*\`}" && rules="" pre=""
    [[ "$token" == "the rule"* ]] && pre="${token% of \`*}"
    while [[ "$pre" =~ \`([^\`]+)\`(.*) ]]; do
      rules+="${rules:+ }${BASH_REMATCH[1]}" && pre="${BASH_REMATCH[2]}"
    done
    [[ "$p" =~ ^[^[:space:]]+$ ]] && printf '%s\t%s\n' "${p%/}" "$rules"
  done < <(grep -oE '(the rules? `[^`]+`((,|, and| and) `[^`]+`)* of )?`[^`]+`' <<< "$1")
}

# adr_files <dir> — every ADR file in the directory, in order.
adr_files() { LC_ALL=C find "$1" -maxdepth 1 -type f -name "$ADR_GLOB" | LC_ALL=C sort; }

# adr_blobs <dir> — the blob list (NUC-base-is-fixed §2), '<blob> <file name>' a line:
# the 'NUC-' files in the directory, in the order adr_files gives, then the rule file
# AGENTS.md two levels up; nothing without a 'NUC-' file. git reads each file as it would store it, its line endings
# converted, so the list holds on every platform. Fails, saying so, when git cannot hash a file.
adr_blobs() {
  local names=() f blobs
  while IFS= read -r f; do [[ "${f##*/}" == NUC-* ]] && names+=("${f##*/}"); done < <(adr_files "$1")
  ((${#names[@]})) || return 0
  [[ -f "$1/../../AGENTS.md" ]] && names+=(../../AGENTS.md)
  blobs="$(cd "$1" && git hash-object -- "${names[@]}")" || { echo "adr_blobs: git hash-object failed in $1" >&2; return 1; }
  paste -d' ' <(printf '%s\n' "$blobs") <(printf '%s\n' "${names[@]##*/}")
}

# adr_is_file <name> — true when it names an ADR file.
adr_is_file() { [[ "${1##*/}" =~ ^$ADR_FILE_RE$ ]]; }

# adr_key_of <file> — sets ADR_ID to the identifier of the ADR file, its name without '.md'.
# adr_id prints it.
adr_key_of() { ADR_ID="${1##*/}" ADR_ID="${ADR_ID%.md}"; }
adr_id() { adr_key_of "$1"; printf '%s\n' "$ADR_ID"; }

# The sections of an ADR a session reads, in order, and the writing rule of AGENTS.md section 4 covers.
ADR_RULE_SECTIONS='Decision|Enforcement'
# ADR_FENCE_AWK — the awk function fenced(<line>), which reads fenced code as Markdown does: 2 on
# the line that opens a fence, three or more backticks or tildes, 1 on each line inside it and on
# the line that closes it, a run of the same character at least as long with nothing after it, and
# 0 on any other line. A '## ' line inside a fence is no heading. Every reader of an ADR's sections
# prepends it to its program, so they all end a section at the same line.
# shellcheck disable=SC2016 # the dollar signs are awk's
ADR_FENCE_AWK='
function fence_run(t, c,  n) { n = 0; while (substr(t, n + 1, 1) == c) n++; return n }
function fenced(s,  t, n) {
  t = s; sub(/^[[:space:]]*/, "", t)
  if (fence_c == "") {
    if (t !~ /^(```|~~~)/) return 0
    n = fence_run(t, substr(t, 1, 1))
    if (substr(t, 1, 1) == "`" && index(substr(t, n + 1), "`")) return 0
    fence_c = substr(t, 1, 1); fence_n = n
    return 2
  }
  n = fence_run(t, fence_c)
  if (n >= fence_n && substr(t, n + 1) ~ /^[[:space:]]*$/) fence_c = ""
  return 1
}
'

# adr_section <name> — the section '## <name>' of the ADR on stdin, its heading included. It ends
# at the next '## ' heading outside fenced code.
adr_section() {
  awk -v h="## $1" "$ADR_FENCE_AWK"'
    { f = fenced($0) } !f && $0 == h { p = 1; print; next } p && !f && /^## / { exit } p'
}

# adr_stray_fields <file> — each header field written in another form than the list item, as
# '<line number>:<line>'. The header ends at the first '## ' heading; the body may quote a field.
adr_stray_fields() {
  awk -v mark="$ADR_FIELD_MARK_RE" -v item="$ADR_ITEM_RE" '
    /^## / { exit }
    $0 ~ mark && $0 !~ item { print NR ":" $0 }
  ' "$1"
}
