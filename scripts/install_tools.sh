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


echo "Installation completed successfully!"