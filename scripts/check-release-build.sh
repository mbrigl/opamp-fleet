#!/usr/bin/env bash
#
# Builds one shipped program in its release profile: the 'release-build' job of
# .github/workflows/ci.yml runs it for the Client on Linux, Windows, and macOS and for the Server
# on Linux, the platforms each ships on (docs/SPECIFICATION.md). It shows on every pull request
# that a release would build; the release itself builds in .github/workflows/release.yml.
# scripts/check-all.sh runs it for both programs on the local platform.
#
# Needs the Rust toolchain of rust-toolchain.toml.
#
# Usage:
#     scripts/check-release-build.sh [--root <dir>] fleet-agent|fleet-server
# Exit code 0 when the build succeeds, 2 on a wrong argument or a root that is no directory.

set -euo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
# A macOS runner runs this under its bash 3.2, where an empty "${ARGS[@]}" under 'set -u' is an
# unbound variable: ${ARGS[@]+"${ARGS[@]}"} expands to nothing instead.
usage="scripts/check-release-build.sh [--root <dir>] fleet-agent|fleet-server"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
set -- ${ARGS[@]+"${ARGS[@]}"}
rest_args "${BASH_SOURCE[0]}" "$usage" 1 ${ARGS[@]+"${ARGS[@]}"}

case "${1:-}" in
  fleet-agent | fleet-server) ;;
  *) echo "check-release-build.sh: name fleet-agent or fleet-server — usage: $usage" >&2; exit 2 ;;
esac
command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

cargo build -p "$1" --release --locked
