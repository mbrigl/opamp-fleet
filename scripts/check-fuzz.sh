#!/usr/bin/env bash
#
# Fuzzes one parser that reads bytes from the network or from a downloaded artifact
# (specification Q-2), for a given number of seconds, from its corpus and its seeds. The 'fuzz'
# job of .github/workflows/fuzz.yml runs it for every target of fuzz/Cargo.toml, briefly on a
# change and long on its schedule. A crash leaves its input under fuzz/artifacts/, where the job
# keeps it. scripts/check-all.sh names it on an '# On instruction:' line: it needs a nightly
# toolchain and runs for minutes.
#
# Needs a nightly toolchain and cargo-fuzz (cargo install cargo-fuzz). The target triple is
# stated, since a prebuilt cargo-fuzz built for musl would otherwise default to its own target,
# which the sanitizer refuses.
#
# Usage:
#     scripts/check-fuzz.sh <target> <seconds>
# Exit code 0 when the run ends without a finding, 2 on a wrong argument.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

target="${1:-}" seconds="${2:-}"
if [[ -z "$target" || ! "$seconds" =~ ^[0-9]+$ ]]; then
  echo "usage: scripts/check-fuzz.sh <target> <seconds>" >&2
  exit 2
fi
command -v cargo-fuzz >/dev/null || { echo "cargo-fuzz is required: cargo install cargo-fuzz" >&2; exit 1; }

mkdir -p "fuzz/corpus/$target"
cargo +nightly fuzz run --target x86_64-unknown-linux-gnu "$target" \
  "fuzz/corpus/$target" "fuzz/seeds/$target" \
  -- -max_total_time="$seconds" -rss_limit_mb=2048
