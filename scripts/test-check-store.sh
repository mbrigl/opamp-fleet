#!/usr/bin/env bash
#
# Self-test of scripts/check-store.sh, its probe of the named store in particular.
#
# Each case starts a fixture listener on the loopback and names it as the store: one that accepts
# a connection and closes it at once, as a forward to a store that is down does, or one that
# answers the probe as PostgreSQL or an S3 endpoint does. A stub `cargo` first on PATH stands in
# for the store's tests and prints a marker, so a case sees whether the check went on to them.
# The cases assert the exit code, the message, and the marker. Nothing reaches the network.
#
# Bash opens no listener, so the fixtures are a few lines of python3, which the Dev Container and
# the GitHub-hosted runner image ship. Without python3 the self-test says so and skips, except in
# a CI run (CI=true), where it fails.
#
# Inputs: scripts/check-store.sh scripts/lib/
#
# Usage:
#     scripts/test-check-store.sh
# Exit code 0 when every case passes, 1 otherwise.

set -uo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/selftest.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/selftest.sh"

CHECK="$ROOT/scripts/check-store.sh"

if ! command -v python3 >/dev/null 2>&1; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: python3 not found in a CI run: the store check's self-test needs it for its fixtures." >&2
    exit 1
  fi
  echo "Note: python3 unavailable, so the store check's self-test is skipped."
  exit 0
fi

listeners=()
trap '((${#listeners[@]})) && kill "${listeners[@]}" 2>/dev/null; rm -rf "$work"' EXIT

# The fixture listener: it prints its port, then accepts each connection, reads what the client
# sends, writes the reply given in hex, and closes. With no reply it closes at once, unread.
LISTENER='
import socket, sys
reply = bytes.fromhex(sys.argv[1])
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(8)
print(s.getsockname()[1], flush=True)
while True:
    c, _ = s.accept()
    if reply:
        c.recv(4096)
        c.sendall(reply)
    c.close()
'

# listen <name> <reply in hex, empty for none> — start a fixture listener, its port in $port.
listen() {
  local file="$work/$1.port" _
  python3 -I -c "$LISTENER" "$2" > "$file" &
  listeners+=("$!")
  for _ in $(seq 1 50); do
    [[ -s "$file" ]] && break
    sleep 0.1
  done
  [[ -s "$file" ]] || { echo "listen: the fixture listener $1 did not start" >&2; exit 1; }
  port="$(<"$file")"
}

# hex <printf format> — the bytes the format prints, in hex.
hex() {
  # shellcheck disable=SC2059 # the format is the argument
  printf "$1" | od -An -tx1 | tr -d ' \n'
}

stubs="$work/bin"
mkdir -p "$stubs"
printf '#!/bin/sh\necho "cargo stub ran"\n' > "$stubs/cargo"
chmod +x "$stubs/cargo"

# expect <pass|fail> <name> <store> [needle] [VAR=value...] — run the check on the store in an
# environment that names only the variables given; a failing case must name the needle and must
# not reach the tests, a passing one must reach them.
expect() {
  local want="$1" name="$2" store="$3" needle="${4:-}" out rc ok=0
  shift 4
  out="$(env -i PATH="$stubs:$PATH" HOME="$work" FLEET_TEST_PKCS11_MODULE=stub "$@" \
    bash "$CHECK" "$store" 2>&1)"; rc=$?
  if [[ "$want" == pass ]]; then
    ((rc == 0)) && [[ "$out" == *"cargo stub ran"* ]] && ok=1
  else
    ((rc == 1)) && [[ "$out" == *"$needle"* && "$out" != *"cargo stub ran"* ]] && ok=1
  fi
  check "$name" test "$ok" -eq 1
  ((ok)) || while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out" >&2
}

listen dead ''; dead="$port"
# PostgreSQL answers an SSLRequest with one byte: 'S' to go on in TLS, 'N' to refuse it.
listen postgres "$(hex S)"; postgres="$port"
listen s3 "$(hex 'HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n')"; s3="$port"
# A port nothing listens on: a fixture's, stopped.
listen gone ''; gone="$port"
kill "${listeners[-1]}"
wait "${listeners[-1]}" 2>/dev/null

expect fail "a PostgreSQL forward that accepts and closes" postgres \
  "the store at 127.0.0.1:$dead does not answer" \
  FLEET_TEST_POSTGRES_HOST=127.0.0.1 FLEET_TEST_POSTGRES_PORT="$dead"
expect fail "a PostgreSQL port nothing listens on" postgres \
  "the store at 127.0.0.1:$gone does not answer" \
  FLEET_TEST_POSTGRES_HOST=127.0.0.1 FLEET_TEST_POSTGRES_PORT="$gone"
expect fail "an S3 forward that accepts and closes" s3 \
  "the store at 127.0.0.1:$dead does not answer" \
  FLEET_TEST_S3_ENDPOINT="http://127.0.0.1:$dead"
expect fail "a PostgreSQL that answers, beside an S3 forward that accepts and closes" postgres \
  "the store at 127.0.0.1:$dead does not answer" \
  FLEET_TEST_POSTGRES_HOST=127.0.0.1 FLEET_TEST_POSTGRES_PORT="$postgres" \
  FLEET_TEST_S3_ENDPOINT="http://127.0.0.1:$dead"
expect fail "an S3 endpoint that answers with no status line" s3 \
  "the store at 127.0.0.1:$postgres does not answer" \
  FLEET_TEST_S3_ENDPOINT="http://127.0.0.1:$postgres"

expect pass "a PostgreSQL that answers the SSLRequest" postgres "" \
  FLEET_TEST_POSTGRES_HOST=127.0.0.1 FLEET_TEST_POSTGRES_PORT="$postgres"
expect pass "an S3 endpoint that answers with a status line" s3 "" \
  FLEET_TEST_S3_ENDPOINT="http://127.0.0.1:$s3/"
expect pass "a PostgreSQL and an S3 endpoint that answer" postgres "" \
  FLEET_TEST_POSTGRES_HOST=127.0.0.1 FLEET_TEST_POSTGRES_PORT="$postgres" \
  FLEET_TEST_S3_ENDPOINT="http://127.0.0.1:$s3"

summary "Store check self-test"
