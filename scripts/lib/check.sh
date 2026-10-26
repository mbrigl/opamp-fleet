# shellcheck shell=bash
# The scaffolding every check shares: its findings, collected so that one run reports all of
# them, and a membership test. Sourced by scripts/check-docs.sh, scripts/check-traceability.sh,
# and scripts/check-devcontainer.sh, each of which prints and counts the findings its own way.

errors=()

# add_error <message> — record one finding of the check.
add_error() { errors+=("$1"); }

# _contains <needle> <item...> — whether the needle is one of the items.
_contains() {
  local needle="$1"; shift
  local item
  for item in "$@"; do [[ "$item" == "$needle" ]] && return 0; done
  return 1
}
