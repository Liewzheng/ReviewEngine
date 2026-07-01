#!/usr/bin/env bash
# Helper script to list third-party Rust crate licenses.
# Run this before each release and update THIRD_PARTY_LICENSES.md with any
# changes to dependency licenses.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

echo "=========================================="
echo "review-engine third-party dependency licenses"
echo "=========================================="
echo ""
echo "Direct dependencies (depth 1):"
echo ""

cargo tree --prefix none --edges normal --depth 1 --format "{p} {l}" | sort | uniq

echo ""
echo "All transitive dependencies:"
echo ""

cargo tree --prefix none --format "{p} {l}" | sort | uniq

echo ""
echo "=========================================="
echo "REMINDER: Review the output above and update"
echo "  ${ROOT_DIR}/THIRD_PARTY_LICENSES.md"
echo "before publishing a release binary."
echo "=========================================="
