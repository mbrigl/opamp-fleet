#!/usr/bin/env bash
#
# Seeds one minimal test Configuration (ADR-0025) per example supervisor from config/supervisor.toml,
#
#
# Two modes:
#   scripts/seed_test_configs.sh [server-url]
#       (default server-url: http://127.0.0.1:4321).
#   scripts/seed_test_configs.sh --offline [config-dir]
#       Writes each Configuration as <config-dir>/<name>.json — the Server's own persistence
#       format, loaded at its next start; no running Server needed. Default config-dir is
#       fleet-configs/ in the repository root (the server.toml default). This is what
# Both modes replace an existing Configuration of the same name.
#
# Note on the contrib Collector: once its opampextension self-reports, the reported
# service.name (the dist.name it was built with, "otelcol-contrib") replaces the name derived
# from the binary's file name — which is also "otelcol-contrib", so the Selector matches before
# and after and live updates keep flowing. That equality is why the example supervisor carries
# that name; a supervisor whose reported type differs from its initial name should be tagged
# with a stable operator attribute ([supervisor.attributes]) and selected on that instead.
#
# The bodies live in config/examples/; install the processes with scripts/install_tools.sh.
# After seeding: start the Server, uncomment the [[supervisor]] blocks in config/supervisor.toml,
# and start the Client; each Agent then receives exactly its Configuration.

set -euo pipefail

examples="$(cd "$(dirname "$0")/../config/examples" && pwd)"

mode=put
if [ "${1:-}" = "--offline" ]; then
    mode=stage
    config_dir="${2:-$examples/../../fleet-configs}"
    mkdir -p "$config_dir"
else
    server="${1:-http://127.0.0.1:4321}"
fi

seed() {
    if [ "$mode" = stage ]; then
    else
    fi
}


if [ "$mode" = stage ]; then
else
    echo "Done — inspect with: curl $server/api/v1/configurations"
fi
