#!/usr/bin/env bash
#
# Builds, tests and lints the Cargo workspace: the commands of README.md → "Build, Test & Run",
# with the lockfile held as it is (ADR-0058 clause 5). scripts/check-all.sh runs it as the local
# gate, and the 'rust' job of .github/workflows/checks.yml runs it in CI, so the build and the
# tests are part of what a push and a merge wait for (AGENTS.md §5).
#
# The opamp crate is linted once per feature as well: inside the workspace Cargo builds it with
# every feature any crate asks for, so only a build of each feature on its own shows that it
# stands alone (ADR-0057).
#
# Needs the Rust toolchain of rust-toolchain.toml; the Dev Container provides it.
#
# Usage:
#     scripts/check-rust.sh
# Exit code 0 when everything passes; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

cargo build --workspace --locked
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
for feature in "" client server; do
  cargo clippy -p opamp --all-targets --locked --no-default-features --features "$feature" -- -D warnings
done
