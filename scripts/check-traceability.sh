#!/usr/bin/env bash
#
# Traceability check for this repository (NUC-decisions-cite-tests): every accepted decision and
# every success criterion is verified by a test that cites it.
#
# What is checked:
#   1. Identifiers: every list item under '## Goals / Success Criteria' and '## Quality Goals' in
#      docs/SPECIFICATION.md starts with a bold identifier — **G-n** under Goals, **Q-n** under
#      Quality Goals — and no identifier appears twice.
#   2. Coverage: every 🟢 accepted ADR is cited by at least one marker. An ADR whose
#      '## Enforcement' section carries a paragraph starting '**Not mechanically decidable:**'
#      followed by a reason is exempt.
#   3. Citations: every marker names an identifier that exists, and no ADR that is ⚪ superseded
#      or 🔴 rejected.
#   4. Sensor self-tests: every scripts/sensor-<name>.sh has its self-test
#      scripts/test-sensor-<name>.sh, and that self-test carries a marker citing the ADR that
#      decides the checks (NUC-checks-are-bash §4), whichever else it cites.
#   5. Named tests: every name an accepted ADR's '## Enforcement' section gives in backticks that
#      reads as a test's, lower case words joined by at least three underscores, is defined at the
#      start of a line of a file that is not Markdown, as a function ('fn <name>') or a field
#      ('<name>:'); a comment defines nothing. A shorter name is not read.
#
# A criterion that no marker cites is pending work: the check lists it in its closing line and
# does not fail — nothing in the specification marks the state, so meeting a goal touches the
# tests, not the constitution.
#
# A marker is the word 'Verifies', a colon, and one or more identifiers separated by spaces or
# commas: an ADR's, such as NUC-base-is-fixed, G-1, or Q-2. A
# word of another form fails. Markers count in any file git does not ignore that is not Markdown,
# which is excluded so that the specification and the ADRs can describe markers without being
# counted as one.
# Whether the citing test asserts anything is review, not this check.
#
# 🟡 proposed ADRs are out of scope: their tests land while they are proposed, and the acceptance
# lands with them (NUC-decisions-cite-tests §3).
#
# Pure bash + coreutils/grep/sed, plus git to enumerate the repository's files. The file list
# comes from git — tracked plus new, not-yet-added files, minus everything .gitignore excludes.
#
# Usage:
#     scripts/check-traceability.sh [repository root]
# Exit code 0 when the check passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/bash.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/bash.sh"

# shellcheck source-path=SCRIPTDIR source=lib/adr.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/adr.sh"
# shellcheck source-path=SCRIPTDIR source=lib/check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/check.sh"

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)"
SPEC="docs/SPECIFICATION.md"
ADR_DIR="docs/adr"
ID_RE="$ADR_ID_RE"
# The ADR that decides the checks, which each self-test of a sensor cites: a NUC- identifier no
# normalization moves (NUC-decisions-are-proposed-then-accepted §8).
CHECKS_ADR="NUC-checks-are-bash"


# ---------------------------------------------------------------------------------------------
# 1. Criteria: identifiers read off the specification.

criteria=()   # every G-n / Q-n identifier, in order of appearance
section=""    # 'G' or 'Q' while inside one of the two sections, empty otherwise

if [[ ! -f "$ROOT/$SPEC" ]]; then
  add_error "$SPEC: not found — the criteria cannot be read"
else
  while IFS= read -r line || [[ -n "$line" ]]; do
    if [[ "$line" =~ ^##[[:space:]] ]]; then
      case "$line" in
        "## Goals / Success Criteria"*) section="G" ;;
        "## Quality Goals"*) section="Q" ;;
        *) section="" ;;
      esac
      continue
    fi
    [[ -n "$section" ]] || continue
    [[ "$line" =~ ^[[:space:]]*([-*+]|[0-9]+\.)[[:space:]]+(.*)$ ]] || continue

    body="${BASH_REMATCH[2]}"
    if [[ "$body" =~ ^\*\*([GQ])-([0-9]+)\*\* ]]; then
      kind="${BASH_REMATCH[1]}"
      id="$kind-${BASH_REMATCH[2]}"
      [[ "$kind" == "$section" ]] \
        || add_error "$SPEC: '$id' is listed under the wrong section — G-n belongs under Goals / Success Criteria, Q-n under Quality Goals"
      _contains "$id" "${criteria[@]}" \
        && add_error "$SPEC: identifier '$id' appears twice — identifiers are never reused"
      criteria+=("$id")
    else
      add_error "$SPEC: list item without a bold ${section}-n identifier: '${body:0:60}'"
    fi
  done < "$ROOT/$SPEC"
fi

# ---------------------------------------------------------------------------------------------
# 2. ADRs: which exist, which are accepted, which are inactive, which declare an exemption.

known_adrs=()
accepted=()
inactive=()
exempt=()
named_tests=()   # "<test> <ADR file>" for each test name an accepted ADR's Enforcement gives

while IFS= read -r f; do
  name="$(basename "$f")"
  id="$(adr_id "$f")"
  known_adrs+=("$id")
  status_line="$(adr_field_line "$f" Status)"
  case "$status_line" in
    *🟢*)
      accepted+=("$id")
      enforcement="$(sed -n '/^## Enforcement/,$p' "$f")"
      while IFS= read -r test; do
        named_tests+=("$test $ADR_DIR/$name")
      done < <(grep -oE "\`[a-z][a-z0-9]*(_[a-z0-9]+){3,}\`" <<< "$enforcement" | tr -d '`' | sort -u)
      if grep -qE '^\*\*Not mechanically decidable:\*\*' <<< "$enforcement"; then
        if grep -qE '^\*\*Not mechanically decidable:\*\*[[:space:]]*[^[:space:]]' <<< "$enforcement"; then
          exempt+=("$id")
        else
          add_error "$ADR_DIR/$name: 'Not mechanically decidable' declared without a reason — the paragraph has to say why no test can decide it"
        fi
      fi
      ;;
    *🔴* | *⚪*) inactive+=("$id") ;;
  esac
done < <(adr_files "$ROOT/$ADR_DIR")

# ---------------------------------------------------------------------------------------------
# 3. Markers: every citation in every non-Markdown text file.

cited=()   # identifiers cited, one entry per citation
declare -A cited_in=()   # file -> the identifiers its markers cite, each followed by a space

while IFS= read -r -d '' rel; do
  f="$ROOT/$rel"
  [[ -f "$f" ]] || continue
  case "$rel" in *.md) continue ;; esac
  grep -IqF 'Verifies:' "$f" || continue

  while IFS= read -r hit; do
    lineno="${hit%%:*}"
    mapfile -t ids < <(grep -oE "Verifies:[[:space:]]*($ID_RE)([[:space:],]+($ID_RE))*" <<< "${hit#*:}" \
                       | sed -E 's/^Verifies:[[:space:]]*//' | grep -oE "$ID_RE")
    for id in "${ids[@]}"; do
      cited+=("$id")
      cited_in[$rel]+="$id "
      if [[ "$id" =~ ^[GQ]-[0-9]+$ ]]; then
        _contains "$id" "${criteria[@]}" \
          || add_error "$rel:$lineno: cites '$id', which is not a criterion of $SPEC"
      elif [[ ! "$id" =~ ^$ADR_NEW_RE$ ]]; then
        add_error "$rel:$lineno: cites '$id', which is no identifier: <CAT>-<slug> of two to five words, G-n, or Q-n [traceability/citations:malformed]"
      elif ! _contains "$id" "${known_adrs[@]}"; then
        add_error "$rel:$lineno: cites '$id', which does not exist in $ADR_DIR/ [traceability/citations:missing]"
      elif _contains "$id" "${inactive[@]}"; then
        add_error "$rel:$lineno: cites '$id', which is superseded or rejected — a test cannot verify a decision no longer in force [traceability/citations:inactive]"
      fi
    done
  done < <(grep -nE "Verifies:[[:space:]]*($ID_RE)" "$f")
done < <(git -C "$ROOT" ls-files --cached --others --exclude-standard -z)

# ---------------------------------------------------------------------------------------------
# 4. Sensor self-tests: each sensor has one, and it cites the ADR that decides the checks.

while IFS= read -r -d '' rel; do
  [[ "$rel" =~ ^scripts/sensor-([^/]+)\.sh$ && -f "$ROOT/$rel" ]] || continue
  if [[ ! -f "$ROOT/$ADR_DIR/$CHECKS_ADR.md" ]]; then
    add_error "$CHECKS_ADR, which the self-test of each sensor cites, is not in $ADR_DIR/ [traceability/sensor-tests:adr]"; break
  fi
  self_test="scripts/test-sensor-${BASH_REMATCH[1]}.sh"
  if [[ ! -f "$ROOT/$self_test" ]]; then
    add_error "$rel has no self-test $self_test — each sensor runs after one that cites $CHECKS_ADR [traceability/sensor-tests:absent]"
  elif [[ " ${cited_in[$self_test]:-}" != *" $CHECKS_ADR "* ]]; then
    add_error "$self_test does not cite $CHECKS_ADR — the self-test of a sensor cites the ADR that decides the checks [traceability/sensor-tests:uncited]"
  fi
done < <(git -C "$ROOT" ls-files --cached --others --exclude-standard -z -- scripts)

# ---------------------------------------------------------------------------------------------
# 5. Named tests: each defined in a file that is not Markdown.

if ((${#named_tests[@]})); then
  defined="$(git -C "$ROOT" ls-files --cached --others --exclude-standard -z \
    | (cd "$ROOT" && grep -zv '\.md$' | xargs -0 -r grep -IhoE \
         '^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?(((async|const|unsafe)[[:space:]]+)*fn[[:space:]]+)?[a-z][a-z0-9_]*[[:space:]]*[(:<]' \
         2>/dev/null) \
    | sed -E 's/^[[:space:]]*(pub(\([a-z]+\))?[[:space:]]+)?(((async|const|unsafe)[[:space:]]+)*fn[[:space:]]+)?//; s/[[:space:]]*[(:<]$//' \
    | sort -u)"
  for entry in "${named_tests[@]}"; do
    grep -qxF "${entry%% *}" <<< "$defined" \
      || add_error "${entry#* }: its Enforcement names '${entry%% *}', which no file defines — name the test there is [traceability/named-tests:undefined]"
  done
fi

# ---------------------------------------------------------------------------------------------
# 6. Coverage: every accepted ADR is cited; criteria without a citation are listed.

for id in "${accepted[@]}"; do
  _contains "$id" "${exempt[@]}" && continue
  _contains "$id" "${cited[@]}" \
    || add_error "$id is accepted but no test cites it — add a 'Verifies' marker or declare it not mechanically decidable in its Enforcement section [traceability/coverage:uncited]"
done

uncovered=()
for id in "${criteria[@]}"; do
  _contains "$id" "${cited[@]}" || uncovered+=("$id")
done

# ---------------------------------------------------------------------------------------------

if ((${#errors[@]})); then
  echo "Traceability check FAILED:" >&2
  printf '  - %s\n' "${errors[@]}" >&2
  exit 1
fi

summary="${#accepted[@]} accepted ADRs, ${#criteria[@]} criteria, ${#cited[@]} citations"
if ((${#uncovered[@]})); then
  summary+="; ${#uncovered[@]} criteria without a test: ${uncovered[*]}"
fi
echo "Traceability check passed ($summary)."
exit 0
