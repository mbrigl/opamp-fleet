#!/usr/bin/env bash
#
# Seeds one minimal test Configuration (ADR-0012) per example supervisor from config/client.toml,
# each targeted by a Selector at the Agent that should receive it:
#
#   otelcol-conf        →  service.name = otelcol        (otelcol-contrib, opampextension)
#   otelcol-plain-conf  →  service.name = otelcol-plain  (core otelcol, observed externally)
#   fluent-bit-conf     →  service.name = fluent-bit     (Foreign Agent, reads the entry file)
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
# Caveat for the contrib Collector: once its opampextension self-reports, the reported
# service.name ("otelcol-contrib") replaces the supervisor's name in the Agent's description, so
# the otelcol-conf Selector stops matching — the first configuration is delivered (that is what
# starts the Collector), but a later update is only offered again after a Client restart. For
# live updates, tag the supervisor with a stable operator attribute in client.toml
# ([supervisor.attributes], e.g. role = "otelcol") and select on that instead.
#
# The bodies live in config/examples/; install the processes with scripts/install_tools.sh.
# After seeding: start the Server, uncomment the [[supervisor]] blocks in config/client.toml,
# and start the Client; each Agent then receives exactly its Configuration. For a repo-local
# run, point the fluent-bit supervisor's -c argument at the entry file under the local
# state_dir: client-state/supervisors/fluent-bit/config/fluent-bit-conf

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

seed otelcol-conf service.name otelcol "$examples/otelcol-conf.yaml"
seed otelcol-plain-conf service.name otelcol-plain "$examples/otelcol-plain-conf.yaml"
seed fluent-bit-conf service.name fluent-bit "$examples/fluent-bit-conf.conf"

if [ "$mode" = stage ]; then
    echo "Done — the Server offers these Configurations from its next start."
else
    echo "Done — inspect with: curl $server/api/v1/configurations"
fi
