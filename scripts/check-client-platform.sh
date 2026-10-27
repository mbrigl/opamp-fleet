#!/usr/bin/env bash
#
# Lints and tests the Client on the platform it runs on: the 'client-windows' and 'client-macos'
# jobs of .github/workflows/checks.yml run it on Windows and macOS. The Client carries
# platform-specific service paths, the Windows SCM shim and launchd's semantics, that a lint on
# Linux never compiles (ADR-0019). fleet-tools is linted too: every Client archive carries
# `opamp-fleetctl`, Windows included (ADR-0032). On Linux, scripts/check-rust.sh covers the same
# and more, so there this one says so and stops.
#
# Needs the Rust toolchain of rust-toolchain.toml.
#
# Usage:
#     scripts/check-client-platform.sh
# Exit code 0 when everything passes; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ "$(uname -s)" == Linux ]]; then
  echo "Skipped: on Linux, scripts/check-rust.sh lints and tests the Client."
  exit 0
fi

command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

cargo clippy -p fleet-agent -p fleet-tools --all-targets --locked -- -D warnings
cargo test -p fleet-agent --locked
