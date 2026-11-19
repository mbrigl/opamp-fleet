#!/usr/bin/env bash
#
# Installs, starts, kills, restarts, stops, and uninstalls the Client as a real service on the
# platform it runs on, with the process account of CLT-accounts-service-runs-under and the enrolment of the install
# (SEC-bootstrap-certificates-and-enrolment §19); .github/workflows/service-smoke.yml says per platform what that asserts.
# The 'smoke' job runs it on Windows, Linux, and macOS. It registers services and accounts on the
# machine, so it runs on a throwaway one only: in a CI run (CI=true), or where
# FLEET_SMOKE_THROWAWAY=1 says the machine is one. Elsewhere it says so and skips.
#
# On Linux and macOS the test runs as root through `sudo -E`, the toolchain's PATH kept; on
# Windows the runner's shell is already elevated. On Linux two tests run as root besides: no link
# of root's beneath the Client's directories is followed (CLT-accounts-service-runs-under §21), and the second
# service follows the Client's links above the Supervisors' root, not its own (CLT-accounts-service-runs-under §20).
#
# Needs the Rust toolchain of rust-toolchain.toml, and passwordless sudo on Linux and macOS.
#
# Usage:
#     scripts/check-service-smoke.sh [--root <dir>]
# Exit code 0 when everything passes; the first failing command ends the run.
# It exits 2 on an argument it does not read or a root that is no directory.

set -euo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
# A macOS runner runs this under its bash 3.2, where an empty "${ARGS[@]}" under 'set -u' is an
# unbound variable: ${ARGS[@]+"${ARGS[@]}"} expands to nothing instead.
usage="scripts/check-service-smoke.sh [--root <dir>]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
rest_args "${BASH_SOURCE[0]}" "$usage" 0 ${ARGS[@]+"${ARGS[@]}"}

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
