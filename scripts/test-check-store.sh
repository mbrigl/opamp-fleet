#!/usr/bin/env bash
#
# Self-test of scripts/check-store.sh, its probe of the named store in particular.
#
# The check makes its probe in an inner bash run under `timeout`, the probe's one external command.
# A stub `timeout` first on PATH stands in for it: it records the address and the request it was
# handed, and answers as the case's store would. A store that accepts and closes at once, as a
# forward to a store that is down does, answers nothing. A port nothing listens on fails the
# connect. A store that answers gets the first bytes of its answer back, but only for the request
# expected of its protocol, so a wrong SSLRequest or HTTP request gets no answer. A stub `cargo`
# stands in for the store's tests and prints a marker, so a case sees whether the check went on to
# them. The cases assert the exit code, the message, the marker, and the probes made. The
# self-test does not exercise /dev/tcp itself; nothing reaches the network.
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

# The requests the probe must send, as the printf formats the check hands the inner bash, each
# followed by the count of bytes it reads back.
PG_REQUEST='\000\000\000\010\004\322\026\057 1'
s3_request() { printf '%s' "HEAD / HTTP/1.0\r\nHost: 127.0.0.1:$1\r\n\r\n 5"; }

stubs="$work/bin"
stores="$work/stores"
mkdir -p "$stubs"
printf '#!/bin/sh\necho "cargo stub ran"\n' > "$stubs/cargo"
# Handed: 10 bash -c <probe> _ <host> <port> <request> <count>.
cat > "$stubs/timeout" <<'STUB'
#!/bin/sh
printf '%s:%s %s %s\n' "$6" "$7" "$8" "$9" >> "$STORES/calls"
[ -f "$STORES/$7.request" ] || exit 1
[ "$8 $9" = "$(cat "$STORES/$7.request")" ] || exit 0
head -c "$9" "$STORES/$7.answer"
STUB
chmod +x "$stubs/cargo" "$stubs/timeout"

# fresh — no store listens.
fresh() {
  rm -rf "$stores"
  mkdir -p "$stores"
}

# serve <port> <request> <answer> — a store on the port that answers the request with the answer;
# an empty answer accepts and closes.
serve() {
  printf '%s' "$2" > "$stores/$1.request"
  printf '%s' "$3" > "$stores/$1.answer"
}

# expect <pass|fail> <name> <store> <needle> <probes> [VAR=value...] — run the check on the store
# in an environment that names only the variables given. A failing case must name the needle and
# must not reach the tests, a passing one must reach them; both must have made the probes, one
# line '<host>:<port> <request> <count>' each.
expect() {
  local want="$1" name="$2" store="$3" needle="$4" probes="$5" out rc made ok=0
  shift 5
  out="$(env -i PATH="$stubs:$PATH" HOME="$work" STORES="$stores" FLEET_TEST_PKCS11_MODULE=stub \
    "$@" bash "$CHECK" "$store" 2>&1)"; rc=$?
  made="$(cat "$stores/calls" 2>/dev/null)"
  if [[ "$want" == pass ]]; then
    ((rc == 0)) && [[ "$out" == *"cargo stub ran"* ]] && ok=1
  else
    ((rc == 1)) && [[ "$out" == *"$needle"* && "$out" != *"cargo stub ran"* ]] && ok=1
  fi
  [[ "$made" == "$probes" ]] || ok=0
  check "$name" test "$ok" -eq 1
  ((ok)) || while IFS= read -r l; do printf '    %s\n' "$l"; done <<< "$out"$'\nprobes made:\n'"$made" >&2
}

PG=(FLEET_TEST_POSTGRES_HOST=127.0.0.1 FLEET_TEST_POSTGRES_PORT=5432)
# PostgreSQL answers an SSLRequest with one byte: 'S' to go on in TLS, 'N' to refuse it.
PG_ANSWER=S
S3_ANSWER=$'HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n'

fresh
serve 5432 "$PG_REQUEST" ''
expect fail "a PostgreSQL forward that accepts and closes" postgres \
  "the store at 127.0.0.1:5432 does not answer" "127.0.0.1:5432 $PG_REQUEST" "${PG[@]}"

fresh
expect fail "a PostgreSQL port nothing listens on" postgres \
  "the store at 127.0.0.1:5432 does not answer" "127.0.0.1:5432 $PG_REQUEST" "${PG[@]}"

fresh
serve 9000 "$(s3_request 9000)" ''
expect fail "an S3 forward that accepts and closes" s3 \
  "the store at 127.0.0.1:9000 does not answer" "127.0.0.1:9000 $(s3_request 9000)" \
  FLEET_TEST_S3_ENDPOINT=http://127.0.0.1:9000

fresh
serve 5432 "$PG_REQUEST" "$PG_ANSWER"
serve 9000 "$(s3_request 9000)" ''
expect fail "a PostgreSQL that answers, beside an S3 forward that accepts and closes" postgres \
  "the store at 127.0.0.1:9000 does not answer" \
  "127.0.0.1:5432 $PG_REQUEST"$'\n'"127.0.0.1:9000 $(s3_request 9000)" \
  "${PG[@]}" FLEET_TEST_S3_ENDPOINT=http://127.0.0.1:9000

fresh
serve 9000 "$(s3_request 9000)" "$PG_ANSWER"
expect fail "an S3 endpoint that answers with no status line" s3 \
  "the store at 127.0.0.1:9000 does not answer" "127.0.0.1:9000 $(s3_request 9000)" \
  FLEET_TEST_S3_ENDPOINT=http://127.0.0.1:9000

fresh
serve 5432 "$PG_REQUEST" "$PG_ANSWER"
expect pass "a PostgreSQL that answers the SSLRequest" postgres "" \
  "127.0.0.1:5432 $PG_REQUEST" "${PG[@]}"

fresh
serve 9000 "$(s3_request 9000)" "$S3_ANSWER"
expect pass "an S3 endpoint that answers with a status line" s3 "" \
  "127.0.0.1:9000 $(s3_request 9000)" FLEET_TEST_S3_ENDPOINT=http://127.0.0.1:9000/

fresh
serve 5432 "$PG_REQUEST" "$PG_ANSWER"
serve 9000 "$(s3_request 9000)" "$S3_ANSWER"
expect pass "a PostgreSQL and an S3 endpoint that answer" postgres "" \
  "127.0.0.1:5432 $PG_REQUEST"$'\n'"127.0.0.1:9000 $(s3_request 9000)" \
  "${PG[@]}" FLEET_TEST_S3_ENDPOINT=http://127.0.0.1:9000

fresh
expect pass "an https endpoint is not probed" s3 "" "" FLEET_TEST_S3_ENDPOINT=https://s3.example.invalid

summary "Store check self-test"
