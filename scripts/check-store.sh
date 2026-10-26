#!/usr/bin/env bash
#
# Runs the tests of one of the Server's external stores against a real one: 's3' against an
# S3-compatible object store (ADR-0029), 'postgres' against PostgreSQL over TLS (ADR-0029). A
# plain `cargo test` skips them; the 's3-store' and 'postgres-store' jobs of
# .github/workflows/ci.yml start the store and run this. Locally, the development stack of
# observability/compose.yaml provides both, so scripts/check-all.sh names this script on an
# '# On instruction:' line.
#
# The store is named through the environment the tests read: FLEET_TEST_S3_ENDPOINT,
# FLEET_TEST_S3_BUCKET, FLEET_TEST_S3_ACCESS_KEY_ID, and FLEET_TEST_S3_SECRET_ACCESS_KEY_FILE, or
# FLEET_TEST_POSTGRES_HOST, _PORT, _DBNAME, _USER, _PASSWORD_FILE, and _CA_FILE. Skipped is not
# passed: once a store is named, a test that reports it skipped fails the run.
#
# Usage:
#     scripts/check-store.sh s3|postgres
# Exit code 0 when every test of the store passed against it, 1 otherwise, 2 on a wrong argument.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

case "${1:-}" in
  s3) filter="s3::" skipped="skipped: no S3 store" named="${FLEET_TEST_S3_ENDPOINT:-}" ;;
  postgres) filter="postgres::" skipped="skipped: no PostgreSQL" named="${FLEET_TEST_POSTGRES_HOST:-}" ;;
  *) echo "usage: scripts/check-store.sh s3|postgres" >&2; exit 2 ;;
esac
[[ -n "$named" ]] || { echo "check-store: no $1 store is named in the environment" >&2; exit 1; }
command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

log="$(mktemp)"
trap 'rm -f "$log"' EXIT
cargo test -p fleet-server --lib --locked "$filter" -- --nocapture 2>&1 | tee "$log"
if grep -q "$skipped" "$log"; then
  echo "a $1 test was skipped although a store is named" >&2
  exit 1
fi
