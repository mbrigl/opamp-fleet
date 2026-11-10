#!/usr/bin/env bash
#
# Builds, tests and lints the Cargo workspace: the commands of README.md → "Build, Test & Run",
# with the lockfile held as it is (ADR-0008 clause 15). scripts/check-all.sh runs it as the local
# gate, and the 'rust' job of .github/workflows/checks.yml runs it in CI, so the build and the
# tests are part of what a push and a merge wait for (AGENTS.md §5).
#
# The opamp crate is linted once per feature as well: inside the workspace Cargo builds it with
# every feature any crate asks for, so only a build of each feature on its own shows that it
# stands alone (ADR-0008).
#
# fleet-agent is linted without any wrapped Supervisor kind and with each one alone, and tested
# without any: the default build carries every kind, so only these show that a build leaving one
# out still compiles and refuses the kind as unknown (ADR-0018 clause 2).
#
# Needs the Rust toolchain of rust-toolchain.toml, and SoftHSM2 for the tests of the keys on a
# token; the Dev Container provides both.
#
# Usage:
#     scripts/check-rust.sh
# Exit code 0 when everything passes; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

# The tests of the keys on a PKCS#11 token run against SoftHSM2 where it is installed, a token
# directory of their own in a temporary directory; a CI run without it fails, and so does a run
# that names a module and still skips one of them.
# shellcheck source-path=SCRIPTDIR source=lib/softhsm.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/softhsm.sh"
softhsm_env
log="$(mktemp)"
trap 'rm -rf "$log" ${SOFTHSM_DIR:+"$SOFTHSM_DIR"}' EXIT

cargo build --workspace --locked
cargo test --workspace --locked
# The test harness keeps a passing test's output to itself, so the tests of the keys on a token
# run again with theirs shown, for the skip they would say aloud.
if [[ -n "${FLEET_TEST_PKCS11_MODULE:-}" ]]; then
  cargo test -p fleet-server --test pkcs11 --locked -- --nocapture 2>&1 | tee "$log"
  cargo test -p fleet-tools --locked -- --nocapture token 2>&1 | tee -a "$log"
  if softhsm_skipped "$log"; then
    exit 1
  fi
fi
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
for feature in "" client server; do
  cargo clippy -p opamp --all-targets --locked --no-default-features --features "$feature" -- -D warnings
done
# The wrapped kinds are the features of fleet-agent's [features] table other than `default`, read
# from there so that a kind adds its feature and nothing here.
mapfile -t kinds < <(sed -n '/^\[features\]/,/^\[/{/^[a-z0-9_-]* *=/p;}' crates/fleet-agent/Cargo.toml \
  | cut -d= -f1 | tr -d ' ' | grep -vx default || true)
for feature in "" "${kinds[@]}"; do
  cargo clippy -p fleet-agent --all-targets --locked --no-default-features --features "$feature" -- -D warnings
done
cargo test -p fleet-agent --locked --no-default-features
