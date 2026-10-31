#!/usr/bin/env bash
#
# Fuzz target check for this repository: the fuzz targets are one set in the three places that
# name them — the [[bin]] names of fuzz/Cargo.toml, the 'target' list of the matrix in
# .github/workflows/fuzz.yml, and the directories under fuzz/seeds/. scripts/check-fuzz.sh runs the
# targets of fuzz/Cargo.toml, CI runs those of its matrix, and each run reads the seeds of its
# target, so a target missing from one of them is fuzzed in one place and not in another, or
# starts without seeds. The rule is this check; no ADR holds it (specification Q-2 asks for the
# fuzzing itself).
#
# The matrix list is read in its block form, one '- name' per line under 'target:'. A repository
# without fuzz/Cargo.toml has nothing to check.
#
# Pure bash + coreutils/sed/awk. Usage:
#     scripts/check-fuzz-targets.sh [repository root]
# The root defaults to this repository; scripts/test-check-fuzz-targets.sh passes fixtures.
# Exit code 0 when the check passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/check.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/check.sh"

ROOT="$(cd "${1:-$(dirname "${BASH_SOURCE[0]}")/..}" && pwd)"
cargo="fuzz/Cargo.toml" workflow=".github/workflows/fuzz.yml" seeds="fuzz/seeds"

if [[ ! -f "$ROOT/$cargo" ]]; then
  echo "Fuzz target check passed (no $cargo)."
  exit 0
fi

mapfile -t in_cargo < <(
  sed -n '/^\[\[bin\]\]/,/^name/s/^name = "\(.*\)"/\1/p' "$ROOT/$cargo" | sort -u
)

in_matrix=()
if [[ -f "$ROOT/$workflow" ]]; then
  # The items under the first 'target:' key that follows 'matrix:', up to the first line indented
  # no deeper than that key.
  mapfile -t in_matrix < <(awk '
    /^[[:space:]]*matrix:[[:space:]]*$/ { matrix = 1; next }
    matrix && !list && /^[[:space:]]*target:[[:space:]]*$/ {
      list = 1; indent = match($0, /[^ ]/); next
    }
    list {
      if ($0 ~ /^[[:space:]]*$/) next
      if (match($0, /[^ ]/) <= indent) exit
      if ($0 ~ /^[[:space:]]*-[[:space:]]/) {
        item = $0
        sub(/^[[:space:]]*-[[:space:]]*/, "", item)
        sub(/[[:space:]]+#.*$/, "", item)
        gsub(/["'\''[:space:]]/, "", item)
        print item
      }
    }
  ' "$ROOT/$workflow" | sort -u)
  ((${#in_matrix[@]})) \
    || add_error "$workflow: no matrix 'target:' list found — one '- name' per line under it"
else
  add_error "$workflow: not found — CI fuzzes the targets of $cargo there"
fi

in_seeds=()
if [[ -d "$ROOT/$seeds" ]]; then
  for dir in "$ROOT/$seeds"/*/; do
    [[ -d "$dir" ]] && dir="${dir%/}" && in_seeds+=("${dir##*/}")
  done
fi

# The places that lack a name, once for each name any of them holds.
mapfile -t names < <(
  printf '%s\n' "${in_cargo[@]}" "${in_matrix[@]}" "${in_seeds[@]}" | sed '/^$/d' | sort -u
)
for name in "${names[@]}"; do
  _contains "$name" "${in_cargo[@]}" \
    || add_error "$name: no [[bin]] in $cargo, but named elsewhere — declare the target there"
  if ((${#in_matrix[@]})); then
    _contains "$name" "${in_matrix[@]}" \
      || add_error "$name: missing from the matrix of $workflow — CI does not fuzz it"
  fi
  _contains "$name" "${in_seeds[@]}" \
    || add_error "$name: no directory $seeds/$name — the target starts without seeds"
done

if ((${#errors[@]})); then
  echo "Fuzz target check FAILED:" >&2
  printf '  - %s\n' "${errors[@]}" >&2
  exit 1
fi

echo "Fuzz target check passed (${#names[@]} targets, the same in $cargo, $workflow and $seeds/)."
exit 0
