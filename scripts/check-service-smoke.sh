#!/usr/bin/env bash
#
# Installs, starts, kills, restarts, stops, and uninstalls the Client as a real service on the
# platform it runs on, with the process account of ADR-0059 and the enrolment of the install
# (ADR-0029 clause 11); .github/workflows/service-smoke.yml says per platform what that asserts.
# The 'smoke' job runs it on Windows, Linux, and macOS. It registers services and accounts on the
# machine, so scripts/check-all.sh names it on an '# On instruction:' line, for a throwaway
# machine only.
#
# On Linux and macOS the test runs as root through `sudo -E`, the toolchain's PATH kept; on
# Windows the runner's shell is already elevated. On Linux two tests run as root besides: no link
# of root's beneath the Client's directories is followed (ADR-0059 clause 6), and the second
# service follows the Client's links above the Supervisors' root, not its own (clause 5).
#
# Needs the Rust toolchain of rust-toolchain.toml, and passwordless sudo on Linux and macOS.
#
# Usage:
#     scripts/check-service-smoke.sh
# Exit code 0 when everything passes; the first failing command ends the run.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

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
