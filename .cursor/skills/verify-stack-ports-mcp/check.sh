#!/usr/bin/env bash
# Check the PortAndMcpContract model under every config in expected.tsv, then the Rust tests that
# run the real port registry, MCP admission, log cursors and CLI parsing behind it.
set -euo pipefail
skill=$(cd "$(dirname "$0")" && pwd)
. "$(git -C "$skill" rev-parse --show-toplevel)/verification/lib.sh"
verify_evidence ports-mcp
verify_model "$skill" PortAndMcpContract.tla
verify_rust_list "$skill/rust-tests.txt"
