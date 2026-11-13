#!/usr/bin/env bash
#
# Runs the tests of one of the Server's external stores against a real one: 's3' against an
# S3-compatible object store (ADR-0023), 'postgres' against PostgreSQL over TLS (ADR-0023). A
# plain `cargo test` skips them; the 's3-store' and 'postgres-store' jobs of
# .github/workflows/checks.yml start the store and run this. Locally, the development stack of
# observability/compose.yaml provides both; without a store named the check says so and skips,
# except in a CI run (CI=true), where it fails.
#
# The store is named through the environment the tests read: FLEET_TEST_S3_ENDPOINT,
# FLEET_TEST_S3_BUCKET, FLEET_TEST_S3_ACCESS_KEY_ID, and FLEET_TEST_S3_SECRET_ACCESS_KEY_FILE, or
# FLEET_TEST_POSTGRES_HOST, _PORT, _DBNAME, _USER, _PASSWORD_FILE, and _CA_FILE; 'postgres' needs a
# bucket named too, for a Server on both stores, and runs the whole library, whose tests outside
# the store's module open the database too. FLEET_TEST_SERVER_IMAGE names the Server's image for
# the test that runs it read-only on both stores; it skips without one, except in a CI run. Skipped
# is not passed: once a store is named, a test that reports it skipped fails the run.
#
# Usage:
#     scripts/check-store.sh s3|postgres
# Exit code 0 when every test of the store passed against it, 1 otherwise, 2 on a wrong argument.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

case "${1:-}" in
  s3) filter="s3::" skipped="skipped: no S3 store" named="${FLEET_TEST_S3_ENDPOINT:-}" ;;
  postgres) filter="" skipped="skipped: no PostgreSQL" named="${FLEET_TEST_POSTGRES_HOST:-}" ;;
  *) echo "usage: scripts/check-store.sh s3|postgres" >&2; exit 2 ;;
esac
if [[ -z "$named" ]]; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: no $1 store is named in the environment of a CI run." >&2
    exit 1
  fi
  echo "Skipped: no $1 store is named in the environment."
  exit 0
fi

# Before the tests, the check probes each named store at its own protocol. A TCP connect is not
# enough: the Dev Container's forwards (.devcontainer/forward-otlp.sh) accept a connection while
# the host's stack is down and close it at once, and the tests then fail as a code error would.
# PostgreSQL answers an SSLRequest with one byte, 'S' or 'N'. An S3 endpoint answers an HTTP
# request with a status line. Bash's /dev/tcp makes the probe, so it needs neither psql nor curl;
# it cannot speak TLS, so an https endpoint is not probed. 'postgres' probes its bucket's endpoint
# too.
#
# answers <host> <port> <request> <count> <answer pattern> — whether the store at <host>:<port>
# answers the request with <count> bytes that match the pattern, within 10 seconds.
answers() {
  local answer
  # shellcheck disable=SC2016 # the inner shell expands its own arguments
  answer="$(timeout 10 bash -c '
    exec 3<>"/dev/tcp/$1/$2" || exit 1
    printf "$3" >&3
    IFS= read -r -N "$4" -t 5 -u 3 answer
    printf "%s" "$answer"' _ "$1" "$2" "$3" "$4" 2>/dev/null)"
  # shellcheck disable=SC2053 # the pattern is the argument
  [[ "$answer" == $5 ]]
}
# probe <host> <port> <request> <count> <answer pattern> — fail when the store does not answer.
probe() {
  answers "$@" && return 0
  echo "ERROR: the store at $1:$2 does not answer: nothing listens there, or a listener closes the" >&2
  echo "connection at once, as a forward to a store that is down does. Start the store first." >&2
  exit 1
}
# probe_s3 <endpoint> — probe an http:// endpoint; the port defaults to 80.
probe_s3() {
  local authority host port
  [[ "$1" == http://* ]] || return 0
  authority="${1#http://}"
  authority="${authority%%/*}"
  host="${authority%:*}"
  port="${authority##*:}"
  if [[ "$host" == "$authority" || "$port" == *]* ]]; then
    host="$authority" port=80
  fi
  host="${host#[}"
  host="${host%]}"
  probe "$host" "$port" "HEAD / HTTP/1.0\r\nHost: $authority\r\n\r\n" 5 'HTTP/'
}
if [[ "$1" == "postgres" ]]; then
  probe "$FLEET_TEST_POSTGRES_HOST" "${FLEET_TEST_POSTGRES_PORT:-5432}" \
    '\000\000\000\010\004\322\026\057' 1 '[SN]'
fi
if [[ -n "${FLEET_TEST_S3_ENDPOINT:-}" ]]; then
  probe_s3 "$FLEET_TEST_S3_ENDPOINT"
fi

command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

# The tests of several instances on one token run against SoftHSM2, as scripts/check-rust.sh
# runs the others.
# shellcheck source-path=SCRIPTDIR source=lib/softhsm.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/softhsm.sh"
SOFTHSM_DIR=""
if [[ "$1" == "postgres" ]]; then
  softhsm_env
fi
log="$(mktemp)"
trap 'rm -rf "$log" ${SOFTHSM_DIR:+"$SOFTHSM_DIR"}' EXIT
cargo test -p fleet-server --lib --locked "$filter" -- --nocapture 2>&1 | tee "$log"
# Several Server instances on one database, each through its real listeners, and a Server on
# PostgreSQL and S3 that keeps nothing on its disk: integration tests of their own, against the same
# database, the latter with the bucket the FLEET_TEST_S3_* variables name too.
if [[ "$1" == "postgres" ]]; then
  cargo test -p fleet-server --locked --test instances -- --nocapture 2>&1 | tee -a "$log"
  cargo test -p fleet-server --locked --test stateless_instance -- --nocapture 2>&1 | tee -a "$log"
  if grep -q "skipped: no S3 store" "$log"; then
    echo "the test of a Server on PostgreSQL and S3 was skipped: name a bucket too" >&2
    exit 1
  fi
  # The image run read-only needs an image built and a container runtime, which a CI run has.
  if [[ "${CI:-}" == "true" ]] && grep -q "skipped: no Server image" "$log"; then
    echo "the test of the Server's image was skipped in a CI run: name FLEET_TEST_SERVER_IMAGE" >&2
    exit 1
  fi
fi
if softhsm_skipped "$log"; then
  exit 1
fi
if grep -q "$skipped" "$log"; then
  echo "a $1 test was skipped although a store is named" >&2
  exit 1
fi
