#!/usr/bin/env bash
# Packs the GLPI Agent's official Linux AppImage into a fleet tree package (ADR-0064).
#
# The AppImage is upstream's only self-contained Linux build, but it is a single file, not an
# archive — and as published it needs libfuse2 on every host, or a re-extraction on every start.
# Extracting it once here removes both: the result is a relocatable tree the Client unpacks as a
# tree package (ADR-0023), with `program_path = "AppRun"` and `--script=glpi-agent` selecting the
# agent.
#
# What this script does, and why each step exists:
#   1. Downloads the release's AppImage and its `.sha256`, and verifies the one against the other —
#      the artifact is about to be transformed, so upstream's hash must be checked *here*; after
#      this, the fleet's own hash and signature are the chain of trust.
#   2. Extracts it (`--appimage-extract` runs without FUSE).
#   3. Deletes dangling symlinks (Debian packaging leftovers) and dereferences the rest at pack
#      time (`tar -h`): a tree package refuses links outright.
#   4. Packs deterministically — fixed order, zeroed times and ownership, unstamped gzip — so the
#      same release repacks to the same hash and never a rollout nobody asked for.
#
# Usage:  scripts/pack-glpi-agent.sh <version> [<output-directory>]
# Example: scripts/pack-glpi-agent.sh 1.19
#
# Prints the artifact's SHA-256 (hex) to stdout — everything else goes to stderr — so it composes:
#   sha=$(scripts/pack-glpi-agent.sh 1.19)
# Runs on Linux x86_64 (the Dev Container qualifies): extraction executes the AppImage's runtime.

set -euo pipefail

say() { echo "$@" >&2; }
die() { say "error: $*"; exit 1; }

[ $# -ge 1 ] || die "usage: $0 <version> [<output-directory>]"
version="$1"
outdir="${2:-.}"
[ -d "$outdir" ] || die "$outdir is not a directory"
[ "$(uname -sm)" = "Linux x86_64" ] || die "the AppImage extracts by running; this needs Linux x86_64"
for tool in curl sha256sum tar gzip; do
    command -v "$tool" >/dev/null || die "$tool is not installed"
done

appimage="glpi-agent-${version}-x86_64.AppImage"
base="https://github.com/glpi-project/glpi-agent/releases/download/${version}"
# The unpacked tree must fit the Client's tree rules (ADR-0023) — checked below, not assumed.
max_members=10000

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

say "downloading ${appimage} and the release checksums …"
curl -fsSL -o "${workdir}/${appimage}" "${base}/${appimage}"
curl -fsSL -o "${workdir}/checksums" "${base}/glpi-agent-${version}.sha256"

say "verifying upstream's SHA-256 …"
grep " ${appimage}\$" "${workdir}/checksums" >"${workdir}/expected" \
    || die "the release's .sha256 has no line for ${appimage}"
(cd "$workdir" && sha256sum -c expected >/dev/null) \
    || die "${appimage} does not match the hash upstream published"

say "extracting the AppImage …"
chmod +x "${workdir}/${appimage}"
(cd "$workdir" && "./${appimage}" --appimage-extract >/dev/null) \
    || die "--appimage-extract failed"
tree="${workdir}/glpi-agent-${version}"
mv "${workdir}/squashfs-root" "$tree"

# Dangling symlinks would fail the dereference; the desktop icon link serves nothing here.
find "$tree" -xtype l -delete
rm -f "$tree/.DirIcon"
[ -x "$tree/AppRun" ] || die "the extracted tree has no executable AppRun"

# `tar -h` dereferences the remaining links at pack time; the count is checked against the
# Client's member cap so an upstream layout change fails here, not on three hundred hosts.
members=$(find "$tree" | wc -l)
[ "$members" -le "$max_members" ] || die "the tree holds ${members} members — past the Client's ${max_members} cap"

artifact="${outdir}/glpi-agent_${version}_linux_amd64.tar.gz"
say "packing ${members} members deterministically …"
tar -C "$workdir" -h --hard-dereference \
    --sort=name --owner=0 --group=0 --numeric-owner --mtime='@0' \
    -cf - "glpi-agent-${version}" | gzip -n >"$artifact"

say "wrote ${artifact} — a Supervisor with program_path = \"AppRun\" installs it"
say "sha256 (hex):"
sha256sum "$artifact" | cut -d' ' -f1
