#!/usr/bin/env bash
# Build and install the `music` binary (same pattern as nile).
set -euo pipefail
cd "$(dirname "$0")/.."

cargo install --locked --force --path .
