#!/usr/bin/env bash
#
# Runs both ends against opamp-go, on both transports (ADR-0011): the Go harness under interop/
# is formatted and vetted first, the test is built next, so that a compile failure does not read
# as an interop failure, and then run. The 'interop' job of .github/workflows/interop.yml runs it
# on a schedule; interop/README.md says what it covers and how a red run is triaged.
# Without a Go toolchain, which nothing else here needs, the check says so and skips, except in
# a CI run (CI=true), where it fails.
#
# Needs the Rust toolchain of rust-toolchain.toml and Go as interop/go.mod names it.
#
# Usage:
#     scripts/check-interop.sh
# Exit code 0 when everything passes; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v go >/dev/null; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: go not found in a CI run (interop/go.mod names the version)." >&2
    exit 1
  fi
  echo "Skipped: no Go toolchain (interop/go.mod names the version)."
  exit 0
fi
command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

unformatted="$(cd interop && gofmt -l .)"
[[ -z "$unformatted" ]] || { echo "gofmt would change:" >&2; echo "$unformatted" >&2; exit 1; }
(cd interop && go vet ./...)

echo "Building the interop test"
cargo test -p fleet-agent --test interop_opamp_go --locked --no-run
echo "Running both ends against opamp-go"
cargo test -p fleet-agent --test interop_opamp_go --locked -- --ignored --nocapture --test-threads=1
