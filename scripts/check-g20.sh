#!/usr/bin/env bash
#
# Runs the two tests that decide G-20 (docs/SPECIFICATION.md): three Server instances behind a
# layer-4 balancer that keeps the peer's address, on PostgreSQL and S3, one of them killed under
# 10 000 simulated Agents, and the presence and notifications of 100 000 simulated Agents for ten
# minutes. `crates/fleet-server/tests/g20.rs` holds both and says what they assert. The
# .github/workflows/g20.yml workflow starts the stores and runs this on demand.
#
# The stack is named by FLEET_G20_STACK=1 beside the FLEET_TEST_POSTGRES_* and FLEET_TEST_S3_*
# variables scripts/check-store.sh reads; the stores named alone do not run it, for it runs for
# about half an hour. Without it the check says so and skips, except in a CI run (CI=true), where
# it fails. Skipped is not passed: once the stack is named, a test that reports it skipped fails the
# run. The tests run in the release profile, as the Server ships.
#
# A shorter run on a smaller machine sets FLEET_G20_LOAD_AGENTS, FLEET_G20_AGENTS_PER_HOST,
# FLEET_G20_LOAD_SECS or FLEET_G20_RAMP_PER_SEC; the load test then says it did not decide G-20,
# and refuses them in CI. The check ends by saying so, too.
#
# Usage:
#     scripts/check-g20.sh
# Exit code 0 when the tests passed against the stack, 1 otherwise. A run whose last line says
# "G-20 not decided" passed at a size other than G-20's and decides nothing.

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

# What G-20's sizes need, as measured: three instances holding 100 000 Agents reach about 12 GiB
# and grow under the load (docs/plan/fleet-at-scale-costs/05), the simulator and its balancer
# about 2 GiB, the stores beside them. A machine with less stops here, saying so, rather than
# failing in a way that reads as a verdict on G-20. A run at another size decides nothing and is
# not checked.
need_mib=$((24 * 1024))
need_cpus=8
if [[ -z "${FLEET_G20_LOAD_AGENTS:-}${FLEET_G20_AGENTS_PER_HOST:-}${FLEET_G20_LOAD_SECS:-}${FLEET_G20_RAMP_PER_SEC:-}" ]]; then
  have_mib="$(awk '/^MemAvailable:/ { print int($2 / 1024) }' /proc/meminfo)"
  have_cpus="$(nproc)"
  if (( have_mib < need_mib || have_cpus < need_cpus )); then
    echo "ERROR: G-20 needs ${need_mib} MiB of memory available and ${need_cpus} CPUs; this machine" \
      "has ${have_mib} MiB and ${have_cpus}. Its instances' memory is" \
      "docs/plan/fleet-at-scale-costs/05-an-instance-s-memory-follows-its-agents-not-its-age.md;" \
      "until it is settled, G-20 runs on a larger machine." >&2
    exit 1
  fi
  echo "Capacity: ${have_mib} MiB available, ${have_cpus} CPUs"
fi

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
# The note may follow the test's name on its line, as the harness prints them.
if note="$(grep -m1 -o "NOTE: .*does not decide G-20" "$log")"; then
  echo "G-20 not decided: ${note#NOTE: }"
fi
