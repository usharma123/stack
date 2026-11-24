#!/usr/bin/env bash
# Check the SnapshotAtomicity model under every config in expected.tsv, then its Rust regressions.
set -euo pipefail
skill=$(cd "$(dirname "$0")" && pwd)
. "$(git -C "$skill" rev-parse --show-toplevel)/verification/lib.sh"
verify_evidence snapshot
verify_model "$skill" SnapshotAtomicity.tla
tests=()
while IFS= read -r test; do
  case "$test" in ''|'#'*) ;; *) tests+=("$test") ;; esac
done <"$skill/rust-tests.txt"
verify_rust "${tests[@]}"
