#!/usr/bin/env bash
#
# Manual check for this repository: no file under docs/manual/ cites an ADR, by an 'ADR-NNNN' or
# by a link or path into docs/adr/. The manual is for the people who run the software and says
# what to do; a decision record is for those who change it, and a citation sends an operator to a
# page that does not answer their question. The rule is this check; no ADR holds it.
#
# Every line is read, code fences included: a citation in an example is still one an operator
# follows. A link into the record is any '../adr/' or 'docs/adr/' on a line, so an inline, a
# reference-style, and an angle-bracket link count alike. A repository without docs/manual/ has
# nothing to check.
#
# Pure bash + coreutils/grep/find. Usage:
#     scripts/check-manual.sh [repository root]
# The root defaults to this repository; scripts/test-check-manual.sh passes fixtures.
# Exit code 0 when the check passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/check.sh"

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)"
dir="$ROOT/docs/manual"

if [[ ! -d "$dir" ]]; then
  echo "Manual check passed (no docs/manual/)."
  exit 0
fi

files=0
while IFS= read -r file; do
  files=$((files + 1))
  while IFS= read -r hit; do
    add_error "${file#"$ROOT"/}:${hit%%:*}: cites an ADR — the manual says what to do and why it matters to an operator; the decision record stays in docs/adr/"
  done < <(grep -nE 'ADR-[0-9]{4}|\.\./adr/|docs/adr/' "$file")
done < <(find "$dir" -type f -name '*.md' | LC_ALL=C sort)

if ((${#errors[@]})); then
  echo "Manual check FAILED:" >&2
  printf '  - %s\n' "${errors[@]}" >&2
  exit 1
fi

echo "Manual check passed ($files files, no ADR cited)."
exit 0
