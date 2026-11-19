#!/usr/bin/env bash
#
# Fuzzes one parser that reads bytes from the network or from a downloaded artifact
# (specification Q-2), for a given number of seconds, from its corpus and its seeds. The 'fuzz'
# job of .github/workflows/fuzz.yml runs it for every target of fuzz/Cargo.toml, briefly on a
# change and long on its schedule. A crash leaves its input under fuzz/artifacts/, where the job
# keeps it. scripts/check-all.sh runs it without arguments: every target for ten seconds. Without
# cargo-fuzz the check says so and skips, except in a CI run (CI=true), where it fails.
#
# Needs a nightly toolchain and cargo-fuzz (cargo install cargo-fuzz). The target triple is
# stated, since a prebuilt cargo-fuzz built for musl would otherwise default to its own target,
# which the sanitizer refuses.
#
# Usage:
#     scripts/check-fuzz.sh [--root <dir>] [<target> <seconds>]
# Exit code 0 when the run ends without a finding, 2 on a wrong argument or a root that is no
# directory.

set -euo pipefail

# shellcheck source-path=SCRIPTDIR source=lib/root.sh
source "$(dirname "${BASH_SOURCE[0]}")/lib/root.sh"
usage="scripts/check-fuzz.sh [--root <dir>] [<target> <seconds>]"
enter_root "${BASH_SOURCE[0]}" "$usage" "$@"
set -- "${ARGS[@]}"
rest_args "${BASH_SOURCE[0]}" "$usage" 2 "${ARGS[@]}"

if (($# == 0)); then
  # Every [[bin]] of fuzz/Cargo.toml is a target.
  mapfile -t targets < <(sed -n '/^\[\[bin\]\]/,/^name/s/^name = "\(.*\)"/\1/p' fuzz/Cargo.toml)
  seconds=10
elif (($# == 2)) && [[ -n "$1" && "$2" =~ ^[0-9]+$ ]]; then
  targets=("$1") seconds="$2"
else
  echo "check-fuzz.sh: name a target and its seconds, or neither — usage: $usage" >&2
  exit 2
fi
if ! command -v cargo-fuzz >/dev/null; then
  if [[ "${CI:-}" == "true" ]]; then
    echo "ERROR: cargo-fuzz not found in a CI run: cargo install cargo-fuzz" >&2
    exit 1
  fi
  echo "Skipped: cargo-fuzz is not installed (cargo install cargo-fuzz)."
  exit 0
fi

for target in "${targets[@]}"; do
  mkdir -p "fuzz/corpus/$target"
  cargo +nightly fuzz run --target x86_64-unknown-linux-gnu "$target" \
    "fuzz/corpus/$target" "fuzz/seeds/$target" \
    -- -max_total_time="$seconds" -rss_limit_mb=2048
done
