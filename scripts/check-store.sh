#!/usr/bin/env bash
#
# Runs the tests of one of the Server's external stores against a real one: 's3' against an
# S3-compatible object store (ADR-0029), 'postgres' against PostgreSQL over TLS (ADR-0029). A
# plain `cargo test` skips them; the 's3-store' and 'postgres-store' jobs of
# .github/workflows/checks.yml start the store and run this. Locally, the development stack of
# observability/compose.yaml provides both; without a store named the check says so and skips,
# except in a CI run (CI=true), where it fails.
#
# The store is named through the environment the tests read: FLEET_TEST_S3_ENDPOINT,
# FLEET_TEST_S3_BUCKET, FLEET_TEST_S3_ACCESS_KEY_ID, and FLEET_TEST_S3_SECRET_ACCESS_KEY_FILE, or
# FLEET_TEST_POSTGRES_HOST, _PORT, _DBNAME, _USER, _PASSWORD_FILE, and _CA_FILE; 'postgres' needs a
# bucket named too, for a Server on both stores. Skipped is not passed: once a store is named, a test
# that reports it skipped fails the run.
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
if [[ -z "$named" ]]; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: no $1 store is named in the environment of a CI run." >&2
    exit 1
  fi
  echo "Skipped: no $1 store is named in the environment."
  exit 0
fi
command -v cargo >/dev/null || { echo "cargo is required (rust-toolchain.toml names the toolchain)" >&2; exit 1; }

log="$(mktemp)"
trap 'rm -f "$log"' EXIT
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
fi
if grep -q "$skipped" "$log"; then
  echo "a $1 test was skipped although a store is named" >&2
  exit 1
fi
