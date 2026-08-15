#!/usr/bin/env bash
#
# Seeds one minimal test Configuration (ADR-0012) per example supervisor from config/client.toml,
# each aimed at the Agent that should receive it — the two Collectors by Selector, the two
# Foreign Agents by Agent type (ADR-0054), whose bodies are formats no other kind of Agent
# could read:
#
#   otelcol-contrib-conf  →  selector service.name = otelcol-contrib  (opampextension, self-reporting)
#   otelcol-conf          →  selector service.name = otelcol          (core otelcol, observed externally)
#   telegraf-conf         →  type telegraf                            (command Supervisor, SIGHUP reload)
#   glpi-agent-conf       →  type glpi-agent                          (command Supervisor, restart on apply)
#
# A Configuration's name is the file name its entry gets in the Supervisor's config directory,
# so each block must read exactly that path — "${config_dir}/telegraf-conf",
# "--conf-file=${config_dir}/glpi-agent-conf". Names carry no extension: they follow the
# ADR-0010 grammar (lowercase letters, digits and '-'), which admits no dot.
#
# Two modes:
#   scripts/seed_test_configs.sh [server-url]
#       PUTs each Configuration to a running Server's REST API and rolls it out — the act that
#       assigns it to the matching Agents (ADR-0061); a PUT alone reaches nobody.
#       (default server-url: http://127.0.0.1:4320).
#   scripts/seed_test_configs.sh --offline [config-dir]
#       Writes each Configuration as <config-dir>/<name>.json — the Server's own persistence
#       format, loaded at its next start; no running Server needed. Default config-dir is
#       fleet-configs/ in the repository root (the server.toml default). This is what
#       scripts/install_tools.sh runs after installing the processes. A staged Configuration is
#       stored, not assigned: under ADR-0061 only an Agent record that predates the ADR is
#       seeded from it, so on a fresh fleet roll each one out once the Agents have enrolled
#       (POST /api/v1/configurations/<name>/rollout, or the fleet view).
# Both modes replace an existing Configuration of the same name.
#
# Note on the contrib Collector: once its opampextension self-reports, the reported
# service.name (the dist.name it was built with, "otelcol-contrib") replaces the name derived
# from the binary's file name — which is also "otelcol-contrib", so the Selector matches before
# and after and live updates keep flowing. That equality is why the example supervisor carries
# that name; a supervisor whose reported type differs from its initial name should be tagged
# with a stable operator attribute ([supervisor.attributes]) and selected on that instead.
#
# Note on the two Foreign Agents: both refuse to start when the file their command line names
# does not exist, so a Supervisor started before the first rollout crash-loops three times and
# then holds. The rollout ends the hold; seeding first avoids the window altogether.
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

# seed <name> <file> <selector-json> [service_name]
#   selector-json  equality pairs as a JSON object; {} matches every Agent of the type below
#   service_name   the Agent type this Configuration is for (ADR-0054); omitted means every type
seed() {
    local name="$1" file="$2" selector="$3" type="${4:-}"
    local spec aimed_at
    spec=$(jq -Rs --argjson selector "$selector" --arg type "$type" \
        '{selector: $selector, body: .}
         + (if $type == "" then {} else {service_name: $type} end)' "$file")
    if [ -n "$type" ]; then
        aimed_at="type $type, selector $selector"
    else
        aimed_at="selector $selector"
    fi
    if [ "$mode" = stage ]; then
        jq --arg name "$name" '{name: $name} + .' <<<"$spec" >"$config_dir/$name.json"
        echo "staged $name.json ($aimed_at)"
    else
        curl -fsS -X PUT -H 'Content-Type: application/json' -d @- \
            "$server/api/v1/configurations/$name" <<<"$spec" >/dev/null
        curl -fsS -X POST "$server/api/v1/configurations/$name/rollout" >/dev/null
        echo "PUT and rolled out $name ($aimed_at)"
    fi
}

seed otelcol-contrib-conf "$examples/otelcol-contrib-conf.yaml" '{"service.name": "otelcol-contrib"}'
seed otelcol-conf "$examples/otelcol-conf.yaml" '{"service.name": "otelcol"}'
seed telegraf-conf "$examples/telegraf-conf.toml" '{}' telegraf
seed glpi-agent-conf "$examples/glpi-agent-conf.cfg" '{}' glpi-agent

if [ "$mode" = stage ]; then
    echo "Done — the Server holds these Configurations from its next start; roll them out to assign them."
else
    echo "Done — inspect with: curl $server/api/v1/configurations"
fi
