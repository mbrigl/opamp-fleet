#!/usr/bin/env bash
# Dev Container tooling, installed once by `onCreateCommand` (devcontainer.json), see ADR-0004.
#
#   - protoc          : prost-build generates the OpAMP types from the vendored schema at build time.
#   - otelcol-contrib : the OpenTelemetry Collector a Collector Supervisor owns while it is developed
#                       inside the dev container. Pinned to the same release as the `opamp-agent`
#                       sidecar so both run the same collector.
#
# The Rust toolchain itself comes from the devcontainer Rust Feature; the channel is pinned by
# rust-toolchain.toml so the container and CI resolve the same compiler.
set -euo pipefail

# Keep in sync with the `opamp-agent` build arg in .devcontainer/docker-compose.yml.
OTEL_VERSION="0.156.0"

sudo apt-get update
sudo apt-get install -y protobuf-compiler

if command -v otelcol-contrib >/dev/null 2>&1; then
  echo "otelcol-contrib already installed: $(otelcol-contrib --version 2>/dev/null || echo present)"
  exit 0
fi

# The release archives are named by Go arch (amd64 | arm64), which matches `dpkg --print-architecture`.
arch="$(dpkg --print-architecture)"
url="https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v${OTEL_VERSION}/otelcol-contrib_${OTEL_VERSION}_linux_${arch}.tar.gz"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "downloading otelcol-contrib ${OTEL_VERSION} (${arch}) ..."
curl -fsSL "$url" -o "$tmp/otelcol-contrib.tar.gz"
tar -xzf "$tmp/otelcol-contrib.tar.gz" -C "$tmp" otelcol-contrib
sudo install -m 0755 "$tmp/otelcol-contrib" /usr/local/bin/otelcol-contrib
otelcol-contrib --version
