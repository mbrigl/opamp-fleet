# shellcheck shell=bash
# The header of an ADR, read one way by every script that reads it: a header field is the list
# item '- **<Name>:** …' that docs/adr/template.md writes (ADR-0001). check-docs.sh fails on a
# field written in any other form, so every other reader may take the list item for granted.
# The identifier of an ADR is read one way too: its number, or the draft identifier of an ADR that
# has not reached main, 'ADR-draft-<slug>' in the file 'draft-<slug>.md' (ADR-0002).
# Sourced by scripts/check-docs.sh, scripts/check-traceability.sh,
# scripts/sensor-binding-adrs.sh, and scripts/number-adrs.sh.

# The slug of a draft identifier: lower case letters and digits, joined by single hyphens.
ADR_SLUG_RE='[a-z0-9]+(-[a-z0-9]+)*'
# An ADR as it is cited: 'ADR-' and the key of its file, its number or 'draft-<slug>'.
ADR_REF_RE="ADR-([0-9]{4}|draft-$ADR_SLUG_RE)"
# An identifier a 'Verifies:' marker cites: an ADR, a goal, or a quality goal (ADR-0002).
# shellcheck disable=SC2034 # ADR_ID_RE is the sourcing script's
ADR_ID_RE="$ADR_REF_RE|[GQ]-[0-9]+"

# The header fields of docs/adr/template.md.
ADR_FIELDS='Status|Date|Deciders|Applies to|Supersedes'

# adr_field_line <file, or - for stdin> <name> — the first line of the header field, or nothing.
adr_field_line() { grep -m1 -E "^- \*\*$2:\*\*" "$1"; }

# adr_files <dir> — every ADR file in the directory, the numbered ones in order, then the drafts.
adr_files() {
  find "$1" -maxdepth 1 -type f \( -name '[0-9][0-9][0-9][0-9]-*.md' -o -name 'draft-*.md' \) \
    | LC_ALL=C sort
}

# adr_key <file> — what follows 'ADR-' in the identifier of the ADR file: its number, or
# 'draft-<slug>' for a draft.
adr_key() {
  local base="${1##*/}"
  case "$base" in
    draft-*) printf '%s\n' "${base%.md}" ;;
    *) printf '%s\n' "${base%%-*}" ;;
  esac
}

# adr_stray_fields <file> — each header field written in another form than the list item, as
# '<line number>:<line>'. The header ends at the first '## ' heading; the body may quote a field.
adr_stray_fields() {
  awk -v fields="$ADR_FIELDS" '
    /^## / { exit }
    $0 ~ "\\*\\*(" fields "):\\*\\*" && $0 !~ /^- \*\*/ { print NR ":" $0 }
  ' "$1"
}
