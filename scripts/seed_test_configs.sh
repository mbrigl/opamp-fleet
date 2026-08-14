#!/usr/bin/env bash
#
# Seeds one minimal test Configuration (ADR-0012) per example supervisor from config/client.toml,
# each targeted by a Selector at the Agent that should receive it:
#
#   otelcol-contrib-conf  →  service.name = otelcol-contrib  (opampextension, self-reporting)
#   otelcol-conf          →  service.name = otelcol          (core otelcol, observed externally)
#
# Two modes:
#   scripts/seed_test_configs.sh [server-url]
#       PUTs each Configuration to a running Server's REST API
#       (default server-url: http://127.0.0.1:4320).
#   scripts/seed_test_configs.sh --offline [config-dir]
#       Writes each Configuration as <config-dir>/<name>.json — the Server's own persistence
#       format, loaded at its next start; no running Server needed. Default config-dir is
#       fleet-configs/ in the repository root (the server.toml default). This is what
#       scripts/install_tools.sh runs after installing the processes.
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
# After seeding: start the Server, uncomment the [[supervisor]] blocks in config/client.toml,
# and start the Client; each Agent then receives exactly its Configuration.

set -euo pipefail

examples="$(cd "$(dirname "$0")/../config/examples" && pwd)"

mode=put
if [ "${1:-}" = "--offline" ]; then
    mode=stage
    config_dir="${2:-$examples/../../fleet-configs}"
    mkdir -p "$config_dir"
else
    server="${1:-http://127.0.0.1:4320}"
fi

seed() {
    local name="$1" key="$2" value="$3" file="$4"
    if [ "$mode" = stage ]; then
        jq -Rs --arg name "$name" --arg key "$key" --arg value "$value" \
            '{name: $name, selector: {($key): $value}, body: .}' "$file" \
            >"$config_dir/$name.json"
        echo "staged $name.json (selector: $key = $value)"
    else
        jq -Rs --arg key "$key" --arg value "$value" '{selector: {($key): $value}, body: .}' "$file" |
            curl -fsS -X PUT -H 'Content-Type: application/json' -d @- \
                "$server/api/v1/configurations/$name" >/dev/null
        echo "PUT $name (selector: $key = $value)"
    fi
}

seed otelcol-contrib-conf service.name otelcol-contrib "$examples/otelcol-contrib-conf.yaml"
seed otelcol-conf service.name otelcol "$examples/otelcol-conf.yaml"

if [ "$mode" = stage ]; then
    echo "Done — the Server offers these Configurations from its next start."
else
    echo "Done — inspect with: curl $server/api/v1/configurations"
fi
