#!/usr/bin/env bash
#
# Installs, starts, kills, restarts, stops, and uninstalls the Client as a real service on the
# platform it runs on, with the process account of ADR-0020 and the enrolment of the install
# (ADR-0022 clause 19); .github/workflows/service-smoke.yml says per platform what that asserts.
# The 'smoke' job runs it on Windows, Linux, and macOS. It registers services and accounts on the
# machine, so it runs on a throwaway one only: in a CI run (CI=true), or where
# FLEET_SMOKE_THROWAWAY=1 says the machine is one. Elsewhere it says so and skips.
#
# On Linux and macOS the test runs as root through `sudo -E`, the toolchain's PATH kept; on
# Windows the runner's shell is already elevated. On Linux two tests run as root besides: no link
# of root's beneath the Client's directories is followed (ADR-0020 clause 21), and the second
# service follows the Client's links above the Supervisors' root, not its own (ADR-0020 clause 20).
#
# Needs the Rust toolchain of rust-toolchain.toml, and passwordless sudo on Linux and macOS.
#
# Usage:
#     scripts/check-service-smoke.sh
# Exit code 0 when everything passes; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

if [[ "${CI:-}" != "true" && "${FLEET_SMOKE_THROWAWAY:-}" != "1" ]]; then
  echo "Skipped: it registers services and accounts; set FLEET_SMOKE_THROWAWAY=1 on a throwaway machine."
  exit 0
fi

command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

as_root=(sudo -E env "PATH=$PATH")
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) os=windows as_root=() ;;
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) echo "check-service-smoke: no service smoke test for $(uname -s)" >&2; exit 1 ;;
esac

echo "Building the smoke test"
cargo test -p fleet-agent --test service_smoke --locked --no-run
echo "Install, start, kill, restart, stop, uninstall ($os)"
"${as_root[@]}" cargo test -p fleet-agent --test service_smoke --locked -- --ignored --nocapture --test-threads=1
if [[ "$os" == linux ]]; then
  "${as_root[@]}" cargo test -p fleet-agent --lib --locked -- --exact \
    storage::tests::no_link_beneath_the_root_is_followed_whoever_owns_it
  "${as_root[@]}" cargo test -p fleet-agent --lib --locked -- --exact \
    supervisor::channel::tests::a_purge_follows_the_clients_links_above_the_root_and_not_the_services
fi
