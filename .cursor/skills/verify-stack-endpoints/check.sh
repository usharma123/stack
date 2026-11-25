#!/usr/bin/env bash
# Check the EndpointWithholding model under every config in expected.tsv, then its Rust regressions.
set -euo pipefail
skill=$(cd "$(dirname "$0")" && pwd)
. "$(git -C "$skill" rev-parse --show-toplevel)/verification/lib.sh"
verify_evidence endpoints
verify_model "$skill" EndpointWithholding.tla
verify_rust_list "$skill/rust-tests.txt"
