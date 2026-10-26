#!/usr/bin/env bash
#
# Lints and tests the Client on the platform it runs on: the 'client-windows' and 'client-macos'
# jobs of .github/workflows/ci.yml run it on Windows and macOS. The Client carries
# platform-specific service paths, the Windows SCM shim and launchd's semantics, that a lint on
# Linux never compiles (ADR-0020). fleet-tools is linted too: every Client archive carries
# `opamp-fleetctl`, Windows included (ADR-0033). On Linux, scripts/check-rust.sh covers the same
# and more, so scripts/check-all.sh names this one on an '# On instruction:' line.
#
# Needs the Rust toolchain of rust-toolchain.toml.
#
# Usage:
#     scripts/check-client-platform.sh
# Exit code 0 when everything passes; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

# Git Bash hands a Windows program its temporary directory by the 8.3 short name (RUNNER~1); the
# Windows tests compare the paths a walk reports with the ones they made, so they get the long one.
if command -v cygpath >/dev/null; then
  TMP="$(cygpath -w -l "${TMP:-$TEMP}")"
  TEMP="$TMP"
  export TMP TEMP
fi

cargo clippy -p fleet-agent -p fleet-tools --all-targets --locked -- -D warnings
cargo test -p fleet-agent --locked
