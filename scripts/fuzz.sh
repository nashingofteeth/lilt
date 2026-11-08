#!/usr/bin/env bash
# Run the property/fuzz tests with a larger case count.
set -euo pipefail
cd "$(dirname "$0")/.."

FUZZ_CASES="${FUZZ_CASES:-10000}" cargo test fuzz -- --nocapture
