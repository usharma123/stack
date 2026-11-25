#!/usr/bin/env bash
# Check the Generation model under every config in expected.tsv, then its Rust regression.
set -euo pipefail
skill=$(cd "$(dirname "$0")" && pwd)
. "$(git -C "$skill" rev-parse --show-toplevel)/verification/lib.sh"
verify_evidence pgtemplate
verify_model "$skill" Generation.tla
verify_rust_list "$skill/rust-tests.txt"
