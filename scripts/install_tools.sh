#!/bin/bash

# Exit immediately if a command exits with a non-zero status
set -e

echo "Starting installation of OpenTelemetry Collector and Fluent Bit..."

# 1. Download and extract OpenTelemetry Collector (Contrib)
echo "Downloading OpenTelemetry Collector Contrib..."
curl --proto '=https' --tlsv1.2 -fL https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v0.157.0/otelcol-contrib_0.157.0_linux_amd64.tar.gz -o /tmp/otelcol-contrib_0.157.0_linux_amd64.tar.gz

echo "Extracting otelcol-contrib to /usr/local/bin..."
sudo tar -xvf /tmp/otelcol-contrib_0.157.0_linux_amd64.tar.gz -C /usr/local/bin otelcol-contrib

# 2. Download and extract OpenTelemetry Collector (Core)
echo "Downloading OpenTelemetry Collector Core..."
curl --proto '=https' --tlsv1.2 -fL https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v0.157.0/otelcol_0.157.0_linux_amd64.tar.gz -o /tmp/otelcol_0.157.0_linux_amd64.tar.gz

echo "Extracting otelcol to /usr/local/bin..."
sudo tar -xvf /tmp/otelcol_0.157.0_linux_amd64.tar.gz -C /usr/local/bin otelcol

# 3. Install Fluent Bit
echo "Installing Fluent Bit..."
curl https://raw.githubusercontent.com/fluent/fluent-bit/master/install.sh | sh

# 4. Stage the minimal example Configurations (ADR-0012) for the three processes into
# fleet-configs/, so the Server offers them from its next start (see the seed script for
# the Selector mapping and usage notes).
echo "Staging example test Configurations..."
"$(dirname "$0")/seed_test_configs.sh" --offline

echo "Installation completed successfully!"