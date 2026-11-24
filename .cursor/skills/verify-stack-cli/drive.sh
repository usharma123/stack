#!/usr/bin/env bash
set -euo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$root"
case "${1:-}" in
  1-independent-checkouts|2-wrong-instance|3-leases|4-mcp) scenario=$1 ;;
  *) echo "Choose 1-independent-checkouts, 2-wrong-instance, 3-leases, or 4-mcp." >&2; exit 2 ;;
esac
mkdir -p target/verification
evidence=$(mktemp -d "$root/target/verification/cli.XXXXXX")
printf 'Evidence: %s\n' "$evidence"
{
  git rev-parse HEAD
  git status --porcelain
} >"$evidence/source.log"
target_dir="$root/target/verification/build"
cargo build --locked --bin stack --target-dir "$target_dir"
binary="$target_dir/debug/stack"
{
  "$binary" --version
  command -v mise
  command -v jq
  printf 'Scenario: %s\n' "$scenario"
} | tee "$evidence/doctor.log"
scenarios=()
for prerequisite in 1-independent-checkouts 2-wrong-instance 3-leases 4-mcp; do
  scenarios+=("$prerequisite")
  [ "$prerequisite" != "$scenario" ] || break
done
STACK_E2E_RESULTS="$evidence/results.jsonl" STACK_E2E_EVIDENCE="$evidence" \
  bash tests/e2e/native.sh "$binary" "${scenarios[@]}" 2>&1 | tee "$evidence/transcript.log"
jq -e --arg name "$scenario" 'select(.scenario == $name and .result == "passed")' "$evidence/results.jsonl"
