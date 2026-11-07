#!/usr/bin/env bash
#
# Runs the two tests that decide G-20 (docs/SPECIFICATION.md): three Server instances behind a
# layer-4 balancer that keeps the peer's address, on PostgreSQL and S3, one of them killed under
# 10 000 simulated Agents, and the presence and notifications of 100 000 simulated Agents for ten
# minutes. `crates/fleet-server/tests/g20.rs` holds both and says what they assert. The
# .github/workflows/g20.yml workflow starts the stores and runs this weekly and on demand.
#
# The stack is named by FLEET_G20_STACK=1 beside the FLEET_TEST_POSTGRES_* and FLEET_TEST_S3_*
# variables scripts/check-store.sh reads; the stores named alone do not run it, for it runs for
# about twenty minutes. Without it the check says so and skips, except in a CI run (CI=true), where
# it fails. Skipped is not passed: once the stack is named, a test that reports it skipped fails the
# run. The tests run in the release profile, as the Server ships.
#
# A shorter run on a smaller machine sets FLEET_G20_LOAD_AGENTS, FLEET_G20_AGENTS_PER_HOST or
# FLEET_G20_LOAD_SECS; the load test then says it did not decide G-20, and refuses them in CI.
#
# Usage:
#     scripts/check-g20.sh
# Exit code 0 when both tests passed against the stack, 1 otherwise.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ "${FLEET_G20_STACK:-}" != "1" ]]; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: no G-20 stack is named in the environment of a CI run (FLEET_G20_STACK=1)." >&2
    exit 1
  fi
  echo "Skipped: no G-20 stack is named in the environment (FLEET_G20_STACK=1)."
  exit 0
fi
command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

# A connection takes a descriptor in the simulator, two in the balancer and one in an instance.
wanted=1048576
if [[ "$(ulimit -Hn)" != unlimited && "$(ulimit -Hn)" -lt "$wanted" ]] && command -v sudo >/dev/null \
  && sudo -n true 2>/dev/null; then
  sudo -n prlimit --pid $$ --nofile="$wanted:$wanted" || true
fi
ulimit -n "$(ulimit -Hn)"
echo "File descriptors: $(ulimit -n)"

log="$(mktemp)"
trap 'rm -f "$log"' EXIT
echo "Building the tests and the Server (release)"
cargo test --release -p fleet-server --test g20 --locked --no-run
cargo test --release -p fleet-server --test g20 --locked -- --nocapture --test-threads=1 2>&1 | tee "$log"
if grep -q "skipped: no G-20 stack" "$log"; then
  echo "a G-20 test was skipped although the stack is named" >&2
  exit 1
fi
