#!/usr/bin/env bash
# Check the BundleIntegrity model under every config in expected.tsv, then its Rust regressions.
set -euo pipefail
skill=$(cd "$(dirname "$0")" && pwd)
. "$(git -C "$skill" rev-parse --show-toplevel)/verification/lib.sh"
verify_evidence bundles
verify_model "$skill" BundleIntegrity.tla
verify_rust_list "$skill/rust-tests.txt"
