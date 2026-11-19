#!/usr/bin/env bash
#
# Lints and tests the Client on the platform it runs on: the 'client-windows' and 'client-macos'
# jobs of .github/workflows/checks.yml run it on Windows and macOS. The Client carries
# platform-specific service paths, the Windows SCM shim and launchd's semantics, that a lint on
# Linux never compiles (CLT-installed-service-and-state). fleet-tools is linted too: every Client archive carries
# `opamp-fleetctl`, Windows included (OPS-operator-program-fleetctl). fleet-util is linted and tested with them: its
# no-follow file operations and unpackers have a Unix and a Windows path, and both the Client and
# the tool run them (DEV-six-crates-tokio-axum). On Linux, scripts/check-rust.sh lints and tests
# the Client for Linux, so this one lints it for Windows through cargo-xwin, as README.md → "Build,
# Test & Run" writes it, and a Windows-only compile error shows before a push rather than in CI.
# Without cargo-xwin or its target it says so and skips; no CI job runs it on Linux.
#
# Needs the Rust toolchain of rust-toolchain.toml; on Linux, cargo-xwin and the target
# x86_64-pc-windows-msvc as well.
#
# Usage:
#     scripts/check-client-platform.sh
# Exit code 0 when everything passes; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

if [[ "$(uname -s)" == Linux ]]; then
  if ! command -v cargo-xwin >/dev/null \
    || ! grep -qx x86_64-pc-windows-msvc <<< "$(rustup target list --installed 2>/dev/null)"; then
    echo "Skipped: linting the Client for Windows needs cargo-xwin and its target" \
      "(cargo install cargo-xwin; rustup target add x86_64-pc-windows-msvc)."
    exit 0
  fi
  cargo xwin clippy -p fleet-util -p fleet-agent -p fleet-tools --all-targets --locked \
    --target x86_64-pc-windows-msvc -- -D warnings
  exit 0
fi

cargo clippy -p fleet-util -p fleet-agent -p fleet-tools --all-targets --locked -- -D warnings
cargo test -p fleet-util -p fleet-agent --locked
