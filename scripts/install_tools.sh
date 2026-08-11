#!/bin/bash

# Fail on any error, an unset variable, or a failure anywhere in a pipeline.
set -euo pipefail

# Pinned versions, and the SHA-256 of every artifact this script downloads. Each download is
# verified against its pinned digest before it is extracted or run — this installs to /usr/local/bin
# and runs an installer, both as root via sudo, so a compromised release asset, a poisoned mirror, or
# a moved branch must not be able to slip an attacker's bytes onto the host.
#
# To bump: change the version, download the artifact, and update the matching digest together
# (`sha256sum <file>`). Never widen this by skipping a check.
OTELCOL_VERSION="0.157.0"
OTELCOL_CONTRIB_SHA256="d33177515a244a2393f03ffd66ab3e68a8fc11a56bc145ec4d0ca2644ee95504"
OTELCOL_CORE_SHA256="2937cf24892af55b143c072fddece17862239cf78280620029276493eb81beae"
# Fluent Bit publishes no checksum for its installer and documents fetching it from `master`. Pin an
# immutable commit instead of a moving branch, and verify the script's own digest before running it.
FLUENTBIT_INSTALL_COMMIT="d17e94305df9847599f474dc4c31318a026581b7"
FLUENTBIT_INSTALL_SHA256="ccd7b1c956253d29ba8e8cea1ef0321c2bd1abd820d0b7006cd0aa717dc134c6"

# Download over verified TLS only, never following a redirect into plaintext.
download() {
  curl --proto '=https' --tlsv1.2 -fsSL "$1" -o "$2"
}

# Fail the script (set -e) unless $1 has the expected SHA-256 $2.
verify_sha256() {
  echo "${2}  ${1}" | sha256sum -c - >/dev/null
}

echo "Starting installation of OpenTelemetry Collector and Fluent Bit..."

otelcol_base="https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v${OTELCOL_VERSION}"

# 1. OpenTelemetry Collector (Contrib): download, verify, then extract.
echo "Downloading OpenTelemetry Collector Contrib..."
contrib_tgz="/tmp/otelcol-contrib_${OTELCOL_VERSION}_linux_amd64.tar.gz"
download "${otelcol_base}/otelcol-contrib_${OTELCOL_VERSION}_linux_amd64.tar.gz" "$contrib_tgz"
verify_sha256 "$contrib_tgz" "$OTELCOL_CONTRIB_SHA256"
echo "Extracting otelcol-contrib to /usr/local/bin..."
sudo tar -xvf "$contrib_tgz" -C /usr/local/bin otelcol-contrib

# 2. OpenTelemetry Collector (Core): download, verify, then extract.
echo "Downloading OpenTelemetry Collector Core..."
core_tgz="/tmp/otelcol_${OTELCOL_VERSION}_linux_amd64.tar.gz"
download "${otelcol_base}/otelcol_${OTELCOL_VERSION}_linux_amd64.tar.gz" "$core_tgz"
verify_sha256 "$core_tgz" "$OTELCOL_CORE_SHA256"
echo "Extracting otelcol to /usr/local/bin..."
sudo tar -xvf "$core_tgz" -C /usr/local/bin otelcol

# 3. Fluent Bit: fetch the pinned installer, verify it, then run it — never `curl | sh`.
echo "Installing Fluent Bit..."
fbit_installer="/tmp/fluent-bit-install.sh"
download "https://raw.githubusercontent.com/fluent/fluent-bit/${FLUENTBIT_INSTALL_COMMIT}/install.sh" "$fbit_installer"
verify_sha256 "$fbit_installer" "$FLUENTBIT_INSTALL_SHA256"
sh "$fbit_installer"

# 4. Stage the minimal example Configurations (ADR-0012) for the three processes into
# fleet-configs/, so the Server offers them from its next start (see the seed script for
# the Selector mapping and usage notes).
echo "Staging example test Configurations..."
"$(dirname "$0")/seed_test_configs.sh" --offline

echo "Installation completed successfully!"
