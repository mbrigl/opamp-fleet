# shellcheck shell=bash
# A SoftHSM2 token directory for the tests of the keys on a PKCS#11 token
# (ADR-0043
# clause 7). Sourced by scripts/check-rust.sh and scripts/check-store.sh.

# The text a test that needs a token prints when none is named.
SOFTHSM_SKIPPED="skipped: no PKCS#11 token"

# softhsm_env — names the module and a fresh configuration to the tests: writes a softhsm2.conf
# whose token directory lies in a new temporary directory, outside the repository, and exports
# SOFTHSM2_CONF and FLEET_TEST_PKCS11_MODULE. Each test initialises a token of its own there.
# Sets SOFTHSM_DIR for the caller to remove. Where FLEET_TEST_PKCS11_MODULE is named already it
# changes nothing. Without SoftHSM2 it says so and names nothing, except in a CI run (CI=true),
# where it returns 1.
softhsm_env() {
  SOFTHSM_DIR=""
  [[ -n "${FLEET_TEST_PKCS11_MODULE:-}" ]] && return 0
  local module="" candidate
  for candidate in /usr/lib/softhsm/libsofthsm2.so /usr/lib/x86_64-linux-gnu/softhsm/libsofthsm2.so \
    /usr/lib/aarch64-linux-gnu/softhsm/libsofthsm2.so; do
    if [[ -f "$candidate" ]]; then
      module="$candidate"
      break
    fi
  done
  if [[ -z "$module" ]]; then
    if [[ "${CI:-}" == "true" ]]; then
      echo "ERROR: SoftHSM2 is not installed in a CI run: apt-get install softhsm2" >&2
      return 1
    fi
    echo "SoftHSM2 is not installed: the tests of the keys on a token skip (apt-get install softhsm2)."
    return 0
  fi
  SOFTHSM_DIR="$(mktemp -d)"
  mkdir "$SOFTHSM_DIR/tokens"
  printf 'directories.tokendir = %s\nobjectstore.backend = file\nlog.level = ERROR\n' \
    "$SOFTHSM_DIR/tokens" > "$SOFTHSM_DIR/softhsm2.conf"
  export SOFTHSM2_CONF="$SOFTHSM_DIR/softhsm2.conf"
  export FLEET_TEST_PKCS11_MODULE="$module"
}

# softhsm_skipped <log> — returns 0, saying so, when a module is named and a test in <log> still
# skipped for want of a token.
softhsm_skipped() {
  if [[ -n "${FLEET_TEST_PKCS11_MODULE:-}" ]] && grep -q "$SOFTHSM_SKIPPED" "$1"; then
    echo "a test of the keys on a token was skipped although FLEET_TEST_PKCS11_MODULE names a module" >&2
    return 0
  fi
  return 1
}
