#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# regenerate-measurements.sh
# --------------------------
# Discover and run all measurement harnesses that produce local estimates.
# Harnesses are identified by a special marker comment "// @measure <mode>"
# where <mode> is "local" (run automatically) or "testnet" (requires a funded
# testnet identity and the `stellar` CLI). The script runs the "local" harnesses
# and prints a summary of skipped "testnet" harnesses.
#
# Usage:
#   ./scripts/regenerate-measurements.sh [--out DIR]
#
#   --out DIR   Directory to write captured output (default: ./measurements-out)
#
# The script is intentionally simple and does not attempt to parse the output –
# it merely captures the raw `cargo test` output for each harness. Users can
# diff the generated files against the existing MEASUREMENTS.md.

set -euo pipefail

OUT_DIR="${1:-measurements-out}"
mkdir -p "$OUT_DIR"

# Helper to run a cargo test harness and capture its output.
run_harness() {
    local crate="$1"
    local test_target="$2"
    local mode="$3"
    local out_file="$OUT_DIR/${crate}__${test_target}.log"
    echo "Running $crate $test_target ($mode)..."
    if [[ "$mode" == "local" ]]; then
        # Build the WASM for the contract (required by many harnesses).
        # We build once per crate; ignore errors if already built.
        cargo build --target wasm32v1-none --release -p "$crate" >/dev/null 2>&1 || true
        # Run the harness with --nocapture to see its eprintln output.
        cargo test -p "$crate" --test "$test_target" -- --nocapture | tee "$out_file"
    else
        echo "SKIPPED (testnet required) – $crate $test_target" | tee "$out_file"
    fi
}

# Discover harnesses: look for files under */tests/*.rs containing the marker.
# The marker format: // @measure <mode>[:<test_name>]
# If <test_name> is omitted, the whole test binary is run.

# Find all Rust test source files.
mapfile -t test_files < <(git ls-files "*/tests/*.rs")

for file in "${test_files[@]}"; do
    # Extract crate name from the path (first component before '/').
    crate=$(echo "$file" | cut -d'/' -f1)
    # Determine the test target name (file stem without .rs).
    test_target=$(basename "$file" .rs)
    # Read the marker line.
    marker=$(grep -m1 "// @measure" "$file" || true)
    if [[ -z "$marker" ]]; then
        # No marker – skip this file (new harnesses must add a marker).
        continue
    fi
    # Parse mode and optional test name.
    # Expected formats: "// @measure local" or "// @measure testnet" or with ":test_name"
    mode_part=$(echo "$marker" | awk -F'@measure' '{print $2}' | xargs)
    mode=$(echo "$mode_part" | cut -d':' -f1 | xargs)
    test_name=$(echo "$mode_part" | cut -d':' -f2 | xargs)
    if [[ -z "$mode" ]]; then
        mode="local"
    fi
    # If a specific test name is provided, we run only that test.
    if [[ -n "$test_name" ]]; then
        run_harness "$crate" "$test_target" "$mode" "--test" "$test_name"
    else
        run_harness "$crate" "$test_target" "$mode"
    fi
done

# Summary
echo "\n=== Regeneration complete ==="
echo "Outputs written to $OUT_DIR/"
