#!/usr/bin/env bash
#
# Sensor: action references.
#
# How the workflows reference the GitHub Actions they run is the project's decision, prepared by
# the catalogue entry 'Action references' of the propose-adr skill: a major version tag kept
# current by Dependabot, or a commit SHA kept current by a tool that rewrites the pins. This
# sensor decides neither. It reads every uncommented 'uses:' line under .github/workflows/,
# counts the forms, and reports what the reviewer of AGENTS.md, section 5, holds against the
# decision:
#
#   1. Mixed forms: major tags, full version tags, and commit SHAs side by side. One of them is
#      not what the project decided, or the decision has a list of trusted owners the reviewer
#      checks the mix against.
#   2. A reference that is no release: no ref at all, or a ref that is neither a version tag nor
#      a full commit SHA, such as a branch. It runs whatever the branch holds today.
#   3. A 'docker://' image without an explicit tag.
#
# With '--require major-tag' or '--require sha' it is the check of a decision the project took:
# every reference not in that form, and every finding of 2 and 3, fails with exit code 1. A
# major tag is kept current by Dependabot, so '--require major-tag' also fails when
# .github/dependabot.yml has no 'github-actions' entry. A SHA may be kept current by Dependabot
# or by pinact, so '--require sha' asks for neither. How a project wires the check is in the
# catalogue entry (.agents/skills/propose-adr/concepts/action-references.md).
#
# Local actions ('./...') are the repository's own code and are not counted. A version tag may
# carry a leading 'v' or not, so a short SHA of digits only reads as one. The workflows are read
# line by line, not as YAML: a 'uses:' line inside a 'run:' block is read too, and a flow
# mapping ('- { uses: ... }') is not. Without '--require' it reports and never fails: exit code 0
# always, except 2 when the root does not exist or the flag names no form.
#
# Pure bash + grep + sed. Usage:
#     scripts/sensor-action-refs.sh [--require major-tag|sha] [repository root]
# The root defaults to this repository; scripts/test-sensor-action-refs.sh passes fixtures.

set -uo pipefail

require=""
if [[ "${1:-}" == "--require" ]]; then
  require="${2:-}"
  shift 2 2>/dev/null || shift
  [[ "$require" == "major-tag" || "$require" == "sha" ]] \
    || { echo "Action references sensor: '--require' takes 'major-tag' or 'sha', not '$require'." >&2; exit 2; }
fi

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" 2>/dev/null && pwd)" \
  || { echo "Action references sensor: '${1:-}' is not a directory." >&2; exit 2; }
WORKFLOWS=".github/workflows"

shopt -s nullglob
workflows=("$ROOT/$WORKFLOWS"/*.yml "$ROOT/$WORKFLOWS"/*.yaml)
shopt -u nullglob

if ((${#workflows[@]} == 0)); then
  echo "Action references sensor: no workflow under $WORKFLOWS — nothing to report."
  exit 0
fi

major=0 full=0 sha=0
findings=()
excluded=()   # with '--require': references not in the required form
for wf in "${workflows[@]}"; do
  rel="${wf#"$ROOT"/}"
  while IFS= read -r hit; do
    n="${hit%%:*}"
    value="$(sed -E "s/^[^:]*:[[:space:]]*(- )?uses:[[:space:]]*//; s/[[:space:]]+#.*$//; s/^[\"']//; s/[\"']$//; s/[[:space:]]+$//" <<< "$hit")"
    case "$value" in
      ./*) ;;
      docker://*)
        # The tag sits on the last path segment; a registry port ('host:5000/image') is not one.
        image="${value#docker://}"
        [[ "${image##*/}" == *:* ]] || findings+=("$rel:$n: '$value' carries no tag")
        ;;
      *@*)
        ref="${value##*@}"
        if [[ "$ref" =~ ^v?[0-9]+$ ]]; then
          major=$((major + 1))
          [[ "$require" == "sha" ]] && excluded+=("$rel:$n: '$value' is on a major tag, not on a commit SHA")
        elif [[ "$ref" =~ ^v?[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
          full=$((full + 1))
          [[ -n "$require" ]] && excluded+=("$rel:$n: '$value' is on a full version tag, which the decision excludes")
        elif [[ "$ref" =~ ^[0-9a-f]{40}$ ]]; then
          sha=$((sha + 1))
          [[ "$require" == "major-tag" ]] && excluded+=("$rel:$n: '$value' is on a commit SHA, not on a major tag")
        else
          findings+=("$rel:$n: '$value' references '$ref', which is neither a version tag nor a commit SHA")
        fi
        ;;
      *) findings+=("$rel:$n: '$value' names no ref") ;;
    esac
  done < <(grep -nE '^[[:space:]]*(- )?uses:' "$wf")
done

forms=0
for count in "$major" "$full" "$sha"; do ((count > 0)) && forms=$((forms + 1)); done
summary="$major on a major tag, $full on a full version tag, $sha on a commit SHA"

if [[ -n "$require" ]]; then
  # Findings 2 and 3 run code no release names, whichever form was decided.
  excluded+=("${findings[@]}")
  if [[ "$require" == "major-tag" ]] \
    && ! grep -qE '^[[:space:]]*-?[[:space:]]*package-ecosystem:[[:space:]]*.?github-actions' "$ROOT/.github/dependabot.yml" 2>/dev/null; then
    excluded+=(".github/dependabot.yml: no 'github-actions' entry — a major tag without Dependabot never learns of the next major")
  fi
  if ((${#excluded[@]} == 0)); then
    echo "Action references check ($require): $summary — every reference is in the decided form."
    exit 0
  fi
  echo "Action references check ($require): ${#excluded[@]} reference(s) outside the decided form:" >&2
  printf '  - %s\n' "${excluded[@]}" >&2
  exit 1
fi

((forms > 1)) && findings=("mixed forms — $summary" "${findings[@]}")

if ((${#findings[@]} == 0)); then
  echo "Action references sensor: $summary — nothing to report."
  exit 0
fi
echo "Action references sensor: ${#findings[@]} finding(s) for the reviewer — each is held against the project's decision on action references:"
printf '  - %s\n' "${findings[@]}"
exit 0
