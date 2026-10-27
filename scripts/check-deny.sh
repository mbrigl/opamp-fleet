#!/usr/bin/env bash
#
# Checks the supply chain with cargo-deny: advisories, licences, sources and banned crates, as
# deny.toml sets them, for the workspace and for the fuzz targets, which have a lockfile of their
# own (fuzz/Cargo.lock). The 'audit' job of .github/workflows/audit.yml runs it on every change
# and on a schedule, since an advisory can appear without one. Without cargo-deny the check says
# so and skips, except in a CI run (CI=true), where it fails.
#
# Needs cargo-deny (cargo install cargo-deny).
#
# Usage:
#     scripts/check-deny.sh
# Exit code 0 when both pass; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if ! command -v cargo-deny >/dev/null; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: cargo-deny not found in a CI run: cargo install cargo-deny" >&2
    exit 1
  fi
  echo "Skipped: cargo-deny is not installed (cargo install cargo-deny)."
  exit 0
fi

cargo deny check
cargo deny --manifest-path fuzz/Cargo.toml check
