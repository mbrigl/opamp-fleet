#!/bin/bash


echo "Starting installation of the OpenTelemetry Collectors..."


echo "Downloading OpenTelemetry Collector Contrib..."
echo "Extracting otelcol-contrib to /usr/local/bin..."

echo "Downloading OpenTelemetry Collector Core..."
echo "Extracting otelcol to /usr/local/bin..."

# 3. Stage the minimal example Configurations (ADR-0025) for the two Collectors into
# fleet-configs/, so the Server offers them from its next start (see the seed script for
# the Selector mapping and usage notes).
echo "Staging example test Configurations..."
"$(dirname "$0")/seed_test_configs.sh" --offline

