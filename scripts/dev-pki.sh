#!/usr/bin/env bash
#
# Creates the certificates a development Server and Client need, now that both refuse to run
# without them (ADR-0023, ADR-0026). Everything lands in one directory, gitignored, and is for a
# development machine only: the keys are unencrypted and the CAs are throwaway.
#
#   ca.pem / ca-key.pem                 the development CA: it signs the Server's certificate, and it
#                                       is the client CA that issues and verifies Agent certificates
#                                       ([tls] client_ca_file, [client_ca])
#   server.pem / server-key.pem         the Server's certificate, for 127.0.0.1, ::1 and localhost
#   client.pem / client-key.pem         an Agent certificate from the client CA ([tls] in
#                                       supervisor.toml), for a Client that needs no enrolment
#   bootstrap-ca.pem / bootstrap-ca-key.pem
#                                       the bootstrap CA ([enrolment] bootstrap_ca_file)
#   bootstrap.pem / bootstrap-key.pem   a bootstrap certificate, which can only enrol
#   server.toml / supervisor.toml       a Server and a Client configuration that use the set, every
#                                       other key at its default
#
# Requires the openssl command-line tool.
#
# Usage:
#     scripts/dev-pki.sh [directory]      (default: .dev-pki in the repository root)
# Refuses to overwrite a directory that already holds a CA.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/.dev-pki}"

if [[ -e "$OUT/ca.pem" ]]; then
  echo "$OUT already holds a CA; remove the directory to start over" >&2
  exit 1
fi
command -v openssl >/dev/null || { echo "openssl is required" >&2; exit 1; }
mkdir -p "$OUT"
chmod 700 "$OUT"
cd "$OUT"
OUT="$(pwd)"  # the configurations name the set by absolute path

# new_key <file> — a P-256 key, readable by its owner only.
new_key() {
  openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out "$1" 2>/dev/null
  chmod 600 "$1"
}

# new_ca <name> <common name>
new_ca() {
  new_key "$1-key.pem"
  openssl req -x509 -new -key "$1-key.pem" -subj "/CN=$2" -days 365 -sha256 \
    -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" \
    -out "$1.pem"
}

# issue <name> <common name> <issuer> <extendedKeyUsage> [subjectAltName]
issue() {
  local extensions
  extensions="$(mktemp)"
  {
    echo "basicConstraints=critical,CA:FALSE"
    echo "keyUsage=critical,digitalSignature"
    echo "extendedKeyUsage=$4"
    [[ -n "${5:-}" ]] && echo "subjectAltName=$5"
  } >"$extensions"
  new_key "$1-key.pem"
  openssl req -new -key "$1-key.pem" -subj "/CN=$2" -out "$1.csr"
  openssl x509 -req -in "$1.csr" -CA "$3.pem" -CAkey "$3-key.pem" -CAcreateserial \
    -days 90 -sha256 -extfile "$extensions" -out "$1.pem" 2>/dev/null
  rm -f "$1.csr" "$extensions"
}

new_ca ca "opamp-fleet development CA"
new_ca bootstrap-ca "opamp-fleet development bootstrap CA"
issue server "opamp-fleet development server" ca serverAuth "IP:127.0.0.1,IP:::1,DNS:localhost"
issue client "opamp-fleet development agent" ca clientAuth
issue bootstrap "opamp-fleet development bootstrap" bootstrap-ca clientAuth
rm -f ./*.srl

cat >server.toml <<EOF
[tls]
cert_file = "$OUT/server.pem"
key_file = "$OUT/server-key.pem"
client_ca_file = "$OUT/ca.pem"

[client_ca]
cert_file = "$OUT/ca.pem"
key_file = "$OUT/ca-key.pem"

[enrolment]
bootstrap_ca_file = "$OUT/bootstrap-ca.pem"
EOF

cat >supervisor.toml <<EOF
endpoint = "wss://127.0.0.1:4320/v1/opamp"

[tls]
ca_file = "$OUT/ca.pem"
cert_file = "$OUT/client.pem"
key_file = "$OUT/client-key.pem"
EOF

cat <<EOF
Created in $OUT, with a server.toml and a supervisor.toml that use the set:

  cargo run -p fleet-server -- --config $OUT/server.toml
  cargo run -p fleet-agent -- --config $OUT/supervisor.toml

To try enrolment, point [tls] in supervisor.toml at bootstrap.pem and bootstrap-key.pem.
EOF
