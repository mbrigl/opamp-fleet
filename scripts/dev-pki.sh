#!/usr/bin/env bash
#
# Creates the certificates a development Server and Client need, since both refuse to run without
# them (ADR-0012, ADR-0022), with the Server's own `server pki init` (ADR-0029) — no openssl.
# Everything lands in one directory, gitignored, and is for a development machine only: the keys
# are unencrypted and the CAs are throwaway.
#
#   server/        what the Server host needs: its certificate, the client CA with its key, the
#                  bootstrap CA's certificate, and server.toml.fragment naming them
#   offline/       what stays with the operator: the server CA and the bootstrap CA with their
#                  keys, and the bootstrap pair every new host is given, named by
#                  supervisor.toml.fragment
#   server.toml / supervisor.toml
#                  a Server and a Client configuration that use the set, every other key at its
#                  default
#
# The development Client holds the bootstrap certificate and enrols like any other host: start the
# Server and the Client, then approve its request with `scripts/seed_test_configs.sh --enrol`.
# Trust the server CA, offline/server-ca.pem, in curl and in the browser.
#
# Usage:
#     scripts/dev-pki.sh [directory]      (default: .dev-pki in the repository root)
# Refuses to overwrite a directory that already holds a set.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/.dev-pki}"

if [[ -e "$OUT/server" || -e "$OUT/offline" ]]; then
  echo "$OUT already holds a set; remove the directory to start over" >&2
  exit 1
fi
mkdir -p "$OUT"
chmod 700 "$OUT"
OUT="$(cd "$OUT" && pwd)"  # the configurations name the set by absolute path

(cd "$ROOT" && cargo run -q -p fleet-server -- pki init \
  --server-dir "$OUT/server" --offline-dir "$OUT/offline" \
  --fleet "opamp-fleet development" --name localhost)

cp "$OUT/server/server.toml.fragment" "$OUT/server.toml"
{
  echo 'endpoint = "wss://127.0.0.1:4320/v1/opamp"'
  echo
  cat "$OUT/offline/supervisor.toml.fragment"
} >"$OUT/supervisor.toml"

cat <<EOF

Created in $OUT, with a server.toml and a supervisor.toml that use the set:

  cargo run -p fleet-server -- --config $OUT/server.toml
  cargo run -p fleet-agent -- --config $OUT/supervisor.toml
  scripts/seed_test_configs.sh --enrol     # approves the Client's enrolment
EOF
