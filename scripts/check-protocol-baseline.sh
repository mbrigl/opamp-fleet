#!/usr/bin/env bash
#
# The Protocol Baseline check (API-protocol-pinned-interop-checked §4, specification G-13): the pinned upstream
# opamp-spec version all protocol code is written against must stay a deliberate choice, so the pin
# is compared with upstream's newest release and a divergence is reported rather than discovered.
#
# - A conformance document without the '<!-- protocol-baseline: vX.Y.Z -->' marker is an error.
# - A newer upstream release is a warning, never an error: upstream tagging a release says nothing
#   about this repository being wrong, and a check that reddened CI for it would be disabled.
# - Without curl or without network the check says so and skips, so the other checks stay usable
#   offline.
#
# The releases list's newest entry is read, not 'releases/latest': opamp-spec marks no release as
# latest, so that endpoint answers 404.
#
# Usage:
#     scripts/check-protocol-baseline.sh [--root <dir>] [conformance file]
# Prints 'Error: …', 'Warning: …' or 'Note: …' lines. Exit code 1 on an error, 0 otherwise, and 2
# on an argument it does not read or a root that is no directory.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/check-protocol-baseline.sh [--root <dir>] [conformance file]"
# A conformance file named by a relative path is read from where the check was started, which
# enter_root leaves.
started="$PWD"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
rest_args "${BASH_SOURCE[0]}" "$usage" 1 "${ARGS[@]}"
CONFORMANCE="${ARGS[0]:-$ROOT/docs/CONFORMANCE.md}"
[[ "$CONFORMANCE" == /* ]] || CONFORMANCE="$started/$CONFORMANCE"
RELEASES="https://api.github.com/repos/open-telemetry/opamp-spec/releases?per_page=1"

if [[ ! -f "$CONFORMANCE" ]]; then
  echo "Error: Protocol Baseline: $CONFORMANCE not found"
  exit 1
fi

# The pin lives in a machine-readable marker so this check never has to parse prose.
pinned="$(sed -nE 's/^<!--[[:space:]]*protocol-baseline:[[:space:]]*([^[:space:]]+)[[:space:]]*-->.*/\1/p' \
  "$CONFORMANCE" | head -n1)"
if [[ -z "$pinned" ]]; then
  echo "Error: $CONFORMANCE: no '<!-- protocol-baseline: vX.Y.Z -->' marker found"
  exit 1
fi

if ! command -v curl > /dev/null 2>&1; then
  echo "Note: curl unavailable — skipping the Protocol Baseline currency check."
  exit 0
fi

latest="$(curl -fsS --max-time 10 "$RELEASES" 2>/dev/null \
  | sed -nE 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' | head -n1)"
if [[ -z "$latest" ]]; then
  # Offline, rate-limited, or the API changed shape. Say so rather than passing silently — a check
  # that quietly does nothing is worse than one that admits it did nothing.
  echo "Note: could not reach the opamp-spec release API — skipping the Protocol Baseline currency check."
  exit 0
fi

if [[ "$pinned" != "$latest" ]]; then
  echo "Warning: Protocol Baseline is $pinned, but open-telemetry/opamp-spec has released $latest. Moving the Baseline is a deliberate change: review the upstream changelog, then update docs/CONFORMANCE.md and the code."
fi
exit 0
