#!/usr/bin/env bash
#
# Checks the supply chain with cargo-deny: advisories, licences, sources and banned crates, as
# deny.toml sets them, for the workspace and for the fuzz targets, which have a lockfile of their
# own (fuzz/Cargo.lock). The 'audit' job of .github/workflows/audit.yml runs it on every change
# and on a schedule, since an advisory can appear without one. scripts/check-all.sh names it on
# an '# On instruction:' line: it reads the advisory database from the network, and a new
# advisory is not the pushed change's defect.
#
# Needs cargo-deny (cargo install cargo-deny).
#
# Usage:
#     scripts/check-deny.sh
# Exit code 0 when both pass; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

command -v cargo-deny >/dev/null || { echo "cargo-deny is required: cargo install cargo-deny" >&2; exit 1; }

cargo deny check
cargo deny --manifest-path fuzz/Cargo.toml check
